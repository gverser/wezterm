use crate::pane::CloseReason;
use crate::{Mux, MuxNotification, Tab, TabId};
use config::GuiPosition;
use std::sync::Arc;

static WIN_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type WindowId = usize;

pub struct Window {
    id: WindowId,
    tabs: Vec<Arc<Tab>>,
    active: usize,
    last_active: Option<TabId>,
    workspace: String,
    title: String,
    initial_position: Option<GuiPosition>,
    /// Set when this window is deliberately being kept with no tabs in it, so
    /// that the GUI can show just the session sidebar and wait for the user to
    /// pick what to open. Without it, prune_dead_windows clears the window away
    /// the moment the startup Activity guard is dropped, and the mux then
    /// reports itself empty and the GUI exits.
    retain_when_empty: bool,
}

impl Window {
    pub fn new(workspace: Option<String>, initial_position: Option<GuiPosition>) -> Self {
        Self {
            id: WIN_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed),
            tabs: vec![],
            active: 0,
            last_active: None,
            title: String::new(),
            workspace: workspace.unwrap_or_else(|| Mux::get().active_workspace()),
            initial_position,
            retain_when_empty: false,
        }
    }

    /// Whether this window should survive having no tabs. Only the empty
    /// window the GUI opens at startup sets this; every other window is dead
    /// once its last tab goes away.
    pub fn retain_when_empty(&self) -> bool {
        self.retain_when_empty
    }

    pub fn set_retain_when_empty(&mut self, retain: bool) {
        self.retain_when_empty = retain;
    }

    pub fn get_initial_position(&self) -> &Option<GuiPosition> {
        &self.initial_position
    }

    pub fn get_workspace(&self) -> &str {
        &self.workspace
    }

    pub fn set_title(&mut self, title: &str) {
        if self.title != title {
            self.title = title.to_string();
            Mux::try_get().map(|mux| {
                mux.notify(MuxNotification::WindowTitleChanged {
                    window_id: self.id,
                    title: title.to_string(),
                })
            });
        }
    }

    pub fn get_title(&self) -> &str {
        &self.title
    }

    pub fn set_workspace(&mut self, workspace: &str) {
        if workspace == self.workspace {
            return;
        }
        self.workspace = workspace.to_string();
        Mux::get().notify(MuxNotification::WindowWorkspaceChanged(self.id));
    }

    pub fn window_id(&self) -> WindowId {
        self.id
    }

    fn check_that_tab_isnt_already_in_window(&self, tab: &Arc<Tab>) {
        for t in &self.tabs {
            assert_ne!(t.tab_id(), tab.tab_id(), "tab already added to this window");
        }
    }

    fn invalidate(&self) {
        let mux = Mux::get();
        mux.notify(MuxNotification::WindowInvalidated(self.id));
    }

    pub fn insert(&mut self, index: usize, tab: &Arc<Tab>) {
        self.check_that_tab_isnt_already_in_window(tab);
        self.tabs.insert(index, Arc::clone(tab));
        self.invalidate();
    }

    pub fn push(&mut self, tab: &Arc<Tab>) {
        self.check_that_tab_isnt_already_in_window(tab);
        self.tabs.push(Arc::clone(tab));
        self.invalidate();
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn get_by_idx(&self, idx: usize) -> Option<&Arc<Tab>> {
        self.tabs.get(idx)
    }

    pub fn can_close_without_prompting(&self) -> bool {
        for tab in &self.tabs {
            if !tab.can_close_without_prompting(CloseReason::Window) {
                return false;
            }
        }
        true
    }

    pub fn idx_by_id(&self, id: TabId) -> Option<usize> {
        for (idx, t) in self.tabs.iter().enumerate() {
            if t.tab_id() == id {
                return Some(idx);
            }
        }
        None
    }

    fn fixup_active_tab_after_removal(&mut self, active: Option<Arc<Tab>>) {
        let len = self.tabs.len();
        if let Some(active) = active {
            for (idx, tab) in self.tabs.iter().enumerate() {
                if tab.tab_id() == active.tab_id() {
                    self.set_active_without_saving(idx);
                    return;
                }
            }
        }

        if len > 0 && self.active >= len {
            self.set_active_without_saving(len - 1);
        } else {
            self.invalidate();
        }
    }

    pub fn remove_by_idx(&mut self, idx: usize) -> Arc<Tab> {
        self.invalidate();
        let active = self.get_active().map(Arc::clone);
        self.do_remove_idx(idx, active)
    }

    pub fn remove_by_id(&mut self, id: TabId) {
        let active = self.get_active().map(Arc::clone);
        if let Some(idx) = self.idx_by_id(id) {
            self.do_remove_idx(idx, active);
        }
    }

    fn do_remove_idx(&mut self, idx: usize, active: Option<Arc<Tab>>) -> Arc<Tab> {
        if let (Some(active), Some(removing)) = (&active, self.tabs.get(idx)) {
            if active.tab_id() == removing.tab_id()
                && config::configuration().switch_to_last_active_tab_when_closing_tab
            {
                // If we are removing the active tab, switch back to
                // the previously active tab
                if let Some(last_active) = self.get_last_active_idx() {
                    self.set_active_without_saving(last_active);
                }
            }
        }
        let tab = self.tabs.remove(idx);
        self.fixup_active_tab_after_removal(active);
        tab
    }

    pub fn get_active(&self) -> Option<&Arc<Tab>> {
        self.get_by_idx(self.active)
    }

    #[inline]
    pub fn get_active_idx(&self) -> usize {
        self.active
    }

    pub fn save_last_active(&mut self) {
        self.last_active = self.get_by_idx(self.active).map(|tab| tab.tab_id());
    }

    #[inline]
    pub fn get_last_active_idx(&self) -> Option<usize> {
        if let Some(tab_id) = self.last_active {
            self.idx_by_id(tab_id)
        } else {
            None
        }
    }

    /// If `idx` is different from the current active tab,
    /// save the current tabid and then make `idx` the active
    /// tab position.
    pub fn save_and_then_set_active(&mut self, idx: usize) {
        if idx == self.get_active_idx() {
            return;
        }
        self.save_last_active();
        self.set_active_without_saving(idx);
    }

    /// Make `idx` the active tab position.
    /// The saved tab id is not changed.
    pub fn set_active_without_saving(&mut self, idx: usize) {
        assert!(idx < self.tabs.len());
        if self.active != idx {
            if let Some(tab) = self.tabs.get(self.active) {
                if let Some(pane) = tab.get_active_pane() {
                    pane.focus_changed(false);
                }
            }
        }
        self.active = idx;
        self.invalidate();
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Tab>> {
        self.tabs.iter()
    }

    /// Move the tab at `from` to `to`, shifting the ones in between along.
    /// The active tab stays active wherever it lands.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active = self.get_active().map(|tab| tab.tab_id());
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.tabs_reordered(active);
    }

    /// Put the tabs that `order` names into that order; see arrange_tab_ids.
    /// The active tab stays active wherever it lands.
    pub fn set_tab_order(&mut self, order: &[TabId]) {
        let ids: Vec<TabId> = self.tabs.iter().map(|tab| tab.tab_id()).collect();
        let arranged = arrange_tab_ids(&ids, order);
        if arranged == ids {
            return;
        }
        let active = self.get_active().map(|tab| tab.tab_id());
        let mut tabs = std::mem::take(&mut self.tabs);
        for id in arranged {
            if let Some(idx) = tabs.iter().position(|tab| tab.tab_id() == id) {
                self.tabs.push(tabs.remove(idx));
            }
        }
        // Nothing should be left over, but a tab dropped here would vanish
        // from the window while its panes went on running.
        self.tabs.append(&mut tabs);
        self.tabs_reordered(active);
    }

    /// Keep `active` the active tab after a reorder, and say so.
    ///
    /// The index is set directly rather than through set_active_without_saving,
    /// which would tell whichever tab now sits at the old index that it lost
    /// focus: the same tab is active before and after, so there is no focus
    /// change to report.
    fn tabs_reordered(&mut self, active: Option<TabId>) {
        if let Some(idx) = active.and_then(|id| self.idx_by_id(id)) {
            self.active = idx;
        }
        self.invalidate();
        Mux::get().notify(MuxNotification::WindowTabsReordered(self.id));
    }

    pub fn prune_dead_tabs(&mut self, live_tab_ids: &[TabId]) {
        let mut invalidated = false;
        let dead: Vec<TabId> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                if tab.prune_dead_panes() {
                    invalidated = true;
                }
                if tab.is_dead() {
                    Some(tab.tab_id())
                } else {
                    None
                }
            })
            .collect();

        for tab_id in dead {
            log::trace!("Window::prune_dead_tabs: tab_id {} is dead", tab_id);
            self.remove_by_id(tab_id);
            invalidated = true;
        }

        let dead: Vec<TabId> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                if live_tab_ids
                    .iter()
                    .find(|&&id| id == tab.tab_id())
                    .is_none()
                {
                    Some(tab.tab_id())
                } else {
                    None
                }
            })
            .collect();
        for tab_id in dead {
            log::trace!("Window::prune_dead_tabs: (live) tab_id {} is dead", tab_id);
            self.remove_by_id(tab_id);
        }

        if invalidated {
            self.invalidate();
        }
    }
}

/// The order `ids` end up in once the ones that `order` names are put into
/// that order.
///
/// Each of them takes a slot that one of them held before, so an id that
/// `order` does not mention keeps its place -- a tab added here that whoever
/// sent the order had not heard of yet -- and one that `order` names but `ids`
/// does not have is ignored.
fn arrange_tab_ids(ids: &[TabId], order: &[TabId]) -> Vec<TabId> {
    let mut wanted: Vec<TabId> = vec![];
    for id in order {
        if ids.contains(id) && !wanted.contains(id) {
            wanted.push(*id);
        }
    }
    let mut wanted = wanted.into_iter();
    ids.iter()
        .map(|id| {
            if order.contains(id) {
                wanted.next().unwrap_or(*id)
            } else {
                *id
            }
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn naming_every_tab_puts_them_in_that_order() {
        assert_eq!(arrange_tab_ids(&[1, 2, 3], &[3, 1, 2]), vec![3, 1, 2]);
    }

    #[test]
    fn a_tab_the_order_does_not_name_keeps_its_place() {
        assert_eq!(arrange_tab_ids(&[1, 2, 3, 4], &[4, 2, 1]), vec![4, 2, 3, 1]);
    }

    #[test]
    fn a_tab_the_window_does_not_have_is_ignored() {
        assert_eq!(arrange_tab_ids(&[1, 2], &[9, 2, 1]), vec![2, 1]);
    }

    #[test]
    fn a_tab_named_twice_counts_once() {
        assert_eq!(arrange_tab_ids(&[1, 2, 3], &[3, 3, 1, 2]), vec![3, 1, 2]);
    }

    #[test]
    fn an_empty_order_changes_nothing() {
        assert_eq!(arrange_tab_ids(&[1, 2], &[]), vec![1, 2]);
    }

    // Passing an explicit workspace keeps Window::new from reaching for the
    // global Mux, which does not exist in a unit test.
    fn a_window() -> Window {
        Window::new(Some("default".to_string()), None)
    }

    #[test]
    fn windows_are_not_retained_by_default() {
        assert!(
            !a_window().retain_when_empty(),
            "only the startup window opts in; every other empty window is dead"
        );
    }

    #[test]
    fn retention_is_independent_of_emptiness() {
        let mut w = a_window();
        w.set_retain_when_empty(true);
        assert!(w.retain_when_empty());
        assert!(
            w.is_empty(),
            "retention says do not prune me, not I have tabs -- prune_dead_windows needs both facts and must not confuse them"
        );
    }
}
