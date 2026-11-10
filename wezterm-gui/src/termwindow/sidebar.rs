//! Runtime state and event handling for the session sidebar.
//!
//! The sidebar is ordinary window chrome rather than a `Modal`: a modal would
//! take the keyboard for as long as it was open, and the point of the sidebar
//! is that you can use the terminal while it is showing.
//!
//! Drawing lives in `termwindow::render::sidebar`; the tree itself lives in
//! `crate::sessions`.

use crate::sessions::ssh::{ssh_domain_name, with_ssh_hosts};
use crate::sessions::store::with_store;
use crate::sessions::{
    clamp_scroll, clamp_width, drop_placement, forms, rail_click, NodeId, NodeKind, Placement,
    SessionNode, SessionSpawn, SidebarPage,
};
use crate::spawn::SpawnWhere;
use crate::termwindow::form::FormModal;
use crate::termwindow::{UIItem, UIItemType};
use ::window::{MouseEvent, MouseEventKind as WMEK, MousePress, WindowOps};
use config::keyassignment::{KeyAssignment, SpawnCommand, SpawnTabDomain};
use config::{DimensionContext, TermConfig};
use mux::Mux;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use wezterm_term::{KeyCode, KeyModifiers};

/// Narrowest useful sidebar, in cells of the title font.
pub const MIN_SIDEBAR_CELLS: f32 = 8.;

/// Width of the page tab rail down the sidebar's left edge, in terminal
/// cells.
///
/// It stays when the panel is collapsed: collapsing to nothing would leave no
/// way to bring the sidebar back short of the command palette.
pub const COLLAPSED_RAIL_CELLS: f32 = 2.;

/// Row height as a multiple of the title font's cell height.
pub const SIDEBAR_LINE_HEIGHT: f64 = 1.25;

/// The part of pasted text a one-line field takes: what follows a newline
/// would have been the next command in a terminal, not more of the field.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// What the sidebar is currently capturing the keyboard for, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarFocus {
    Search,
    Rename(NodeId),
    /// The file page's path bar; its text is `TermWindow.files.path_edit`.
    FilesPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    NewSession,
    NewSshSession,
    NewFolder,
    Rename,
    Edit,
    Delete,
}

impl MenuAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NewSession => "New Session",
            Self::NewSshSession => "New SSH Session",
            Self::NewFolder => "New Folder",
            Self::Rename => "Rename",
            Self::Edit => "Edit...",
            Self::Delete => "Delete",
        }
    }
}

/// Which ssh session the form edits: a node that exists, or one to be
/// created under `parent` when the form is saved (so that cancelling leaves
/// nothing behind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshEditTarget {
    New { parent: Option<NodeId> },
    Existing(NodeId),
}

/// A right-click menu. We draw this ourselves because there is no native
/// context menu API in the `window` crate on any of our platforms.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenu {
    /// The node that was clicked, or None for the empty area below the tree.
    pub target: Option<NodeId>,
    pub x: f32,
    pub y: f32,
    pub items: Vec<MenuAction>,
}

/// A node being dragged to a new place in the tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeDrag {
    pub id: NodeId,
    /// Where it would go if dropped now; None over anywhere it cannot go.
    pub target: Option<Placement>,
}

pub struct SidebarState {
    pub collapsed: bool,

    /// The page the panel shows; picked from the tab rail.
    pub page: SidebarPage,

    /// User's dragged width in pixels; None means "use the configured width".
    pub width: Option<f32>,

    /// Width being previewed during a splitter drag.
    ///
    /// We don't commit it until the button is released: changing the real width
    /// mid-drag would recompute the terminal geometry, and therefore SIGWINCH
    /// every pty in the workspace, on every mouse-move.
    pub pending_width: Option<f32>,

    /// Set once the pointer has gone far enough from a press on a node for it
    /// to be a drag rather than a click.
    pub drag: Option<NodeDrag>,

    pub scroll_offset: usize,
    pub selected: Option<NodeId>,
    pub focus: Option<SidebarFocus>,
    pub search: String,

    /// Buffer backing an in-place rename.
    pub edit_buffer: String,

    /// Uncommitted IME composition text belonging to the focused field.
    ///
    /// While the sidebar owns the keyboard this holds what the terminal would
    /// otherwise have drawn at its own cursor; see the `AdviseDeadKeyStatus`
    /// handler in `termwindow::mod`.
    pub composing: Option<String>,

    pub menu: Option<ContextMenu>,

    /// A node whose workspace we have asked for but which hasn't come up yet.
    ///
    /// Opening a workspace that doesn't exist is asynchronous, so the second
    /// click of a double-click would otherwise still see it as absent and spawn
    /// a duplicate window.
    pub pending_open: Option<NodeId>,

    /// Nodes whose most recent launch failed, so they can be drawn differently
    /// rather than appearing to do nothing when clicked.
    pub failed: HashSet<NodeId>,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            collapsed: false,
            page: SidebarPage::default(),
            width: None,
            pending_width: None,
            drag: None,
            scroll_offset: 0,
            selected: None,
            focus: None,
            search: String::new(),
            edit_buffer: String::new(),
            composing: None,
            menu: None,
            pending_open: None,
            failed: HashSet::new(),
        }
    }
}

impl super::TermWindow {
    pub fn sidebar_is_enabled(&self) -> bool {
        self.config.session_sidebar_enabled
    }

    /// Is the tree showing? False while collapsed to the rail.
    pub fn sidebar_is_expanded(&self) -> bool {
        self.sidebar_is_enabled() && !self.sidebar.collapsed
    }

    /// The width the sidebar occupies, in pixels: the tab rail plus, unless
    /// collapsed, the panel.
    ///
    /// This is folded into `border.left`, which the rest of the GUI already
    /// treats as the left origin of the content area, so widening it shifts the
    /// panes, splits, tab bar and mouse coordinates all at once.
    pub fn sidebar_width(&self) -> f32 {
        if !self.sidebar_is_enabled() {
            return 0.;
        }
        self.sidebar_rail_width() + self.sidebar_panel_width()
    }

    /// Width of the page tab rail; see [`COLLAPSED_RAIL_CELLS`].
    pub fn sidebar_rail_width(&self) -> f32 {
        if !self.sidebar_is_enabled() {
            return 0.;
        }
        self.render_metrics.cell_size.width as f32 * COLLAPSED_RAIL_CELLS
    }

    /// Width of the panel to the right of the rail; 0 while collapsed.
    ///
    /// `session_sidebar_width` and a dragged width both size the panel alone.
    pub fn sidebar_panel_width(&self) -> f32 {
        if !self.sidebar_is_expanded() {
            return 0.;
        }

        let context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.dimensions.pixel_width as f32,
            pixel_cell: self.render_metrics.cell_size.width as f32,
        };

        let configured = self
            .config
            .session_sidebar_width
            .evaluate_as_pixels(context);

        clamp_width(
            self.sidebar.width.unwrap_or(configured),
            self.dimensions.pixel_width as f32,
            context.pixel_cell * MIN_SIDEBAR_CELLS,
        )
    }

    /// A click on one of the rail's page tabs.
    pub fn click_sidebar_page(&mut self, clicked: SidebarPage) {
        let (page, collapsed) = rail_click(clicked, self.sidebar.page, self.sidebar.collapsed);
        if page != self.sidebar.page {
            // The search box and rename field belong to the sessions page;
            // don't leave the keyboard captured by one that isn't showing.
            self.release_sidebar_focus();
            self.sidebar.menu = None;
            self.sidebar.page = page;
        }
        if collapsed != self.sidebar.collapsed {
            self.toggle_session_sidebar();
        } else {
            self.invalidate_sidebar();
        }
    }

    pub fn toggle_session_sidebar(&mut self) {
        if !self.sidebar_is_enabled() {
            log::warn!(
                "ToggleSessionSidebar: the sidebar is disabled; \
                 set session_sidebar_enabled = true to use it"
            );
            return;
        }
        self.sidebar.collapsed = !self.sidebar.collapsed;
        if self.sidebar.collapsed {
            // Don't leave the keyboard captured by something that isn't visible.
            self.release_sidebar_focus();
            self.sidebar.menu = None;
        }
        self.apply_chrome_geometry_change();
    }

    pub fn focus_session_sidebar_search(&mut self) {
        if !self.sidebar_is_enabled() {
            return;
        }
        if self.sidebar.page != SidebarPage::Sessions {
            self.sidebar.menu = None;
            self.sidebar.page = SidebarPage::Sessions;
        }
        if self.sidebar.collapsed {
            self.sidebar.collapsed = false;
            self.apply_chrome_geometry_change();
        }
        self.sidebar.focus = Some(SidebarFocus::Search);
        self.invalidate_sidebar();
    }

    /// Re-run the window geometry now that the sidebar's width or the status
    /// bar has changed.
    pub(crate) fn apply_chrome_geometry_change(&mut self) {
        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            let dimensions = self.dimensions;
            self.apply_dimensions(&dimensions, None, &window);
            window.invalidate();
        }
    }

    /// Drop the cached sidebar geometry without asking for a repaint.
    ///
    /// Used from the resize paths, which already schedule their own repaint.
    pub fn invalidate_sidebar_cache(&mut self) {
        self.sidebar_computed = None;
    }

    /// Drop the cached geometry and repaint.
    ///
    /// Every state change that alters what the sidebar draws must come through
    /// here, otherwise the cache serves a stale frame.
    pub fn invalidate_sidebar(&mut self) {
        self.sidebar_computed = None;
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// How many terminals are running under `id`.
    ///
    /// A node owns one workspace, which may hold several mux windows; the
    /// count the user cares about is tabs, not windows.
    pub fn session_terminal_count(&self, id: NodeId) -> usize {
        let mux = Mux::get();
        mux.iter_windows_in_workspace(&id.workspace_name())
            .into_iter()
            .filter_map(|window_id| mux.get_window(window_id).map(|w| w.len()))
            .sum()
    }

    /// Workspaces running with nothing on screen showing them, and how many
    /// terminals each is holding.
    ///
    /// The question that matters when closing is not "which session nodes are
    /// running" but "what would die that the user cannot see". That includes
    /// the workspace wezterm started in, which becomes unattended as soon as
    /// you switch to a session and is not a session node at all.
    ///
    /// Windows another GUI window *is* showing are excluded: those are ordinary
    /// extra windows with close buttons of their own.
    pub fn unattended_workspaces(&self) -> Vec<(String, usize)> {
        let mux = Mux::get();
        let front_end = crate::frontend::front_end();

        // Workspace name -> the sidebar entry it belongs to, so the prompt can
        // say "casb" rather than "session:2".
        let labels: HashMap<String, String> = with_store(|store| {
            store
                .tree()
                .sessions_with_names()
                .into_iter()
                .map(|(id, name)| (id.workspace_name(), name))
                .collect()
        });

        let mut totals: BTreeMap<String, usize> = BTreeMap::new();
        for window_id in mux.iter_windows() {
            if window_id == self.mux_window_id {
                // This is the one being closed; it is not a surprise.
                continue;
            }
            if front_end.gui_window_for_mux_window(window_id).is_some() {
                continue;
            }
            if let Some(window) = mux.get_window(window_id) {
                let tabs = window.len();
                if tabs == 0 {
                    continue;
                }
                let workspace = window.get_workspace().to_string();
                let label = labels.get(&workspace).cloned().unwrap_or(workspace);
                *totals.entry(label).or_default() += tabs;
            }
        }

        totals.into_iter().collect()
    }

    /// How many rows of the tree fit in the sidebar right now.
    pub fn sidebar_row_capacity(&self) -> usize {
        let row_height = self.sidebar_row_height();
        if row_height <= 0. {
            return 0;
        }
        let available = self.sidebar_bounds().height()
            // the search box occupies one row
            - row_height;
        (available / row_height).floor().max(0.) as usize
    }

    fn sidebar_total_rows(&self) -> usize {
        let search = self.sidebar.search.clone();
        with_store(|store| {
            let tree = store.tree();
            let filter = tree.matching_ids(&search);
            tree.visible_rows(filter.as_ref()).len()
        })
    }

    pub fn sidebar_scroll(&mut self, lines: isize) {
        let total = self.sidebar_total_rows();
        let capacity = self.sidebar_row_capacity();
        let offset = self.sidebar.scroll_offset as isize + lines;
        self.sidebar.scroll_offset = clamp_scroll(total, offset.max(0) as usize, capacity);
        self.invalidate_sidebar();
    }

    /// Single click: highlight the node, and switch to it if it is already
    /// running. Never starts anything.
    ///
    /// Browsing the tree must not spawn shells; with a few dozen nodes that
    /// would leave a trail of processes behind every time you looked around.
    pub fn select_session_node(&mut self, id: NodeId) {
        let is_folder = with_store(|store| {
            store
                .tree()
                .find(id)
                .map(|node| node.is_folder())
                .unwrap_or(false)
        });

        self.sidebar.selected = Some(id);

        if is_folder {
            with_store(|store| store.mutate(|tree| tree.toggle_expanded(id)));
            self.invalidate_sidebar();
            return;
        }

        // Switching to something already running is free, so a plain click
        // still does the useful half of the job.
        if self.session_terminal_count(id) > 0 {
            // Restarting with sessions already running puts the user in front
            // of the empty window the sidebar lives in: the sessions are in the
            // mux, but nothing is showing them yet and so there is no pane.
            // Returning here left a single click doing nothing in exactly the
            // state where switching is the only thing anyone wants -- and the
            // double click that did work opens a tab nobody asked for on the
            // way past. Switching never needed a pane; only
            // perform_key_assignment's signature did.
            //
            // Neither arm spawns. A single click starting something is the
            // behaviour this gesture was split away from, and this branch is
            // only reached for a workspace that is already up.
            match self.get_active_pane_or_overlay() {
                Some(pane) => {
                    if let Err(err) = self.perform_key_assignment(
                        &pane,
                        &KeyAssignment::SwitchToWorkspace {
                            name: Some(id.workspace_name()),
                            spawn: None,
                        },
                    ) {
                        log::error!("failed to switch to session {id}: {err:#}");
                    }
                }
                None => self.switch_to_workspace(&Some(id.workspace_name()), &None),
            }
        }

        self.invalidate_sidebar();
    }

    /// Open (or switch to) the workspace behind a node. This is the
    /// double-click path and the only one that starts a terminal.
    ///
    /// `want_new_tab` asks for an extra tab, which only applies when the node's
    /// workspace is already up: on a node that isn't running yet the first
    /// double-click has already started bringing one up, and asking for a
    /// second would race it.
    pub fn activate_session_node(&mut self, id: NodeId, want_new_tab: bool) {
        enum Target {
            Folder,
            Session(SpawnCommand),
            Gone,
        }

        let target = with_store(|store| match store.tree().find(id) {
            Some(SessionNode {
                kind: NodeKind::Folder { .. },
                ..
            }) => Target::Folder,
            Some(SessionNode {
                kind: NodeKind::Session { spawn },
                ..
            }) => Target::Session(spawn.into()),
            None => Target::Gone,
        });

        let mut command = match target {
            Target::Gone => {
                log::warn!("session node {id} no longer exists");
                return;
            }
            Target::Folder => {
                with_store(|store| store.mutate(|tree| tree.toggle_expanded(id)));
                self.sidebar.selected = Some(id);
                self.invalidate_sidebar();
                return;
            }
            Target::Session(command) => command,
        };

        // An ssh node connects through its own runtime domain, whatever its
        // stored spawn says; the domain is (re)registered here so that an
        // edit made since the last connection takes effect.
        if with_ssh_hosts(|store| store.get(id).is_some()) {
            match crate::sessions::ssh::ensure_ssh_domain(id) {
                Ok(_) => {
                    command.domain = SpawnTabDomain::DomainName(ssh_domain_name(id));
                }
                Err(err) => {
                    log::error!("failed to prepare ssh session {id}: {err:#}");
                    self.sidebar.failed.insert(id);
                    self.invalidate_sidebar();
                    return;
                }
            }
        }

        self.sidebar.selected = Some(id);

        let workspace = id.workspace_name();
        let mux = Mux::get();
        let already_open = !mux.iter_windows_in_workspace(&workspace).is_empty();

        if already_open {
            self.sidebar.pending_open = None;
            self.sidebar.failed.remove(&id);
        } else {
            if self.sidebar.pending_open == Some(id) {
                // We asked for this workspace on an earlier gesture and it
                // still hasn't appeared, so that launch failed. The spawn is
                // async and its error only reaches the log, so this is the
                // signal we have; mark the node and try again.
                self.sidebar.failed.insert(id);
            }
            self.sidebar.pending_open = Some(id);
        }

        // A window with no panes is exactly the state the user is in when picking
        // their very first session, so returning early here would make the
        // sidebar look dead on a fresh start. Switching workspaces never needed
        // a pane -- only perform_key_assignment's signature did.
        match self.get_active_pane_or_overlay() {
            Some(pane) => {
                if let Err(err) = self.perform_key_assignment(
                    &pane,
                    &KeyAssignment::SwitchToWorkspace {
                        name: Some(workspace.clone()),
                        spawn: Some(command.clone()),
                    },
                ) {
                    log::error!("failed to open session {id}: {err:#}");
                    self.sidebar.failed.insert(id);
                    self.sidebar.pending_open = None;
                    return;
                }
            }
            None => {
                self.switch_to_workspace(&Some(workspace.clone()), &Some(command.clone()));
            }
        }

        if want_new_tab && already_open {
            // Address the mux window explicitly rather than relying on this GUI
            // window having finished reconciling to the new workspace.
            if let Some(mux_window_id) = mux.iter_windows_in_workspace(&workspace).first().copied()
            {
                let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
                crate::spawn::spawn_command_impl(
                    &command,
                    SpawnWhere::NewTab,
                    self.terminal_size,
                    Some(mux_window_id),
                    term_config,
                );
            }
        }

        self.invalidate_sidebar();
    }

    /// Does the sidebar currently own the keyboard?
    pub fn sidebar_has_keyboard_focus(&self) -> bool {
        self.sidebar.focus.is_some()
    }

    /// Hand the keyboard back to the terminal.
    pub(crate) fn release_sidebar_focus(&mut self) {
        self.sidebar.focus = None;
        self.sidebar.composing = None;
        self.files.path_edit = None;
    }

    /// Insert committed text into whichever sidebar field has focus.
    ///
    /// This is the entry point for IME output. Without it, every keystroke
    /// typed while a CJK input method is active -- including plain ASCII, which
    /// the IME still composes -- is delivered as `Key::Composed` and goes
    /// straight to the terminal.
    /// Paste `clipboard` into the focused sidebar field. The fields are one
    /// line, so only the first line is taken.
    pub fn sidebar_paste(&mut self, clipboard: ::window::Clipboard) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let future = window.get_clipboard(clipboard);
        promise::spawn::spawn(async move {
            match future.await {
                Ok(text) => window.notify(crate::termwindow::TermWindowNotif::Apply(Box::new(
                    move |tw| {
                        if tw.sidebar_has_keyboard_focus() {
                            tw.sidebar_insert_text(first_line(&text));
                        }
                    },
                ))),
                Err(err) => log::error!("reading the clipboard: {err:#}"),
            }
        })
        .detach();
    }

    pub fn sidebar_insert_text(&mut self, text: &str) {
        match self.sidebar.focus.clone() {
            Some(SidebarFocus::Search) => {
                self.sidebar.search.push_str(text);
                self.sidebar.scroll_offset = 0;
            }
            Some(SidebarFocus::Rename(_)) => self.sidebar.edit_buffer.push_str(text),
            Some(SidebarFocus::FilesPath) => self.files_path_type(text),
            None => return,
        }
        self.sidebar.composing = None;
        self.invalidate_sidebar();
    }

    pub fn close_sidebar_menu(&mut self) {
        if self.sidebar.menu.take().is_some() {
            self.invalidate_sidebar();
        }
    }

    fn open_sidebar_menu(&mut self, target: Option<NodeId>, x: f32, y: f32) {
        let items = match target {
            Some(_) => vec![
                MenuAction::NewSession,
                MenuAction::NewSshSession,
                MenuAction::NewFolder,
                MenuAction::Rename,
                MenuAction::Edit,
                MenuAction::Delete,
            ],
            // Nothing to rename, edit or delete in empty space.
            None => vec![
                MenuAction::NewSession,
                MenuAction::NewSshSession,
                MenuAction::NewFolder,
            ],
        };
        self.sidebar.menu = Some(ContextMenu {
            target,
            x,
            y,
            items,
        });
        self.sidebar.selected = target.or(self.sidebar.selected);
        self.invalidate_sidebar();
    }

    fn begin_rename(&mut self, id: NodeId) {
        let name = with_store(|store| store.tree().find(id).map(|n| n.name.clone()));
        match name {
            Some(name) => {
                self.sidebar.edit_buffer = name;
                self.sidebar.focus = Some(SidebarFocus::Rename(id));
                self.sidebar.selected = Some(id);
                self.invalidate_sidebar();
            }
            None => log::warn!("cannot rename session node {id}: it no longer exists"),
        }
    }

    /// Where a newly created node should go: inside `target` when it is a
    /// folder, otherwise alongside it.
    fn insertion_parent(&self, target: Option<NodeId>) -> Option<NodeId> {
        let target = target?;
        with_store(|store| {
            let tree = store.tree();
            match tree.find(target) {
                Some(node) if node.is_folder() => Some(target),
                Some(_) => tree.parent_of(target),
                None => None,
            }
        })
    }

    fn perform_sidebar_menu_action(&mut self, action: MenuAction) {
        let target = self.sidebar.menu.as_ref().and_then(|m| m.target);
        self.close_sidebar_menu();

        match action {
            MenuAction::NewSession | MenuAction::NewFolder => {
                let parent = self.insertion_parent(target);
                let is_folder = action == MenuAction::NewFolder;
                let id = with_store(|store| {
                    store.mutate(|tree| {
                        let id = tree.alloc_id();
                        let node = if is_folder {
                            SessionNode::folder(id, "New Folder")
                        } else {
                            SessionNode::session(id, "New Session", SessionSpawn::default())
                        };
                        if tree.insert(parent, node.clone()) {
                            Some(id)
                        } else {
                            // The parent vanished between opening the menu and
                            // choosing an item; fall back to the top level.
                            tree.insert(None, node);
                            Some(id)
                        }
                    })
                });
                match (id, is_folder) {
                    // A folder only needs a name, so rename it in place. A
                    // session also needs a directory and possibly a domain, so
                    // go straight to the editor rather than leaving a node
                    // called "New Session" that opens a default shell.
                    (Some(id), true) => self.begin_rename(id),
                    (Some(id), false) => self.show_session_editor(id),
                    (None, _) => {}
                }
            }
            MenuAction::NewSshSession => {
                let parent = self.insertion_parent(target);
                self.show_ssh_editor(SshEditTarget::New { parent });
            }
            MenuAction::Rename => {
                if let Some(id) = target {
                    self.begin_rename(id);
                }
            }
            MenuAction::Edit => {
                if let Some(id) = target {
                    self.show_session_editor(id);
                }
            }
            MenuAction::Delete => {
                if let Some(id) = target {
                    self.delete_session_node(id);
                }
            }
        }
    }

    fn show_session_editor(&mut self, id: NodeId) {
        let node = with_store(|store| {
            store.tree().find(id).map(|node| {
                let spawn = match &node.kind {
                    NodeKind::Session { spawn } => spawn.clone(),
                    NodeKind::Folder { .. } => SessionSpawn::default(),
                };
                (node.name.clone(), spawn, node.is_folder())
            })
        });

        let (name, spawn, is_folder) = match node {
            Some(node) => node,
            None => return,
        };

        if is_folder {
            // A folder has nothing to configure beyond its name.
            self.begin_rename(id);
            return;
        }

        if with_ssh_hosts(|store| store.get(id).is_some()) {
            self.show_ssh_editor(SshEditTarget::Existing(id));
        } else {
            self.show_local_editor(id, name, spawn);
        }
    }

    /// Put a form up in place of whatever the sidebar was doing. A form is a
    /// modal rather than an overlay, so it needs no tab: it works in the empty
    /// window the user starts with.
    pub(crate) fn open_form(&mut self, form: FormModal) {
        self.release_sidebar_focus();
        self.close_sidebar_menu();
        self.set_modal(std::rc::Rc::new(form));
    }

    fn show_local_editor(&mut self, id: NodeId, name: String, spawn: SessionSpawn) {
        let old = spawn.clone();
        let form = FormModal::new(forms::local_form(&name, &spawn))
            .validate(forms::validate_local_form)
            .on_submit(move |state, tw| {
                let (name, spawn) = forms::local_form_values(state, &old);
                with_store(|store| {
                    store.mutate(|tree| {
                        if let Some(node) = tree.find_mut(id) {
                            if !name.is_empty() {
                                node.name = name;
                            }
                            if let NodeKind::Session { spawn: s } = &mut node.kind {
                                *s = spawn;
                            }
                        }
                    })
                });
                tw.invalidate_sidebar();
            });
        self.open_form(form);
    }

    pub fn show_ssh_editor(&mut self, target: SshEditTarget) {
        let (name, host) = match target {
            SshEditTarget::New { .. } => (String::new(), None),
            SshEditTarget::Existing(id) => {
                let name = with_store(|store| store.tree().find(id).map(|n| n.name.clone()));
                let Some(name) = name else {
                    return;
                };
                (name, with_ssh_hosts(|store| store.get(id).cloned()))
            }
        };
        let form = FormModal::new(forms::ssh_form(&name, host.as_ref()))
            .on_change(forms::apply_auth_choice)
            .validate(forms::validate_ssh_form)
            .on_extra(|field, tw| tw.browse_for_form_field(field))
            .on_submit(move |state, tw| {
                let (name, host) = forms::ssh_form_values(state);
                let id = match target {
                    SshEditTarget::Existing(id) => {
                        with_store(|store| store.mutate(|tree| tree.rename(id, name.clone())));
                        id
                    }
                    SshEditTarget::New { parent } => with_store(|store| {
                        store.mutate(|tree| {
                            let id = tree.alloc_id();
                            let spawn = SessionSpawn {
                                // Recorded so that an older build, which knows
                                // nothing of ssh_hosts.json, fails to open this
                                // node rather than opening a local shell.
                                domain: SpawnTabDomain::DomainName(ssh_domain_name(id)),
                                ..SessionSpawn::default()
                            };
                            let node = SessionNode::session(id, name.clone(), spawn);
                            if !tree.insert(parent, node.clone()) {
                                tree.insert(None, node);
                            }
                            id
                        })
                    }),
                };
                with_ssh_hosts(|store| store.set(id, host));
                tw.sidebar.selected = Some(id);
                tw.sidebar.failed.remove(&id);
                tw.invalidate_sidebar();
            });
        self.open_form(form);
    }

    /// The "浏览…" button of a form field: pick a file for it.
    ///
    /// The dialog has a thread of its own, so the form may have gone, or
    /// been replaced, by the time it returns; only the form that asked gets
    /// the answer.
    fn browse_for_form_field(&mut self, field: usize) {
        let Some(modal) = self.get_modal() else {
            return;
        };
        let asked = std::rc::Rc::as_ptr(&modal) as *const () as usize;
        let Some(window) = self.window.clone() else {
            return;
        };
        promise::spawn::spawn(async move {
            let picked = crate::sftp::dialogs::pick_open_files(false).await;
            let Some(path) = picked.into_iter().next() else {
                return;
            };
            let value = path.display().to_string();
            window.notify(crate::termwindow::TermWindowNotif::Apply(Box::new(
                move |tw| {
                    let Some(modal) = tw.get_modal() else {
                        return;
                    };
                    if std::rc::Rc::as_ptr(&modal) as *const () as usize != asked {
                        return;
                    }
                    if let Some(form) = modal.downcast_ref::<FormModal>() {
                        form.set_field_value(field, &value, tw);
                    }
                },
            )));
        })
        .detach();
    }

    fn confirm_delete_session_node(&mut self, id: NodeId) {
        let name = match with_store(|store| store.tree().find(id).map(|node| node.name.clone())) {
            Some(name) => name,
            None => return,
        };

        let tab = match Mux::get().get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let (overlay, future) = crate::overlay::start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::session_edit::confirm_delete_session_node(term, id, name)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn delete_session_node(&mut self, id: NodeId) {
        // Deleting the tree entry doesn't kill anything that is running: the
        // workspace stays alive and is still reachable from the launcher.
        let has_live_terminals = !Mux::get()
            .iter_windows_in_workspace(&id.workspace_name())
            .is_empty();

        if has_live_terminals {
            self.confirm_delete_session_node(id);
        } else {
            if let Some(removed) = with_store(|store| store.mutate(|tree| tree.remove(id))) {
                crate::sessions::ssh::forget_ssh_hosts(&removed.session_ids());
            }
            if self.sidebar.selected == Some(id) {
                self.sidebar.selected = None;
            }
            self.invalidate_sidebar();
        }
    }

    // --- keyboard -------------------------------------------------------

    /// Handle a key while the sidebar holds focus. Returns true when consumed.
    pub fn sidebar_key_down(&mut self, key: &KeyCode, mods: KeyModifiers) -> bool {
        let focus = match self.sidebar.focus.clone() {
            Some(focus) => focus,
            None => return false,
        };

        if let KeyCode::Char('v') | KeyCode::Char('V') = key {
            if mods.contains(KeyModifiers::CTRL) {
                self.sidebar_paste(::window::Clipboard::Clipboard);
                return true;
            }
        }

        match focus {
            SidebarFocus::Search => self.search_key_down(key, mods),
            SidebarFocus::Rename(id) => self.rename_key_down(id, key, mods),
            SidebarFocus::FilesPath => self.files_path_key_down(key, mods),
        }
    }

    fn search_key_down(&mut self, key: &KeyCode, mods: KeyModifiers) -> bool {
        match (key, mods) {
            (KeyCode::Escape, _) => {
                self.release_sidebar_focus();
                self.sidebar.search.clear();
                self.sidebar.scroll_offset = 0;
            }
            (KeyCode::Enter, _) => {
                // Open whatever the filter has narrowed us down to.
                let first = self.first_visible_node();
                self.release_sidebar_focus();
                self.sidebar.search.clear();
                self.sidebar.scroll_offset = 0;
                if let Some(id) = first {
                    self.activate_session_node(id, false);
                    return true;
                }
            }
            (KeyCode::UpArrow, _) => {
                self.sidebar_scroll(-1);
                return true;
            }
            (KeyCode::DownArrow, _) => {
                self.sidebar_scroll(1);
                return true;
            }
            (KeyCode::Backspace, _) => {
                self.sidebar.search.pop();
                self.sidebar.scroll_offset = 0;
            }
            (KeyCode::Char('u'), KeyModifiers::CTRL) => {
                self.sidebar.search.clear();
                self.sidebar.scroll_offset = 0;
            }
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                self.sidebar.search.push(*c);
                self.sidebar.scroll_offset = 0;
            }
            _ => return false,
        }
        self.invalidate_sidebar();
        true
    }

    fn rename_key_down(&mut self, id: NodeId, key: &KeyCode, mods: KeyModifiers) -> bool {
        match (key, mods) {
            (KeyCode::Escape, _) => {
                self.release_sidebar_focus();
                self.sidebar.edit_buffer.clear();
            }
            (KeyCode::Enter, _) => {
                let name = self.sidebar.edit_buffer.trim().to_string();
                self.release_sidebar_focus();
                self.sidebar.edit_buffer.clear();
                if !name.is_empty() {
                    // The workspace is keyed on the node id, so renaming never
                    // orphans terminals that are already running.
                    with_store(|store| store.mutate(|tree| tree.rename(id, name)));
                }
            }
            (KeyCode::Backspace, _) => {
                self.sidebar.edit_buffer.pop();
            }
            (KeyCode::Char('u'), KeyModifiers::CTRL) => {
                self.sidebar.edit_buffer.clear();
            }
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                self.sidebar.edit_buffer.push(*c);
            }
            _ => return false,
        }
        self.invalidate_sidebar();
        true
    }

    fn first_visible_node(&self) -> Option<NodeId> {
        let search = self.sidebar.search.clone();
        with_store(|store| {
            let tree = store.tree();
            let filter = tree.matching_ids(&search);
            tree.visible_rows(filter.as_ref()).first().map(|row| row.id)
        })
    }

    // --- mouse ----------------------------------------------------------

    pub fn mouse_event_sidebar(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        // The wheel scrolls the tree wherever it is over the sidebar, rather
        // than only over a row.
        if let WMEK::VertWheel(amount) = event.kind {
            self.sidebar_scroll(-(amount as isize));
            return;
        }

        match item.item_type.clone() {
            UIItemType::SidebarNode(id) => self.mouse_event_sidebar_node(item, id, event),
            UIItemType::SidebarSearch => {
                if matches!(event.kind, WMEK::Press(MousePress::Left)) {
                    self.sidebar.focus = Some(SidebarFocus::Search);
                    self.invalidate_sidebar();
                }
            }
            UIItemType::SidebarMenuItem(action) => {
                if matches!(event.kind, WMEK::Press(MousePress::Left)) {
                    self.perform_sidebar_menu_action(action);
                }
            }
            UIItemType::SidebarResizeHandle => {
                context.set_cursor(Some(::window::MouseCursor::SizeLeftRight));
                if event.kind == WMEK::Press(MousePress::Left) {
                    self.sidebar.pending_width = Some(self.sidebar_panel_width());
                    self.dragging.replace((item, event));
                }
            }
            UIItemType::SidebarCollapseToggle => {
                if event.kind == WMEK::Press(MousePress::Left) {
                    self.toggle_session_sidebar();
                }
            }
            UIItemType::SidebarPageTab(page) => {
                if event.kind == WMEK::Press(MousePress::Left) {
                    self.click_sidebar_page(page);
                }
            }
            UIItemType::SidebarBackground => {
                if event.kind == WMEK::Press(MousePress::Right) {
                    let (x, y) = (event.coords.x as f32, event.coords.y as f32);
                    self.open_sidebar_menu(None, x, y);
                }
            }
            _ => {}
        }
    }

    fn mouse_event_sidebar_node(&mut self, item: UIItem, id: NodeId, event: MouseEvent) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                let streak = self
                    .last_mouse_click
                    .as_ref()
                    .map(|click| click.streak)
                    .unwrap_or(1);

                if streak == 1 {
                    // Only the highlight for now. Whether this press is a click
                    // or the start of a drag isn't known until the pointer
                    // moves or the button comes up, and a click's own effects
                    // -- a folder folding up, the terminal switching to another
                    // workspace -- would pull the tree out from under a drag.
                    // See finish_sidebar_node_press.
                    self.sidebar.selected = Some(id);
                    self.invalidate_sidebar();
                    self.dragging.replace((item, event));
                    return;
                }

                let is_folder = with_store(|store| {
                    store
                        .tree()
                        .find(id)
                        .map(|node| node.is_folder())
                        .unwrap_or(false)
                });

                // A folder's second click would just undo the first one's
                // expand/collapse.
                if is_folder {
                    return;
                }

                match streak {
                    2 => self.activate_session_node(id, true),
                    // A triple-click is still one gesture; opening again would
                    // race the spawn the second click just started.
                    _ => {}
                }
            }
            WMEK::Press(MousePress::Right) => {
                let (x, y) = (event.coords.x as f32, event.coords.y as f32);
                self.open_sidebar_menu(Some(id), x, y);
            }
            _ => {}
        }
    }

    /// Track a splitter drag. This only moves the preview line; see
    /// [`Self::commit_sidebar_resize`].
    pub fn drag_sidebar_splitter(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        // The dragged edge is the panel's; the rail to its left is fixed.
        let left_edge = self.sidebar_panel_bounds().min_x();
        let desired = (event.coords.x as f32 - left_edge).max(0.);

        let context_width = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.dimensions.pixel_width as f32,
            pixel_cell: self.render_metrics.cell_size.width as f32,
        };

        self.sidebar.pending_width = Some(clamp_width(
            desired,
            self.dimensions.pixel_width as f32,
            context_width.pixel_cell * MIN_SIDEBAR_CELLS,
        ));

        self.dragging.replace((item, start_event));
        self.invalidate_sidebar_cache();
        context.invalidate();
    }

    /// Apply a finished splitter drag.
    ///
    /// This is where the terminal actually gets resized; doing it per
    /// mouse-move would SIGWINCH every pty in the workspace continuously.
    pub fn commit_sidebar_resize(&mut self, context: &dyn WindowOps) {
        if let Some(width) = self.sidebar.pending_width.take() {
            // A click on the splitter that didn't move anywhere shouldn't pin
            // the width and stop it following the configured value.
            if (width - self.sidebar_panel_width()).abs() >= 1. {
                self.sidebar.width = Some(width);
                self.apply_chrome_geometry_change();
            }
        }
        context.invalidate();
    }

    /// Track a press on a node as the pointer moves: once it has gone far
    /// enough to be a drag, work out where the node would land. Nothing moves
    /// until the button comes up; see [`Self::finish_sidebar_node_press`].
    pub fn drag_sidebar_node(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let id = match item.item_type {
            UIItemType::SidebarNode(id) => id,
            _ => return,
        };

        if self.sidebar.drag.is_none() {
            let dx = (event.coords.x - start_event.coords.x) as f32;
            let dy = (event.coords.y - start_event.coords.y) as f32;
            // While a search is narrowing the tree, the rows on screen are not
            // each other's real neighbours, so a gap between two of them is no
            // particular place in the tree.
            let filtering = !self.sidebar.search.trim().is_empty();
            if filtering || dx.hypot(dy) < self.sidebar_row_height() / 4. {
                self.dragging.replace((item, start_event));
                return;
            }
            self.sidebar.drag = Some(NodeDrag { id, target: None });
        }

        let target = self.sidebar_drop_target(id, event.coords.x, event.coords.y);
        if let Some(drag) = self.sidebar.drag.as_mut() {
            if drag.target != target {
                drag.target = target;
                // The marker is drawn over the cached rows rather than built
                // into them, so a repaint is all it needs.
                context.invalidate();
            }
        }
        self.dragging.replace((item, start_event));
    }

    /// Where `dragged` would go if dropped at (x, y), or None when that is
    /// nowhere it can go.
    fn sidebar_drop_target(&self, dragged: NodeId, x: isize, y: isize) -> Option<Placement> {
        let bounds = self.sidebar_panel_bounds();
        if (x as f32) < bounds.min_x() || (x as f32) >= bounds.max_x() {
            return None;
        }

        // The rows as they were last drawn. The tree doesn't change until the
        // drop, so they are still where the user sees them.
        let mut hovered = None;
        let mut last_drawn = None;
        for item in &self.ui_items {
            if let UIItemType::SidebarNode(id) = item.item_type {
                if y >= item.y as isize && y < (item.y + item.height) as isize {
                    let fraction = (y - item.y as isize) as f32 / item.height.max(1) as f32;
                    hovered = Some((id, fraction));
                }
                last_drawn = Some((id, item.y + item.height));
            }
        }

        with_store(|store| {
            let tree = store.tree();
            let rows = tree.visible_rows(None);
            let placement = match hovered {
                Some((id, fraction)) => {
                    let index = rows.iter().position(|row| row.id == id)?;
                    drop_placement(&rows, index, fraction)?
                }
                // Below the last row is the end of the top level -- provided
                // that row is the end of the tree, and not merely the last one
                // that fits on screen.
                None => {
                    let (last_id, bottom) = last_drawn?;
                    let at_end = rows.last().map(|row| row.id) == Some(last_id);
                    if !at_end || y < bottom as isize {
                        return None;
                    }
                    Placement::LastIn(None)
                }
            };
            tree.can_move(dragged, placement).then_some(placement)
        })
    }

    /// The button came up on a press that began on a node. Drop the node where
    /// the drag left it -- or, if it never moved far enough to be a drag, carry
    /// out the click that the press put off.
    pub fn finish_sidebar_node_press(&mut self, id: NodeId, context: &dyn WindowOps) {
        match self.sidebar.drag.take() {
            Some(NodeDrag {
                target: Some(target),
                ..
            }) => {
                with_store(|store| store.mutate(|tree| tree.move_node(id, target)));
                self.invalidate_sidebar();
            }
            // Let go somewhere it cannot go: nothing moves, but the marker has
            // to come off.
            Some(NodeDrag { target: None, .. }) => context.invalidate(),
            None => self.select_session_node(id),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_paste_into_a_sidebar_field_keeps_only_its_first_line() {
        assert_eq!(first_line("/etc/nginx\nrm -rf /tmp/x\n"), "/etc/nginx");
        assert_eq!(first_line("/srv/app\r\n"), "/srv/app");
        assert_eq!(first_line("plain"), "plain");
        assert_eq!(first_line(""), "");
    }
}
