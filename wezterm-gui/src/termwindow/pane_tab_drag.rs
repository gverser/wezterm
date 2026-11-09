//! Dragging a tab of a pane stack strip: along its own strip to reorder the
//! stack, or onto another split's strip to move the pane into that stack.
//!
//! While the button is down only an insertion line follows the pointer; the
//! pane moves once, on release. The window tab bar reorders as the pointer
//! goes, but connected to a mux server every step here would be a PDU and a
//! resync of the whole tree, and a resync that lands late would pull the
//! strip back to an older order.
use crate::pane_strip::{drawn_tabs, drop_gap, move_index_for_drop};
use crate::termwindow::{UIItem, UIItemType};
use ::window::{MouseEvent, WindowOps};
use mux::pane::PaneId;
use mux::tab::TabId;
use mux::Mux;

/// How far the pointer must travel from the press, in cells, before a press
/// on a strip tab becomes a drag; less is a click with an unsteady hand.
pub const DRAG_THRESHOLD_CELLS: f32 = 0.5;

/// Compared squared so that a distance of exactly the threshold passes
/// whatever `hypot` rounds it to.
fn passes_drag_threshold(dx: f32, dy: f32, cell_width: f32) -> bool {
    let threshold = DRAG_THRESHOLD_CELLS * cell_width;
    dx * dx + dy * dy >= threshold * threshold
}

/// A left press on a strip tab, from the press until the release.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneTabDrag {
    pub tab_id: TabId,
    pub pane_id: PaneId,
    /// False until the pointer passes the threshold. Until then the press is
    /// just the click it already performed.
    pub dragging: bool,
    /// Where the tab would land, as of the last move.
    pub target: Option<PaneTabDrop>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneTabDrop {
    /// Any member of the target stack. The stack is named by a pane rather
    /// than by its leaf index because a move that empties the source leaf
    /// removes it and renumbers the leaves after it.
    pub target_pane_id: PaneId,
    /// The gap in the target stack, counting the dragged pane if it is there.
    pub gap: usize,
    /// The dragged pane's position in the target stack, if it is a member.
    pub from: Option<usize>,
    /// Pixel x of the insertion line.
    pub line_x: f32,
    /// Pixel y of the target strip.
    pub line_top: f32,
}

impl crate::TermWindow {
    /// Called for a left press on a strip tab that performed its click.
    /// `self.dragging` routes the moves and the release back here.
    pub(super) fn start_pane_tab_drag(&mut self, item: UIItem, event: MouseEvent) {
        let (tab_id, pane_id) = match item.item_type {
            UIItemType::PaneTab { tab_id, pane_id } => (tab_id, pane_id),
            _ => return,
        };
        self.pane_tab_drag = Some(PaneTabDrag {
            tab_id,
            pane_id,
            dragging: false,
            target: None,
        });
        self.dragging = Some((item, event));
    }

    /// Forgets a strip tab press whose release will never arrive: Windows
    /// does not deliver WM_LBUTTONUP when another window takes the
    /// foreground mid-drag. Returns true if there was one, in which case an
    /// insertion line may be on screen.
    pub(super) fn cancel_pane_tab_drag(&mut self) -> bool {
        if matches!(
            self.dragging,
            Some((
                UIItem {
                    item_type: UIItemType::PaneTab { .. },
                    ..
                },
                _
            ))
        ) {
            self.dragging = None;
        }
        self.pane_tab_drag.take().is_some()
    }

    /// Follows the pointer while the button is down. Nothing moves; only the
    /// insertion line does, and the window repaints only when it changes.
    pub(super) fn drag_pane_tab(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let drag = match self.pane_tab_drag {
            Some(drag) => drag,
            // Cancelled; drop the routing too.
            None => return,
        };

        if !drag.dragging {
            let dx = (event.coords.x - start_event.coords.x) as f32;
            let dy = (event.coords.y - start_event.coords.y) as f32;
            let cell_width = self.render_metrics.cell_size.width as f32;
            if !passes_drag_threshold(dx, dy, cell_width) {
                self.dragging.replace((item, start_event));
                return;
            }
        }

        let target = self.pane_tab_drop_at(&drag, event.coords.x, event.coords.y);
        if let Some(drag) = self.pane_tab_drag.as_mut() {
            drag.dragging = true;
            if drag.target != target {
                drag.target = target;
                context.invalidate();
            }
        }
        self.dragging.replace((item, start_event));
    }

    /// Where the tab of `drag` would land if dropped at pixel (x, y), or None
    /// if that is not on a strip of the tab it was picked up in.
    ///
    /// Measured against the hit regions of the last frame: that is what the
    /// user is aiming at, and the drag itself changes nothing in the mux
    /// before the release.
    fn pane_tab_drop_at(&self, drag: &PaneTabDrag, x: isize, y: isize) -> Option<PaneTabDrop> {
        let tab = Mux::get().get_active_tab_for_window(self.mux_window_id)?;
        // Another tab, or a tab-level overlay that covers the strips: the
        // strips the user aimed at are not on screen any more.
        if tab.tab_id() != drag.tab_id
            || self.tab_state(drag.tab_id).overlay.is_some()
            || !tab.contains_pane(drag.pane_id)
        {
            return None;
        }

        // Searched from the back like resolve_ui_item, so where two items
        // overlap the one painted last wins.
        let bar = self.ui_items.iter().rev().find(|item| {
            matches!(item.item_type, UIItemType::PaneTabBar { tab_id, .. } if tab_id == drag.tab_id)
                && item.hit_test(x, y)
        })?;
        let leaf = match bar.item_type {
            UIItemType::PaneTabBar { leaf, .. } => leaf,
            _ => return None,
        };

        let items: Vec<(PaneId, f32, f32)> = self
            .ui_items
            .iter()
            .filter_map(|item| match item.item_type {
                UIItemType::PaneTab { tab_id, pane_id }
                    if tab_id == drag.tab_id
                        && item.x >= bar.x
                        && item.x < bar.x + bar.width
                        && item.y >= bar.y
                        && item.y < bar.y + bar.height =>
                {
                    Some((pane_id, item.x as f32, item.width as f32))
                }
                _ => None,
            })
            .collect();

        // While zoomed only the zoomed stack is returned, so the leaf index
        // is matched rather than used as a position in the list.
        let stack: Vec<PaneId> = tab
            .iter_stacks()
            .into_iter()
            .find(|stack| stack.index == leaf)?
            .panes
            .iter()
            .map(|pane| pane.pane_id())
            .collect();
        let target_pane_id = *stack.first()?;

        let (gap, line_x) = drop_gap(&drawn_tabs(&items, &stack), bar.x as f32, x as f32);
        Some(PaneTabDrop {
            target_pane_id,
            gap,
            from: stack.iter().position(|&id| id == drag.pane_id),
            line_x,
            line_top: bar.y as f32,
        })
    }

    /// The button came up on a press that began on a strip tab.
    pub(super) fn finish_pane_tab_drag(&mut self, event: &MouseEvent, context: &dyn WindowOps) {
        // Takes the insertion line off, whatever happens below.
        context.invalidate();

        let drag = match self.pane_tab_drag.take() {
            Some(drag) => drag,
            None => return,
        };
        // Never left the threshold: a click, and the press already did it.
        if !drag.dragging {
            return;
        }
        // From the release itself rather than the last move, which may be
        // a while and a fair distance back. Released outside the window
        // (SetCapture still delivers it), this finds no strip.
        let target = match self.pane_tab_drop_at(&drag, event.coords.x, event.coords.y) {
            Some(target) => target,
            None => return,
        };
        let index = match move_index_for_drop(target.from, target.gap) {
            Some(index) => index,
            None => return,
        };

        let pane_id = drag.pane_id;
        let target_pane_id = target.target_pane_id;
        let window = self.window.clone();
        promise::spawn::spawn(async move {
            if let Err(err) = Mux::get()
                .move_pane_to_stack(pane_id, target_pane_id, index)
                .await
            {
                log::error!(
                    "moving pane {} to index {} of the stack of pane {}: {:#}",
                    pane_id,
                    index,
                    target_pane_id,
                    err
                );
            }
            // Connected to a mux server, the local copy only takes the new
            // order in the resync at the end of the await, and nothing
            // repaints for that: TabResized only updates the title, and a
            // reorder, or a move between leaves of equal width, makes no
            // output. Without this the strip would show the old order until
            // the pointer next moved, and the next drag would be measured
            // against the old frame's positions with the new order.
            if let Some(window) = window {
                window.invalidate();
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn drag_threshold_is_half_a_cell() {
        assert!(!passes_drag_threshold(4.9, 0.0, 10.0));
        assert!(passes_drag_threshold(5.0, 0.0, 10.0));
        // Measured along the diagonal: (3, 4) is 5 pixels away.
        assert!(passes_drag_threshold(3.0, 4.0, 10.0));
    }
}
