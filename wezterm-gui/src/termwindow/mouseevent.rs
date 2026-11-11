use crate::spawn::SpawnWhere;
use crate::tabbar::{tab_drop_index, TabBarItem};
use crate::termwindow::{
    tab_member, GuiWin, MouseCapture, PositionedSplit, ScrollHit, TermWindowNotif, UIItem,
    UIItemType, TMB,
};
use ::window::{
    MouseButtons as WMB, MouseCursor, MouseEvent, MouseEventKind as WMEK, MousePress,
    WindowDecorations, WindowOps, WindowState,
};
use config::keyassignment::{KeyAssignment, MouseEventTrigger, SpawnCommand, SpawnTabDomain};
use config::MouseEventAltScreen;
use mux::pane::{Pane, PaneId, WithPaneLines};
use mux::tab::SplitDirection;
use mux::Mux;
use mux_lua::MuxPane;
use std::convert::TryInto;
use std::ops::Sub;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::Line;
use wezterm_dynamic::ToDynamic;
use wezterm_term::input::{MouseButton, MouseEventKind as TMEK, CLICK_INTERVAL};
use wezterm_term::{ClickPosition, LastMouseClick, StableRowIndex};

impl super::TermWindow {
    fn resolve_ui_item(&self, event: &MouseEvent) -> Option<UIItem> {
        let x = event.coords.x;
        let y = event.coords.y;
        self.ui_items
            .iter()
            .rev()
            .find(|item| item.hit_test(x, y))
            .cloned()
    }

    fn leave_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {
                self.update_title_post_status();
            }
            UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::SidebarBackground
            | UIItemType::SidebarNode(_)
            | UIItemType::SidebarSearch
            | UIItemType::SidebarResizeHandle
            | UIItemType::SidebarMenuItem(_)
            | UIItemType::SidebarHeaderAction(_)
            | UIItemType::SidebarCollapseToggle
            | UIItemType::SidebarPageTab(_)
            | UIItemType::FilesRow(_)
            | UIItemType::FilesToolbar(_)
            | UIItemType::FilesPath
            | UIItemType::FilesFollow
            | UIItemType::FilesMenuItem(_)
            | UIItemType::FilesBackground
            | UIItemType::PaneTab { .. }
            | UIItemType::PaneTabClose { .. }
            | UIItemType::PaneTabBar { .. }
            | UIItemType::ModalItem(_) => {}
        }
    }

    fn enter_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {}
            UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::SidebarBackground
            | UIItemType::SidebarNode(_)
            | UIItemType::SidebarSearch
            | UIItemType::SidebarResizeHandle
            | UIItemType::SidebarMenuItem(_)
            | UIItemType::SidebarHeaderAction(_)
            | UIItemType::SidebarCollapseToggle
            | UIItemType::SidebarPageTab(_)
            | UIItemType::FilesRow(_)
            | UIItemType::FilesToolbar(_)
            | UIItemType::FilesPath
            | UIItemType::FilesFollow
            | UIItemType::FilesMenuItem(_)
            | UIItemType::FilesBackground
            | UIItemType::PaneTab { .. }
            | UIItemType::PaneTabClose { .. }
            | UIItemType::PaneTabBar { .. }
            | UIItemType::ModalItem(_) => {}
        }
    }

    pub fn mouse_event_impl(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        log::trace!("{:?}", event);
        // A window can legitimately have no panes: that is exactly what a
        // deliberately empty window looks like while the user picks their first
        // session out of the sidebar. Returning here made the entire window
        // inert -- not just the terminal area, but the sidebar, the tab bar and
        // every button on them.
        let pane = self.get_active_pane_or_overlay();

        self.current_mouse_event.replace(event.clone());

        let border = self.get_os_border();

        let first_line_offset = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            self.tab_bar_pixel_height().unwrap_or(0.) as isize
        } else {
            0
        } + border.top.get() as isize;

        let (padding_left, padding_top) = self.padding_left_top();

        let y = (event
            .coords
            .y
            .sub(padding_top as isize)
            .sub(first_line_offset)
            .max(0)
            / self.render_metrics.cell_size.height) as i64;

        let x = (event
            .coords
            .x
            .sub((padding_left + border.left.get() as f32) as isize)
            .max(0) as f32)
            / self.render_metrics.cell_size.width as f32;
        let x = if !pane.as_ref().map_or(false, |p| p.is_mouse_grabbed()) {
            // Round the x coordinate so that we're a bit more forgiving of
            // the horizontal position when selecting cells
            x.round()
        } else {
            x
        }
        .trunc() as usize;

        let mut y_pixel_offset = event
            .coords
            .y
            .sub(padding_top as isize)
            .sub(first_line_offset);
        if y > 0 {
            y_pixel_offset = y_pixel_offset.max(0) % self.render_metrics.cell_size.height;
        }

        let mut x_pixel_offset = event
            .coords
            .x
            .sub((padding_left + border.left.get() as f32) as isize);
        if x > 0 {
            x_pixel_offset = x_pixel_offset.max(0) % self.render_metrics.cell_size.width;
        }

        self.last_mouse_coords = (x, y);

        let mut capture_mouse = false;

        match event.kind {
            WMEK::Release(ref press) => {
                self.current_mouse_capture = None;
                self.current_mouse_buttons.retain(|p| p != press);
                if press == &MousePress::Left && self.window_drag_position.take().is_some() {
                    // Completed a window drag
                    return;
                }
                if press == &MousePress::Left {
                    if let Some((item, _)) = self.dragging.take() {
                        // Completed a drag
                        match item.item_type {
                            UIItemType::SidebarResizeHandle => self.commit_sidebar_resize(context),
                            UIItemType::SidebarNode(id) => {
                                self.finish_sidebar_node_press(id, context)
                            }
                            UIItemType::PaneTab { .. } => {
                                self.finish_pane_tab_drag(&event, context)
                            }
                            _ => {}
                        }
                        return;
                    }
                }
            }

            WMEK::Press(ref press) => {
                capture_mouse = true;

                // A strip tab drag still pending here lost its release (see
                // cancel_pane_tab_drag); this press starts something new.
                if press == &MousePress::Left && self.cancel_pane_tab_drag() {
                    context.invalidate();
                }

                // Perform click counting
                let button = mouse_press_to_tmb(press);

                let click_position = ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                };

                let click = match self.last_mouse_click.take() {
                    None => LastMouseClick::new(button, click_position),
                    Some(click) => click.add(button, click_position),
                };
                self.last_mouse_click = Some(click);
                self.current_mouse_buttons.retain(|p| p != press);
                self.current_mouse_buttons.push(*press);
            }

            WMEK::Move => {
                if let Some(start) = self.window_drag_position.as_ref() {
                    // Dragging the window
                    // Compute the distance since the initial event
                    let delta_x = start.screen_coords.x - event.screen_coords.x;
                    let delta_y = start.screen_coords.y - event.screen_coords.y;

                    // Now compute a new window position.
                    // We don't have a direct way to get the position,
                    // but we can infer it by comparing the mouse coords
                    // with the screen coords in the initial event.
                    // This computes the original top_left position,
                    // and applies the total drag delta to it.
                    let top_left = ::window::ScreenPoint::new(
                        (start.screen_coords.x - start.coords.x) - delta_x,
                        (start.screen_coords.y - start.coords.y) - delta_y,
                    );
                    // and now tell the window to go there
                    context.set_window_position(top_left);
                    return;
                }

                if let Some((item, start_event)) = self.dragging.take() {
                    self.drag_ui_item(item, start_event, x, y, event, context);
                    return;
                }
            }
            _ => {}
        }

        let prior_ui_item = self.last_ui_item.clone();

        let ui_item = if matches!(self.current_mouse_capture, None | Some(MouseCapture::UI)) {
            let ui_item = self.resolve_ui_item(&event);

            match (self.last_ui_item.take(), &ui_item) {
                (Some(prior), Some(item)) => {
                    if prior != *item || !self.config.use_fancy_tab_bar {
                        self.leave_ui_item(&prior);
                        self.enter_ui_item(item);
                        context.invalidate();
                    }
                }
                (Some(prior), None) => {
                    self.leave_ui_item(&prior);
                    context.invalidate();
                }
                (None, Some(item)) => {
                    self.enter_ui_item(item);
                    context.invalidate();
                }
                (None, None) => {}
            }

            ui_item
        } else {
            None
        };

        if matches!(event.kind, WMEK::Press(_)) {
            // Any press that isn't on the context menu dismisses it. The menu
            // is drawn last, so it is what `resolve_ui_item` finds first when
            // the press really is on it.
            let on_menu = matches!(
                ui_item.as_ref().map(|item| &item.item_type),
                Some(UIItemType::SidebarMenuItem(_) | UIItemType::FilesMenuItem(_))
            );
            if !on_menu {
                self.close_sidebar_menu();
                self.close_files_menu();
            }
            // mouse_event_pane_strip judges a strip press against the press
            // before it. A press elsewhere must not leave an old strip press
            // behind for a later one to be judged against.
            let on_strip = matches!(
                ui_item.as_ref().map(|item| &item.item_type),
                Some(
                    UIItemType::PaneTab { .. }
                        | UIItemType::PaneTabClose { .. }
                        | UIItemType::PaneTabBar { .. }
                )
            );
            if !on_strip {
                self.last_strip_press = None;
            }
        }

        // A modal that owns the mouse lets nothing through to what is
        // underneath it: a click beside a dialog must not land in the
        // terminal or open a sidebar session.
        if let Some(modal) = self.get_modal() {
            let on_modal = matches!(
                ui_item.as_ref().map(|item| &item.item_type),
                Some(UIItemType::ModalItem(_))
            );
            if modal.captures_mouse() && !on_modal {
                return;
            }
        }

        if let Some(item) = ui_item.clone() {
            if capture_mouse {
                self.current_mouse_capture = Some(MouseCapture::UI);
            }
            self.mouse_event_ui_item(item, pane, y, event, context);
        } else if matches!(
            self.current_mouse_capture,
            None | Some(MouseCapture::TerminalPane(_))
        ) {
            // Only this half genuinely needs a pane. With none, a click in the
            // empty area simply has nothing to land on.
            if let Some(pane) = pane {
                self.mouse_event_terminal(
                    pane,
                    ClickPosition {
                        column: x,
                        row: y,
                        x_pixel_offset,
                        y_pixel_offset,
                    },
                    event,
                    context,
                    capture_mouse,
                );
            }
        }

        if prior_ui_item != ui_item {
            self.update_title_post_status();
        }
    }

    pub fn mouse_leave_impl(&mut self, context: &dyn WindowOps) {
        self.current_mouse_event = None;
        self.update_title();
        context.set_cursor(Some(MouseCursor::Arrow));
        context.invalidate();
    }

    fn drag_split(
        &mut self,
        mut item: UIItem,
        split: PositionedSplit,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        context: &dyn WindowOps,
    ) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let delta = match split.direction {
            SplitDirection::Horizontal => (x as isize).saturating_sub(split.left as isize),
            SplitDirection::Vertical => (y as isize).saturating_sub(split.top as isize),
        };

        if delta != 0 {
            tab.resize_split_by(split.index, delta);
            if let Some(split) = tab.iter_splits().into_iter().nth(split.index) {
                item.item_type = UIItemType::Split(split);
                context.invalidate();
            }
        }
        self.dragging.replace((item, start_event));
    }

    fn drag_scroll_thumb(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let dims = pane.get_dimensions();
        let current_viewport = self.get_viewport(pane.pane_id());

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let y_offset = top_bar_height + border.top.get() as f32;

        let from_top = start_event.coords.y.saturating_sub(item.y as isize);
        let effective_thumb_top = event
            .coords
            .y
            .saturating_sub(y_offset as isize + from_top)
            .max(0) as usize;

        // Convert thumb top into a row index by reversing the math
        // in ScrollHit::thumb
        let row = ScrollHit::thumb_top_to_scroll_top(
            effective_thumb_top,
            &*pane,
            current_viewport,
            self.dimensions.pixel_height.saturating_sub(
                y_offset as usize + border.bottom.get() + bottom_bar_height as usize,
            ),
            self.min_scroll_bar_height() as usize,
        );
        self.set_viewport(pane.pane_id(), Some(row), dims);
        context.invalidate();
        self.dragging.replace((item, start_event));
    }

    /// Follow a tab being dragged along the bar, moving it as the pointer goes.
    ///
    /// The tab moves during the drag rather than on release, so the bar itself
    /// shows where it will land. The press that started the drag activated it,
    /// so this moves the active tab, the same as MoveTab does.
    fn drag_tab(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let (active_idx, num_tabs) = match Mux::get().get_window(self.mux_window_id) {
            Some(window) => (window.get_active_idx(), window.len()),
            None => return,
        };

        let mut drawn_active = None;
        let mut others = vec![];
        for ui_item in &self.ui_items {
            if let UIItemType::TabBar(TabBarItem::Tab { tab_idx, active }) = ui_item.item_type {
                if active {
                    drawn_active = Some(tab_idx);
                } else {
                    others.push((ui_item.x as f32, ui_item.width as f32));
                }
            }
        }

        // Until the bar is redrawn after the last move -- or after the press
        // that activated this tab -- it still shows the old order, and
        // measuring against that would take a step the tab has already taken.
        let drawn_is_current = drawn_active == Some(active_idx) && others.len() + 1 == num_tabs;
        if drawn_is_current {
            let target = tab_drop_index(&others, event.coords.x as f32);
            if target != active_idx {
                self.move_tab(target).ok();
                context.invalidate();
            }
        }

        self.dragging.replace((item, start_event));
    }

    fn drag_ui_item(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match item.item_type {
            UIItemType::Split(split) => {
                self.drag_split(item, split, start_event, x, y, context);
            }
            UIItemType::ScrollThumb => {
                self.drag_scroll_thumb(item, start_event, event, context);
            }
            UIItemType::SidebarResizeHandle => {
                self.drag_sidebar_splitter(item, start_event, event, context);
            }
            UIItemType::SidebarNode(_) => {
                self.drag_sidebar_node(item, start_event, event, context);
            }
            UIItemType::TabBar(TabBarItem::Tab { .. }) => {
                self.drag_tab(item, start_event, event, context);
            }
            UIItemType::PaneTab { .. } => {
                self.drag_pane_tab(item, start_event, event, context);
            }
            _ => {
                log::error!("drag not implemented for {:?}", item);
            }
        }
    }

    fn mouse_event_ui_item(
        &mut self,
        item: UIItem,
        pane: Option<Arc<dyn Pane>>,
        _y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        self.last_ui_item.replace(item.clone());
        match item.item_type {
            UIItemType::TabBar(item) => {
                self.mouse_event_tab_bar(item, event, context);
            }
            // The scrollbar scrolls a pane, so without one there is nothing for
            // these to act on -- and nothing draws them in that state either.
            UIItemType::AboveScrollThumb => {
                if let Some(pane) = pane {
                    self.mouse_event_above_scroll_thumb(item, pane, event, context);
                }
            }
            UIItemType::ScrollThumb => {
                if let Some(pane) = pane {
                    self.mouse_event_scroll_thumb(item, pane, event, context);
                }
            }
            UIItemType::BelowScrollThumb => {
                if let Some(pane) = pane {
                    self.mouse_event_below_scroll_thumb(item, pane, event, context);
                }
            }
            UIItemType::Split(split) => {
                self.mouse_event_split(item, split, event, context);
            }
            UIItemType::CloseTab(idx) => {
                self.mouse_event_close_tab(idx, event, context);
            }
            UIItemType::SidebarBackground
            | UIItemType::SidebarNode(_)
            | UIItemType::SidebarSearch
            | UIItemType::SidebarResizeHandle
            | UIItemType::SidebarMenuItem(_)
            | UIItemType::SidebarHeaderAction(_)
            | UIItemType::SidebarCollapseToggle
            | UIItemType::SidebarPageTab(_) => {
                self.mouse_event_sidebar(item, event, context);
            }
            UIItemType::FilesRow(_)
            | UIItemType::FilesToolbar(_)
            | UIItemType::FilesPath
            | UIItemType::FilesFollow
            | UIItemType::FilesMenuItem(_)
            | UIItemType::FilesBackground => {
                self.mouse_event_files(item, event, context);
            }
            UIItemType::PaneTab { .. }
            | UIItemType::PaneTabClose { .. }
            | UIItemType::PaneTabBar { .. } => {
                self.mouse_event_pane_strip(item, event, context);
            }
            UIItemType::ModalItem(index) => {
                if let Some(modal) = self.get_modal() {
                    modal.ui_item_event(index, &event, self);
                }
            }
        }
    }

    fn mouse_event_pane_strip(
        &mut self,
        ui_item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let item = ui_item.item_type.clone();
        if let WMEK::Press(press) = event.kind {
            // Not the streak in last_mouse_click: that also wants both
            // presses in the same cell, and the cell a pixel maps to changes
            // when the active pane starts or stops grabbing the mouse, as
            // the close confirmation does. Two presses at one pixel could
            // then count as two single clicks and close twice.
            let now = Instant::now();
            // Every press either records itself here or clears this, so it
            // is the previous press, if that one was on a strip.
            let prev = self.last_strip_press.take().filter(|prev| {
                now.duration_since(prev.at) <= Duration::from_millis(CLICK_INTERVAL)
            });
            let (action, memory) = strip_press(
                &item,
                press,
                prev.as_ref().map(|prev| (&prev.item, prev.action)),
            );
            log::trace!("strip press {press:?} on {item:?}: {action:?}");
            self.last_strip_press = Some(StripPress {
                item: item.clone(),
                action: memory,
                at: now,
            });
            if action != StripClickAction::Nothing {
                self.perform_strip_click_action(&item, action, context);
                // Only a press that did something can start a drag. A
                // swallowed one is the tail of a close or spawn whose
                // re-layout slid some other tab under the pointer.
                if press == MousePress::Left && matches!(item, UIItemType::PaneTab { .. }) {
                    self.start_pane_tab_drag(ui_item, event);
                }
            }
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    /// Each action changes the active pane at most once.
    fn perform_strip_click_action(
        &mut self,
        item: &UIItemType,
        action: StripClickAction,
        context: &dyn WindowOps,
    ) {
        let item_tab_id = match item {
            UIItemType::PaneTab { tab_id, .. }
            | UIItemType::PaneTabClose { tab_id, .. }
            | UIItemType::PaneTabBar { tab_id, .. } => *tab_id,
            _ => return,
        };
        let tab = match Mux::get().get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        // Hit regions are registered while painting, so they describe the
        // previous frame; if another tab has become active since, they are
        // stale and their ids may not belong to this tab.
        if tab.tab_id() != item_tab_id {
            return;
        }
        match action {
            StripClickAction::Activate(pane_id) => {
                if let Some(pane) = tab_member(&tab, pane_id) {
                    tab.set_active_pane(&pane);
                }
            }
            StripClickAction::Close(pane_id) => self.close_pane_by_id(pane_id, true),
            // The strip names its leaf by the real leaf index, which is what
            // visible_pane_of_leaf takes, also while zoomed.
            StripClickAction::ActivateLeaf(leaf) => {
                if let Some(pane) = tab.visible_pane_of_leaf(leaf) {
                    tab.set_active_pane(&pane);
                }
            }
            StripClickAction::SpawnInLeaf(leaf) => {
                if let Some(pane) = tab.visible_pane_of_leaf(leaf) {
                    self.spawn_command(
                        &SpawnCommand::default(),
                        SpawnWhere::PaneStack(pane.pane_id()),
                    );
                }
            }
            StripClickAction::Nothing => {}
        }
        context.invalidate();
    }

    pub fn mouse_event_close_tab(
        &mut self,
        idx: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                log::debug!("Should close tab {}", idx);
                self.close_specific_tab(idx, true);
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn do_new_tab_button_click(&mut self, button: MousePress) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };
        let action = match button {
            MousePress::Left => Some(KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain)),
            MousePress::Right => Some(KeyAssignment::ShowLauncher),
            MousePress::Middle => None,
        };

        async fn dispatch_new_tab_button(
            lua: Option<Rc<mlua::Lua>>,
            window: GuiWin,
            pane: MuxPane,
            button: MousePress,
            action: Option<KeyAssignment>,
        ) -> anyhow::Result<()> {
            let default_action = match lua {
                Some(lua) => {
                    let args = lua.pack_multi((
                        window.clone(),
                        pane,
                        format!("{button:?}"),
                        action.clone(),
                    ))?;
                    config::lua::emit_event(&lua, ("new-tab-button-click".to_string(), args))
                        .await
                        .map_err(|e| {
                            log::error!("while processing new-tab-button-click event: {:#}", e);
                            e
                        })?
                }
                None => true,
            };
            if let (true, Some(assignment)) = (default_action, action) {
                window.window.notify(TermWindowNotif::PerformAssignment {
                    pane_id: pane.0,
                    assignment,
                    tx: None,
                });
            }
            Ok(())
        }
        let window = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());
        promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
            dispatch_new_tab_button(lua, window, pane, button, action)
        }))
        .detach();
    }

    pub fn mouse_event_tab_bar(
        &mut self,
        item: TabBarItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    self.activate_tab(tab_idx as isize).ok();
                    // Moving with the button still down drags the tab along
                    // the bar; see drag_tab.
                    if let Some(ui_item) = self.last_ui_item.clone() {
                        self.dragging.replace((ui_item, event.clone()));
                    }
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Left);
                }
                TabBarItem::FeishuButton => {
                    // The icon has to say two things at once -- service state and
                    // whether this pane is mirrored -- so clicking it opens the
                    // place where both are spelled out, rather than guessing
                    // which one the user meant to change.
                    self.show_feishu_panel();
                }
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    let maximized = self
                        .window_state
                        .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                    if let Some(ref window) = self.window {
                        if self.config.window_decorations
                            == WindowDecorations::INTEGRATED_BUTTONS | WindowDecorations::RESIZE
                        {
                            if self.last_mouse_click.as_ref().map(|c| c.streak) == Some(2) {
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                        }
                    }
                    // Potentially starting a drag by the tab bar
                    if !maximized {
                        self.window_drag_position.replace(event.clone());
                    }
                    context.request_drag_move();
                }
                TabBarItem::WindowButton(button) => {
                    use window::IntegratedTitleButton as Button;
                    if let Some(ref window) = self.window {
                        match button {
                            Button::Hide => window.hide(),
                            Button::Maximize => {
                                let maximized = self
                                    .window_state
                                    .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                            Button::Close => self.close_requested(&window.clone()),
                        }
                    }
                }
            },
            WMEK::Press(MousePress::Middle) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    self.close_specific_tab(tab_idx, true);
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Middle);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::FeishuButton
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Press(MousePress::Right) => match item {
                TabBarItem::Tab { .. } => {
                    self.show_tab_navigator();
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Right);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::FeishuButton
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Move => match item {
                // Not a draggable part of the title bar.
                TabBarItem::FeishuButton => {}
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    context.set_window_drag_position(event.screen_coords);
                }
                TabBarItem::WindowButton(window::IntegratedTitleButton::Maximize) => {
                    let item = self.last_ui_item.clone().unwrap();
                    let bounds: ::window::ScreenRect = euclid::rect(
                        item.x as isize - (event.coords.x as isize - event.screen_coords.x),
                        item.y as isize - (event.coords.y as isize - event.screen_coords.y),
                        item.width as isize,
                        item.height as isize,
                    );
                    context.set_maximize_button_position(bounds);
                }
                TabBarItem::WindowButton(_)
                | TabBarItem::Tab { .. }
                | TabBarItem::NewTabButton { .. } => {}
            },
            WMEK::VertWheel(n) => {
                if self.config.mouse_wheel_scrolls_tabs {
                    self.activate_tab_relative(if n < 1 { 1 } else { -1 }, true)
                        .ok();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_above_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page up
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_sub(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_below_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page down
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_add(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_scroll_thumb(
        &mut self,
        item: UIItem,
        _pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            // Start a scroll drag
            // self.scroll_drag_start = Some(from_top);
            self.dragging = Some((item, event));
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_split(
        &mut self,
        item: UIItem,
        split: PositionedSplit,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(match &split.direction {
            SplitDirection::Horizontal => MouseCursor::SizeLeftRight,
            SplitDirection::Vertical => MouseCursor::SizeUpDown,
        }));

        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    fn mouse_event_terminal(
        &mut self,
        mut pane: Arc<dyn Pane>,
        position: ClickPosition,
        event: MouseEvent,
        context: &dyn WindowOps,
        capture_mouse: bool,
    ) {
        let mut is_click_to_focus_pane = false;

        let ClickPosition {
            mut column,
            mut row,
            mut x_pixel_offset,
            mut y_pixel_offset,
        } = position;

        let is_already_captured = matches!(
            self.current_mouse_capture,
            Some(MouseCapture::TerminalPane(_))
        );

        for pos in self.get_panes_to_render() {
            if !is_already_captured
                && row >= pos.top as i64
                && row <= (pos.top + pos.height) as i64
                && column >= pos.left
                && column <= pos.left + pos.width
            {
                if pane.pane_id() != pos.pane.pane_id() {
                    // We're over a pane that isn't active
                    match &event.kind {
                        WMEK::Press(_) => {
                            let mux = Mux::get();
                            mux.get_active_tab_for_window(self.mux_window_id)
                                .map(|tab| tab.set_active_idx(pos.index));

                            pane = Arc::clone(&pos.pane);
                            is_click_to_focus_pane = true;
                        }
                        WMEK::Move => {
                            if self.config.pane_focus_follows_mouse {
                                let mux = Mux::get();
                                mux.get_active_tab_for_window(self.mux_window_id)
                                    .map(|tab| tab.set_active_idx(pos.index));

                                pane = Arc::clone(&pos.pane);
                                context.invalidate();
                            }
                        }
                        WMEK::Release(_) | WMEK::HorzWheel(_) => {}
                        WMEK::VertWheel(_) => {
                            // Let wheel events route to the hovered pane,
                            // even if it doesn't have focus
                            pane = Arc::clone(&pos.pane);
                            context.invalidate();
                        }
                    }
                }
                column = column.saturating_sub(pos.left);
                row = row.saturating_sub(pos.top as i64);
                break;
            } else if is_already_captured && pane.pane_id() == pos.pane.pane_id() {
                column = column.saturating_sub(pos.left);
                row = row.saturating_sub(pos.top as i64).max(0);

                if position.column < pos.left {
                    x_pixel_offset -= self.render_metrics.cell_size.width
                        * (pos.left as isize - position.column as isize);
                }
                if position.row < pos.top as i64 {
                    y_pixel_offset -= self.render_metrics.cell_size.height
                        * (pos.top as isize - position.row as isize);
                }

                break;
            }
        }

        if capture_mouse {
            self.current_mouse_capture = Some(MouseCapture::TerminalPane(pane.pane_id()));
        }

        let is_focused = if let Some(focused) = self.focused.as_ref() {
            !self.config.swallow_mouse_click_on_window_focus
                || (focused.elapsed() > Duration::from_millis(200))
        } else {
            false
        };

        if self.focused.is_some() && !is_focused {
            if matches!(&event.kind, WMEK::Press(_))
                && self.config.swallow_mouse_click_on_window_focus
            {
                // Entering click to focus state
                self.is_click_to_focus_window = true;
                context.invalidate();
                log::trace!("enter click to focus");
                return;
            }
        }
        if self.is_click_to_focus_window && matches!(&event.kind, WMEK::Release(_)) {
            // Exiting click to focus state
            self.is_click_to_focus_window = false;
            context.invalidate();
            log::trace!("exit click to focus");
            return;
        }

        let allow_action = if self.is_click_to_focus_window || !is_focused {
            matches!(&event.kind, WMEK::VertWheel(_) | WMEK::HorzWheel(_))
        } else {
            true
        };

        log::trace!(
            "is_focused={} allow_action={} event={:?}",
            is_focused,
            allow_action,
            event
        );

        let dims = pane.get_dimensions();
        let stable_row = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top)
            + row as StableRowIndex;

        self.pane_state(pane.pane_id())
            .mouse_terminal_coords
            .replace((
                ClickPosition {
                    column,
                    row,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                stable_row,
            ));

        pane.apply_hyperlinks(stable_row..stable_row + 1, &self.config.hyperlink_rules);

        struct FindCurrentLink {
            current: Option<Arc<Hyperlink>>,
            stable_row: StableRowIndex,
            column: usize,
        }

        impl WithPaneLines for FindCurrentLink {
            fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                if stable_top == self.stable_row {
                    if let Some(line) = lines.get(0) {
                        if let Some(cell) = line.get_cell(self.column) {
                            self.current = cell.attrs().hyperlink().cloned();
                        }
                    }
                }
            }
        }

        let mut find_link = FindCurrentLink {
            current: None,
            stable_row,
            column,
        };
        pane.with_lines_mut(stable_row..stable_row + 1, &mut find_link);
        let new_highlight = find_link.current;

        match (self.current_highlight.as_ref(), new_highlight) {
            (Some(old_link), Some(new_link)) if Arc::ptr_eq(&old_link, &new_link) => {
                // Unchanged
            }
            (None, None) => {
                // Unchanged
            }
            (_, rhs) => {
                // We're hovering over a different URL, so invalidate and repaint
                // so that we render the underline correctly
                self.current_highlight = rhs;
                context.invalidate();
            }
        };

        let outside_window = event.coords.x < 0
            || event.coords.x as usize > self.dimensions.pixel_width
            || event.coords.y < 0
            || event.coords.y as usize > self.dimensions.pixel_height;

        context.set_cursor(Some(if self.current_highlight.is_some() {
            // When hovering over a hyperlink, show an appropriate
            // mouse cursor to give the cue that it is clickable
            MouseCursor::Hand
        } else if pane.is_mouse_grabbed() || outside_window {
            MouseCursor::Arrow
        } else {
            MouseCursor::Text
        }));

        let event_trigger_type = match &event.kind {
            WMEK::Press(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Down {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Release(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Up {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Move => {
                if !self.current_mouse_buttons.is_empty() {
                    if let Some(LastMouseClick { streak, button, .. }) =
                        self.last_mouse_click.as_ref()
                    {
                        if Some(*button)
                            == self.current_mouse_buttons.last().map(mouse_press_to_tmb)
                        {
                            Some(MouseEventTrigger::Drag {
                                streak: *streak,
                                button: *button,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            WMEK::VertWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelUp(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelDown(-amount as usize),
                },
            }),
            WMEK::HorzWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelLeft(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelRight(-amount as usize),
                },
            }),
        };

        if allow_action {
            if let Some(mut event_trigger_type) = event_trigger_type {
                self.current_event = Some(event_trigger_type.to_dynamic());
                let mut modifiers = event.modifiers;

                // Since we use shift to force assessing the mouse bindings, pretend
                // that shift is not one of the mods when the mouse is grabbed.
                let mut mouse_reporting = pane.is_mouse_grabbed();
                if mouse_reporting {
                    if modifiers.contains(self.config.bypass_mouse_reporting_modifiers) {
                        modifiers.remove(self.config.bypass_mouse_reporting_modifiers);
                        mouse_reporting = false;
                    }
                }

                if mouse_reporting {
                    // If they were scrolled back prior to launching an
                    // application that captures the mouse, then mouse based
                    // scrolling assignments won't have any effect.
                    // Ensure that we scroll to the bottom if they try to
                    // use the mouse so that things are less surprising
                    self.scroll_to_bottom(&pane);
                }

                // normalize delta and streak to make mouse assignment
                // easier to wrangle
                match event_trigger_type {
                    MouseEventTrigger::Down {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Up {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Drag {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    } => {
                        *streak = 1;
                        *delta = 1;
                    }
                    _ => {}
                };

                let mouse_mods = config::MouseEventTriggerMods {
                    mods: modifiers,
                    mouse_reporting,
                    alt_screen: if pane.is_alt_screen_active() {
                        MouseEventAltScreen::True
                    } else {
                        MouseEventAltScreen::False
                    },
                };

                if let Some(action) = self.input_map.lookup_mouse(event_trigger_type, mouse_mods) {
                    self.perform_key_assignment(&pane, &action).ok();
                    return;
                }
            }
        }

        let mouse_event = wezterm_term::MouseEvent {
            kind: match event.kind {
                WMEK::Move => TMEK::Move,
                WMEK::VertWheel(_) | WMEK::HorzWheel(_) | WMEK::Press(_) => TMEK::Press,
                WMEK::Release(_) => TMEK::Release,
            },
            button: match event.kind {
                WMEK::Release(ref press) | WMEK::Press(ref press) => mouse_press_to_tmb(press),
                WMEK::Move => {
                    if event.mouse_buttons == WMB::LEFT {
                        TMB::Left
                    } else if event.mouse_buttons == WMB::RIGHT {
                        TMB::Right
                    } else if event.mouse_buttons == WMB::MIDDLE {
                        TMB::Middle
                    } else {
                        TMB::None
                    }
                }
                WMEK::VertWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelUp(amount as usize)
                    } else {
                        TMB::WheelDown((-amount) as usize)
                    }
                }
                WMEK::HorzWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelLeft(amount as usize)
                    } else {
                        TMB::WheelRight((-amount) as usize)
                    }
                }
            },
            x: column,
            y: row,
            x_pixel_offset,
            y_pixel_offset,
            modifiers: event.modifiers,
        };

        if allow_action
            && !(self.config.swallow_mouse_click_on_pane_focus && is_click_to_focus_pane)
        {
            pane.mouse_event(mouse_event).ok();
        }

        match event.kind {
            WMEK::Move => {}
            _ => {
                context.invalidate();
            }
        }
    }
}

fn mouse_press_to_tmb(press: &MousePress) -> TMB {
    match press {
        MousePress::Left => TMB::Left,
        MousePress::Right => TMB::Right,
        MousePress::Middle => TMB::Middle,
    }
}

/// What a press on a pane stack strip asks for. Kept apart from the window
/// so that the mapping from button, target and previous press can be tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StripClickAction {
    /// Bring this member to the front of its stack and focus it.
    Activate(PaneId),
    /// Close this member, asking first if it wants to be asked.
    Close(PaneId),
    /// Focus the visible member of the leaf with this index.
    ActivateLeaf(usize),
    /// Spawn a new member into the leaf with this index.
    SpawnInLeaf(usize),
    Nothing,
}

impl StripClickAction {
    /// A close or a spawn re-lays the strip out under the cursor: a tab
    /// goes, comes, moves to the front or loses its unseen marker, so the
    /// next press of the same double or triple click lands on whatever slid
    /// there. Such presses are the rest of that click, not new ones; acting
    /// on them would close a second pane, or open a second confirmation
    /// that cancels the first. (The rule that an Activate swallows an
    /// immediately following Close lives in `strip_press`.)
    fn swallows_followers(self) -> bool {
        matches!(
            self,
            StripClickAction::Close(_) | StripClickAction::SpawnInLeaf(_)
        )
    }
}

/// The previous press on a pane stack strip.
pub(super) struct StripPress {
    item: UIItemType,
    /// See `strip_press`.
    action: StripClickAction,
    at: Instant,
}

/// `prev` is the previous strip press and what it is remembered as, given
/// only when it came within `CLICK_INTERVAL`; a press after a longer pause
/// is a click of its own.
fn strip_click_action(
    item: &UIItemType,
    press: MousePress,
    prev: Option<(&UIItemType, StripClickAction)>,
) -> StripClickAction {
    if prev.map_or(false, |(_, action)| action.swallows_followers()) {
        return StripClickAction::Nothing;
    }
    match (item, press) {
        (UIItemType::PaneTab { pane_id, .. }, MousePress::Left) => {
            StripClickAction::Activate(*pane_id)
        }
        // The close button sits inside its tab, so a middle click on it is
        // still a middle click on the tab.
        (UIItemType::PaneTab { pane_id, .. }, MousePress::Middle)
        | (UIItemType::PaneTabClose { pane_id, .. }, MousePress::Left | MousePress::Middle) => {
            StripClickAction::Close(*pane_id)
        }
        (UIItemType::PaneTabBar { leaf, .. }, MousePress::Left) => match prev {
            // The first click of the pair has already focused the leaf. Only
            // a pair on this same stretch of empty strip is a double click
            // on it.
            Some((prev_item, StripClickAction::ActivateLeaf(_))) if prev_item == item => {
                StripClickAction::SpawnInLeaf(*leaf)
            }
            _ => StripClickAction::ActivateLeaf(*leaf),
        },
        _ => StripClickAction::Nothing,
    }
}

/// What a strip press is remembered as, for judging the press after it. A
/// press swallowed after a close or a spawn passes that action on, so the
/// whole run of quick presses is swallowed, not just the one right after
/// it: the third press of a triple click on × must not close a second pane.
/// The rule that an Activate swallows an immediately following Close lives in
/// `strip_press`.
fn strip_press_memory(
    prev: Option<StripClickAction>,
    action: StripClickAction,
) -> StripClickAction {
    match prev {
        Some(prev) if prev.swallows_followers() => prev,
        _ => action,
    }
}

/// What to do for a strip press, and what to remember it as for the press
/// after it. One function so the caller cannot remember something other than
/// what the tests checked.
///
/// A close right after an Activate is swallowed, whichever pane it is for:
/// activating a tab that shows an unseen marker drops the marker and narrows
/// the tab, and the re-layout can put a × (this pane's or another's) under
/// the pointer, where it would close a pane the user only meant to select.
/// Looks at the action, not the item, and the swallowed press keeps
/// remembering the Activate, so a third press is swallowed as well.
fn strip_press(
    item: &UIItemType,
    press: MousePress,
    prev: Option<(&UIItemType, StripClickAction)>,
) -> (StripClickAction, StripClickAction) {
    let action = strip_click_action(item, press, prev);
    match (prev, action) {
        (Some((_, prev_action @ StripClickAction::Activate(_))), StripClickAction::Close(_)) => {
            (StripClickAction::Nothing, prev_action)
        }
        _ => (action, strip_press_memory(prev.map(|(_, a)| a), action)),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use mux::tab::TabId;

    fn tab_item(pane_id: PaneId) -> UIItemType {
        UIItemType::PaneTab { tab_id: 1, pane_id }
    }

    fn close_item(pane_id: PaneId) -> UIItemType {
        UIItemType::PaneTabClose { tab_id: 1, pane_id }
    }

    fn bar_item(tab_id: TabId, leaf: usize) -> UIItemType {
        UIItemType::PaneTabBar { tab_id, leaf }
    }

    /// Feeds presses that each come within CLICK_INTERVAL of the one before
    /// through the same steps as mouse_event_pane_strip.
    fn quick_run(presses: &[(&UIItemType, MousePress)]) -> Vec<StripClickAction> {
        let mut prev: Option<(UIItemType, StripClickAction)> = None;
        let mut actions = vec![];
        for &(item, press) in presses {
            let (action, memory) = strip_press(item, press, prev.as_ref().map(|(i, a)| (i, *a)));
            prev = Some((item.clone(), memory));
            actions.push(action);
        }
        actions
    }

    #[test]
    fn strip_click_actions_follow_the_spec_table() {
        use MousePress::{Left, Middle, Right};
        use StripClickAction::*;
        // What slides under the cursor once pane 7 is closed is pane 8.
        let (tab7, close7, tab8, close8) = (tab_item(7), close_item(7), tab_item(8), close_item(8));
        let bar = bar_item(1, 3);
        let other_bar = bar_item(1, 4);
        let other_tabs_bar = bar_item(2, 3);
        let strip = [&tab7, &close7, &tab8, &close8, &bar, &other_bar];

        // A single click: no previous press, or none within CLICK_INTERVAL.
        assert_eq!(strip_press(&tab7, Left, None).0, Activate(7));
        assert_eq!(strip_press(&close7, Left, None).0, Close(7));
        // A middle click on the close button is a middle click on its tab.
        assert_eq!(strip_press(&tab7, Middle, None).0, Close(7));
        assert_eq!(strip_press(&close7, Middle, None).0, Close(7));
        assert_eq!(strip_press(&bar, Left, None).0, ActivateLeaf(3));
        assert_eq!(strip_press(&bar, Middle, None).0, Nothing);
        for item in strip {
            assert_eq!(strip_press(item, Right, None).0, Nothing, "{item:?}");
        }
        // Items that are not part of a strip are not handled here.
        assert_eq!(strip_press(&UIItemType::ScrollThumb, Left, None).0, Nothing);

        // Right after a close or a spawn, no strip press does anything,
        // whatever it hits and whatever item the close or spawn came from.
        for prev_action in [Close(7), SpawnInLeaf(3)] {
            for prev_item in strip {
                for item in strip {
                    for press in [Left, Middle, Right] {
                        assert_eq!(
                            strip_press(item, press, Some((prev_item, prev_action))).0,
                            Nothing,
                            "{press:?} on {item:?} after {prev_action:?} on {prev_item:?}"
                        );
                    }
                }
            }
        }

        // A close that comes more than CLICK_INTERVAL after a previous close
        // closes too: the caller passes no previous press then.
        assert_eq!(strip_press(&close8, Left, None).0, Close(8));
        assert_eq!(strip_press(&tab8, Middle, None).0, Close(8));

        // Right after a press that neither closed nor spawned, the remaining
        // mappings hold as for a single click.
        for prev in [(&bar, ActivateLeaf(3)), (&close7, Nothing), (&bar, Nothing)] {
            assert_eq!(strip_press(&close7, Left, Some(prev)).0, Close(7));
            assert_eq!(strip_press(&close7, Middle, Some(prev)).0, Close(7));
            assert_eq!(strip_press(&tab7, Middle, Some(prev)).0, Close(7));
            assert_eq!(strip_press(&tab8, Left, Some(prev)).0, Activate(8));
            assert_eq!(strip_press(&bar, Middle, Some(prev)).0, Nothing);
            for item in strip {
                assert_eq!(strip_press(item, Right, Some(prev)).0, Nothing);
            }
        }

        // A close right after an Activate is swallowed, whichever pane it is for.
        for close in [&close7, &close8] {
            for press in [Left, Middle] {
                assert_eq!(
                    strip_press(close, press, Some((&tab7, Activate(7)))).0,
                    Nothing
                );
            }
        }
        assert_eq!(
            strip_press(&tab7, Middle, Some((&tab7, Activate(7)))).0,
            Nothing
        );
        assert_eq!(
            strip_press(&tab8, Left, Some((&tab7, Activate(7)))).0,
            Activate(8)
        );
        assert_eq!(
            strip_press(&bar, Middle, Some((&tab7, Activate(7)))).0,
            Nothing
        );
        for item in strip {
            assert_eq!(
                strip_press(item, Right, Some((&tab7, Activate(7)))).0,
                Nothing,
                "{item:?}"
            );
        }

        // A left press on empty strip spawns only as the second press of a
        // pair on the same stretch of the same tab's strip, whose first
        // press focused the leaf.
        assert_eq!(
            strip_press(&bar, Left, Some((&bar, ActivateLeaf(3)))).0,
            SpawnInLeaf(3)
        );
        // Same stretch, but the first press did nothing (a middle click).
        assert_eq!(
            strip_press(&bar, Left, Some((&bar, Nothing))).0,
            ActivateLeaf(3)
        );
        // First press on another leaf's strip, another tab's strip, a tab.
        assert_eq!(
            strip_press(&bar, Left, Some((&other_bar, ActivateLeaf(4)))).0,
            ActivateLeaf(3)
        );
        assert_eq!(
            strip_press(&bar, Left, Some((&other_tabs_bar, ActivateLeaf(3)))).0,
            ActivateLeaf(3)
        );
        assert_eq!(
            strip_press(&bar, Left, Some((&tab7, Activate(7)))).0,
            ActivateLeaf(3)
        );
    }

    #[test]
    fn activate_then_close_same_pane_is_swallowed() {
        let tab = UIItemType::PaneTab {
            tab_id: 1,
            pane_id: 7,
        };
        let close = UIItemType::PaneTabClose {
            tab_id: 1,
            pane_id: 7,
        };
        let other_close = UIItemType::PaneTabClose {
            tab_id: 1,
            pane_id: 8,
        };
        let (first, mem) = strip_press(&tab, MousePress::Left, None);
        assert_eq!(first, StripClickAction::Activate(7));
        let (second, mem2) = strip_press(&close, MousePress::Left, Some((&tab, mem)));
        assert_eq!(second, StripClickAction::Nothing);
        // The swallowed press keeps the Activate, so a third press on the same × is swallowed too.
        assert_eq!(
            strip_press(&close, MousePress::Left, Some((&close, mem2))).0,
            StripClickAction::Nothing
        );
        // Activating re-lays out the strip, so another pane's × may now be under the pointer.
        assert_eq!(
            strip_press(&other_close, MousePress::Left, Some((&tab, mem))).0,
            StripClickAction::Nothing
        );
        assert_eq!(
            strip_press(&tab, MousePress::Middle, Some((&tab, mem))).0,
            StripClickAction::Nothing
        );
        // Without a recent Activate a close still closes.
        assert_eq!(
            strip_press(&other_close, MousePress::Left, None).0,
            StripClickAction::Close(8)
        );
    }

    #[test]
    fn quick_strip_presses_after_a_close_or_spawn_stay_swallowed() {
        use MousePress::{Left, Middle};
        use StripClickAction::*;
        let (tab7, close7, tab8, close8) = (tab_item(7), close_item(7), tab_item(8), close_item(8));
        let bar = bar_item(1, 3);
        // The pane that a double click on the empty strip spawns.
        let close9 = close_item(9);

        // Double click on ×: the second press hits the same ×, or the next
        // tab's × that slid under it.
        assert_eq!(
            quick_run(&[(&close7, Left), (&close7, Left)]),
            [Close(7), Nothing]
        );
        assert_eq!(
            quick_run(&[(&close7, Left), (&close8, Left)]),
            [Close(7), Nothing]
        );
        // A tab with an unseen marker narrows when the close brings it to
        // the front, and the second press hits the neighbour's body.
        assert_eq!(
            quick_run(&[(&close7, Left), (&tab8, Left)]),
            [Close(7), Nothing]
        );
        // The second press hits the strip space that the close uncovered.
        assert_eq!(
            quick_run(&[(&close7, Left), (&bar, Left)]),
            [Close(7), Nothing]
        );
        // Triple click on ×: the third press is swallowed too.
        assert_eq!(
            quick_run(&[(&close7, Left), (&close7, Left), (&close7, Left)]),
            [Close(7), Nothing, Nothing]
        );
        assert_eq!(
            quick_run(&[(&close7, Left), (&close8, Left), (&close8, Left)]),
            [Close(7), Nothing, Nothing]
        );
        // Middle double click on a tab.
        assert_eq!(
            quick_run(&[(&tab7, Middle), (&tab8, Middle)]),
            [Close(7), Nothing]
        );
        // Double click on the empty strip spawns one pane.
        assert_eq!(
            quick_run(&[(&bar, Left), (&bar, Left)]),
            [ActivateLeaf(3), SpawnInLeaf(3)]
        );
        // Triple click on the empty strip spawns one pane, and the third
        // press does not close the new pane whose × appeared under it.
        assert_eq!(
            quick_run(&[(&bar, Left), (&bar, Left), (&close9, Left)]),
            [ActivateLeaf(3), SpawnInLeaf(3), Nothing]
        );
        assert_eq!(
            quick_run(&[(&bar, Left), (&bar, Left), (&bar, Left), (&bar, Left)]),
            [ActivateLeaf(3), SpawnInLeaf(3), Nothing, Nothing]
        );
        // Quick presses that neither close nor spawn swallow no activation.
        assert_eq!(
            quick_run(&[(&tab7, Left), (&tab8, Left), (&tab7, Left)]),
            [Activate(7), Activate(8), Activate(7)]
        );
        // But a close right after an Activate is swallowed.
        assert_eq!(
            quick_run(&[(&tab7, Left), (&tab8, Left), (&close8, Left)]),
            [Activate(7), Activate(8), Nothing]
        );
    }
}
