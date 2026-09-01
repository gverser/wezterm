//! Runtime state and event handling for the session sidebar.
//!
//! The sidebar is ordinary window chrome rather than a `Modal`: a modal would
//! take the keyboard for as long as it was open, and the point of the sidebar
//! is that you can use the terminal while it is showing.
//!
//! Drawing lives in `termwindow::render::sidebar`; the tree itself lives in
//! `crate::sessions`.

use crate::sessions::store::with_store;
use crate::sessions::{clamp_scroll, clamp_width, NodeId, NodeKind, SessionNode, SessionSpawn};
use crate::spawn::SpawnWhere;
use crate::termwindow::{UIItem, UIItemType};
use ::window::{MouseEvent, MouseEventKind as WMEK, MousePress, WindowOps};
use config::keyassignment::{KeyAssignment, SpawnCommand};
use config::{DimensionContext, TermConfig};
use mux::Mux;
use std::collections::HashSet;
use std::sync::Arc;
use wezterm_term::{KeyCode, KeyModifiers};

/// Narrowest useful sidebar, in cells of the title font.
pub const MIN_SIDEBAR_CELLS: f32 = 8.;

/// Row height as a multiple of the title font's cell height.
pub const SIDEBAR_LINE_HEIGHT: f64 = 1.25;

/// What the sidebar is currently capturing the keyboard for, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarFocus {
    Search,
    Rename(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    NewSession,
    NewFolder,
    Rename,
    Edit,
    Delete,
}

impl MenuAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NewSession => "New Session",
            Self::NewFolder => "New Folder",
            Self::Rename => "Rename",
            Self::Edit => "Edit...",
            Self::Delete => "Delete",
        }
    }
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

pub struct SidebarState {
    pub collapsed: bool,

    /// User's dragged width in pixels; None means "use the configured width".
    pub width: Option<f32>,

    /// Width being previewed during a splitter drag.
    ///
    /// We don't commit it until the button is released: changing the real width
    /// mid-drag would recompute the terminal geometry, and therefore SIGWINCH
    /// every pty in the workspace, on every mouse-move.
    pub pending_width: Option<f32>,

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
            width: None,
            pending_width: None,
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

    pub fn sidebar_is_visible(&self) -> bool {
        self.sidebar_is_enabled() && !self.sidebar.collapsed
    }

    /// The width the sidebar occupies, in pixels.
    ///
    /// This is folded into `border.left`, which the rest of the GUI already
    /// treats as the left origin of the content area, so widening it shifts the
    /// panes, splits, tab bar and mouse coordinates all at once.
    pub fn sidebar_width(&self) -> f32 {
        if !self.sidebar_is_visible() {
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
        self.apply_sidebar_geometry_change();
    }

    pub fn focus_session_sidebar_search(&mut self) {
        if !self.sidebar_is_enabled() {
            return;
        }
        if self.sidebar.collapsed {
            self.sidebar.collapsed = false;
            self.apply_sidebar_geometry_change();
        }
        self.sidebar.focus = Some(SidebarFocus::Search);
        self.invalidate_sidebar();
    }

    /// Re-run the window geometry now that the sidebar's width has changed.
    fn apply_sidebar_geometry_change(&mut self) {
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

    /// How many rows of the tree fit in the sidebar right now.
    pub fn sidebar_row_capacity(&self) -> usize {
        let row_height = self.sidebar_row_height();
        if row_height <= 0. {
            return 0;
        }
        let border = self.get_os_border();
        let available = (self.dimensions.pixel_height as f32)
            - (border.top + border.bottom).get() as f32
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

    /// Open (or switch to) the workspace behind a node.
    ///
    /// `want_new_tab` corresponds to a double-click: it only adds a tab when
    /// the node's workspace is already up. On a node that isn't open yet the
    /// first click has already started bringing one up, and asking for a second
    /// would race it.
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

        let command = match target {
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

        self.sidebar.selected = Some(id);

        let workspace = id.workspace_name();
        let mux = Mux::get();
        let already_open = !mux.iter_windows_in_workspace(&workspace).is_empty();

        if already_open {
            self.sidebar.pending_open = None;
            self.sidebar.failed.remove(&id);
        } else if want_new_tab && self.sidebar.pending_open == Some(id) {
            // The first click of this double-click is still bringing the
            // workspace up; spawning again would give us two windows.
            //
            // Only the double-click path checks this. A plain click always
            // retries, so a launch that failed can never wedge the node.
            return;
        } else {
            if self.sidebar.pending_open == Some(id) {
                // We asked for this workspace on an earlier click and it still
                // hasn't appeared, so that launch failed. The spawn is async
                // and its error only reaches the log, so this is the signal we
                // have; mark the node and try again.
                self.sidebar.failed.insert(id);
            }
            self.sidebar.pending_open = Some(id);
        }

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

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
    fn release_sidebar_focus(&mut self) {
        self.sidebar.focus = None;
        self.sidebar.composing = None;
    }

    /// Insert committed text into whichever sidebar field has focus.
    ///
    /// This is the entry point for IME output. Without it, every keystroke
    /// typed while a CJK input method is active -- including plain ASCII, which
    /// the IME still composes -- is delivered as `Key::Composed` and goes
    /// straight to the terminal.
    pub fn sidebar_insert_text(&mut self, text: &str) {
        match self.sidebar.focus.clone() {
            Some(SidebarFocus::Search) => {
                self.sidebar.search.push_str(text);
                self.sidebar.scroll_offset = 0;
            }
            Some(SidebarFocus::Rename(_)) => self.sidebar.edit_buffer.push_str(text),
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
                MenuAction::NewFolder,
                MenuAction::Rename,
                MenuAction::Edit,
                MenuAction::Delete,
            ],
            // Nothing to rename, edit or delete in empty space.
            None => vec![MenuAction::NewSession, MenuAction::NewFolder],
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

        let tab = match Mux::get().get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let (overlay, future) = crate::overlay::start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::session_edit::edit_session_node(term, id, name, spawn)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
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
            with_store(|store| store.mutate(|tree| tree.remove(id)));
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

        match focus {
            SidebarFocus::Search => self.search_key_down(key, mods),
            SidebarFocus::Rename(id) => self.rename_key_down(id, key, mods),
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
            UIItemType::SidebarNode(id) => self.mouse_event_sidebar_node(id, event),
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
                    self.sidebar.pending_width = Some(self.sidebar_width());
                    self.dragging.replace((item, event));
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

    fn mouse_event_sidebar_node(&mut self, id: NodeId, event: MouseEvent) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                let streak = self
                    .last_mouse_click
                    .as_ref()
                    .map(|click| click.streak)
                    .unwrap_or(1);

                let is_folder = with_store(|store| {
                    store
                        .tree()
                        .find(id)
                        .map(|node| node.is_folder())
                        .unwrap_or(false)
                });

                // A folder's second click would just undo the first one's
                // expand/collapse.
                if is_folder && streak >= 2 {
                    return;
                }

                self.activate_session_node(id, streak >= 2);
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
        let border = self.get_os_border();
        let left_edge = (border.left.get() as f32) - self.sidebar_width();
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
            if (width - self.sidebar_width()).abs() >= 1. {
                self.sidebar.width = Some(width);
                self.apply_sidebar_geometry_change();
            }
        }
        context.invalidate();
    }
}
