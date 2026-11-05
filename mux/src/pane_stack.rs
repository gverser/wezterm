//! A stack of panes occupying a single leaf of a tab's split tree.
//!
//! Exactly one member is visible at a time. The leaf's slot may reserve a
//! header row at the top for a tab strip, in which case members are sized
//! `member_size(slot, header_rows)` rather than the full slot.
use crate::pane::{Pane, PaneId};
use std::sync::Arc;
use wezterm_term::TerminalSize;

/// Upper bound for the header rows reserved above a stack's members.
/// Anything entering `Tab` from outside must be clamped to this.
pub const MAX_HEADER_ROWS: usize = 1;

fn cell_height(size: &TerminalSize) -> usize {
    // Zero rows would otherwise divide by zero; such a slot has no
    // meaningful cell height anyway.
    size.pixel_height.checked_div(size.rows).unwrap_or(0)
}

/// The size of each stack member given the size of the leaf slot that
/// holds the stack and `header_rows` reserved at its top.
pub fn member_size(slot: &TerminalSize, header_rows: usize) -> TerminalSize {
    if header_rows == 0 {
        // Must be an exact copy so that header-less tabs behave as before.
        return *slot;
    }
    let cell_h = cell_height(slot);
    TerminalSize {
        rows: slot.rows.saturating_sub(header_rows),
        pixel_height: slot.pixel_height.saturating_sub(header_rows * cell_h),
        ..*slot
    }
}

/// Inverse of `member_size`: the slot size needed to hold a member of
/// size `member` plus `header_rows`.
pub fn slot_size(member: &TerminalSize, header_rows: usize) -> TerminalSize {
    if header_rows == 0 {
        return *member;
    }
    let cell_h = cell_height(member);
    TerminalSize {
        rows: member.rows + header_rows,
        pixel_height: member.pixel_height + header_rows * cell_h,
        ..*member
    }
}

pub struct PaneStack {
    panes: Vec<Arc<dyn Pane>>,
    /// Always a valid index into `panes` while `panes` is non-empty.
    visible: usize,
}

impl PaneStack {
    pub fn new(pane: Arc<dyn Pane>) -> Self {
        Self {
            panes: vec![pane],
            visible: 0,
        }
    }

    /// Returns `None` for an empty list. An out of range `visible`
    /// (possible with data from the wire) is clamped to 0.
    pub fn from_parts(panes: Vec<Arc<dyn Pane>>, visible: usize) -> Option<Self> {
        if panes.is_empty() {
            return None;
        }
        let visible = if visible < panes.len() {
            visible
        } else {
            log::warn!(
                "PaneStack::from_parts: visible index {} out of range for {} panes, using 0",
                visible,
                panes.len()
            );
            0
        };
        Some(Self { panes, visible })
    }

    pub fn len(&self) -> usize {
        self.panes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    pub fn panes(&self) -> &[Arc<dyn Pane>] {
        &self.panes
    }

    pub fn visible_index(&self) -> usize {
        self.visible
    }

    /// Calling this on an empty stack is a logic error; `Tab` never keeps
    /// an empty stack around.
    pub fn visible_pane(&self) -> &Arc<dyn Pane> {
        &self.panes[self.visible]
    }

    pub fn position_of(&self, pane_id: PaneId) -> Option<usize> {
        self.panes.iter().position(|p| p.pane_id() == pane_id)
    }

    /// Returns false, leaving the stack untouched, if `index` is out of range.
    pub fn set_visible(&mut self, index: usize) -> bool {
        if index < self.panes.len() {
            self.visible = index;
            true
        } else {
            false
        }
    }

    /// Inserts at `min(index, len)`, makes the new member visible and
    /// returns the index it ended up at.
    pub fn insert(&mut self, index: usize, pane: Arc<dyn Pane>) -> usize {
        let index = index.min(self.panes.len());
        self.panes.insert(index, pane);
        self.visible = index;
        index
    }

    /// Removes every member for which `f` returns true and returns them in
    /// their original order.
    ///
    /// If the visible member is removed, the first survivor to its right
    /// becomes visible, else the nearest survivor to its left. If it
    /// survives, it stays visible and its index shifts with the removals
    /// before it.
    pub fn remove_where<F: FnMut(&Arc<dyn Pane>) -> bool>(
        &mut self,
        mut f: F,
    ) -> Vec<Arc<dyn Pane>> {
        let old_visible = self.visible;
        let mut removed = vec![];
        let mut kept: Vec<(usize, Arc<dyn Pane>)> = vec![];
        for (idx, pane) in std::mem::take(&mut self.panes).into_iter().enumerate() {
            if f(&pane) {
                removed.push(pane);
            } else {
                kept.push((idx, pane));
            }
        }

        self.visible = if kept.is_empty() {
            0
        } else {
            kept.iter()
                .position(|(idx, _)| *idx >= old_visible)
                // Nothing at or right of the old position survived, so
                // fall back to the nearest survivor on the left.
                .unwrap_or(kept.len() - 1)
        };
        self.panes = kept.into_iter().map(|(_, pane)| pane).collect();
        removed
    }

    /// Removes the member at `index`, following the same visibility rules
    /// as `remove_where`. Used to reorder within a stack.
    pub fn take_at(&mut self, index: usize) -> Option<Arc<dyn Pane>> {
        if index >= self.panes.len() {
            return None;
        }
        let mut current = 0;
        self.remove_where(|_| {
            let hit = current == index;
            current += 1;
            hit
        })
        .pop()
    }

    /// Resizes every member, not just the visible one, so that switching
    /// members never shows a stale size. All members are attempted even
    /// if some fail; the first error is returned.
    pub fn resize_all(&self, size: TerminalSize) -> anyhow::Result<()> {
        let mut first_err = None;
        for pane in &self.panes {
            if let Err(err) = pane.resize(size) {
                first_err.get_or_insert(err);
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_pane::*;

    fn stack_of(ids: &[PaneId]) -> PaneStack {
        let panes = ids
            .iter()
            .map(|id| FakePane::new(*id, size(23, 80)))
            .collect();
        PaneStack::from_parts(panes, 0).unwrap()
    }

    #[test]
    fn member_and_slot_sizes() {
        let slot = size(24, 80); // 600px high, 25px cells
        let m = member_size(&slot, 1);
        assert_eq!(
            (m.rows, m.pixel_height, m.cols, m.pixel_width),
            (23, 575, 80, 800)
        );
        assert_eq!(slot_size(&m, 1), slot);
        assert_eq!(member_size(&slot, 0), slot);
        let tiny = size(0, 80);
        assert_eq!(member_size(&tiny, 1).rows, 0); // saturates, no panic
    }

    #[test]
    fn insert_shows_new_member() {
        let mut s = PaneStack::new(FakePane::new(1, size(23, 80)));
        assert_eq!(s.insert(1, FakePane::new(2, size(23, 80))), 1);
        assert_eq!(s.insert(99, FakePane::new(3, size(23, 80))), 2);
        assert_eq!(ids(s.panes()), vec![1, 2, 3]);
        assert_eq!(s.visible_pane().pane_id(), 3);
    }

    #[test]
    fn removing_visible_prefers_right_neighbor() {
        let mut s = stack_of(&[1, 2, 3, 4]);
        s.set_visible(1); // pane 2
        assert_eq!(ids(&s.remove_where(|p| p.pane_id() == 2)), vec![2]);
        assert_eq!(s.visible_pane().pane_id(), 3);
        s.set_visible(2); // pane 4, rightmost
        s.remove_where(|p| p.pane_id() == 4);
        assert_eq!(s.visible_pane().pane_id(), 3);
        s.set_visible(0); // pane 1
        s.remove_where(|p| p.pane_id() == 3); // a member left of nothing, right of visible
        assert_eq!((s.visible_pane().pane_id(), s.visible_index()), (1, 0));
        s.remove_where(|_| true);
        assert!(s.is_empty());
    }

    #[test]
    fn removing_hidden_keeps_visible_member() {
        let mut s = stack_of(&[1, 2, 3]);
        s.set_visible(2);
        s.remove_where(|p| p.pane_id() == 1);
        assert_eq!((s.visible_pane().pane_id(), s.visible_index()), (3, 1));
    }

    #[test]
    fn from_parts_clamps_and_rejects_empty() {
        assert!(PaneStack::from_parts(vec![], 0).is_none());
        let s = PaneStack::from_parts(vec![FakePane::new(1, size(23, 80))], 5).unwrap();
        assert_eq!(s.visible_index(), 0);
    }

    #[test]
    fn resize_all_reaches_every_member() {
        let s = stack_of(&[1, 2]);
        s.resize_all(size(10, 40)).unwrap();
        for p in s.panes() {
            assert_eq!(current_size(p), size(10, 40));
        }
    }
}
