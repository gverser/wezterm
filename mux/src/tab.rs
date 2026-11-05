use crate::domain::DomainId;
use crate::pane::*;
use crate::renderable::StableCursorPosition;
use crate::{Mux, MuxNotification, WindowId};
use bintree::PathBranch;
use config::configuration;
use config::keyassignment::PaneDirection;
use parking_lot::Mutex;
use rangeset::intersects_range;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::Arc;
use url::Url;
use wezterm_term::{StableRowIndex, TerminalSize};

pub use crate::pane_stack::{member_size, min_leaf_rows, slot_size, PaneStack, MAX_HEADER_ROWS};

pub type Tree = bintree::Tree<PaneStack, SplitDirectionAndSize>;
pub type Cursor = bintree::Cursor<PaneStack, SplitDirectionAndSize>;

static TAB_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type TabId = usize;

#[derive(Default)]
struct Recency {
    count: usize,
    by_idx: HashMap<usize, usize>,
}

impl Recency {
    fn tag(&mut self, idx: usize) {
        self.by_idx.insert(idx, self.count);
        self.count += 1;
    }

    fn score(&self, idx: usize) -> usize {
        self.by_idx.get(&idx).copied().unwrap_or(0)
    }
}

struct TabInner {
    id: TabId,
    pane: Option<Tree>,
    size: TerminalSize,
    size_before_zoom: TerminalSize,
    active: usize,
    zoomed: Option<Arc<dyn Pane>>,
    title: String,
    recency: Recency,
    /// Rows reserved at the top of every leaf slot for its tab strip;
    /// always within `0..=MAX_HEADER_ROWS`.
    header_rows: usize,
}

/// A Tab is a container of Panes
pub struct Tab {
    inner: Mutex<TabInner>,
    tab_id: TabId,
}

#[derive(Clone)]
pub struct PositionedPane {
    /// The topological pane index that can be used to reference this pane
    pub index: usize,
    /// true if this is the active pane at the time the position was computed
    pub is_active: bool,
    /// true if this pane is zoomed
    pub is_zoomed: bool,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this pane, in cells.
    pub left: usize,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this pane, in cells.
    pub top: usize,
    /// The width of this pane in cells
    pub width: usize,
    pub pixel_width: usize,
    /// The height of this pane in cells
    pub height: usize,
    pub pixel_height: usize,
    /// The pane instance
    pub pane: Arc<dyn Pane>,
}

impl std::fmt::Debug for PositionedPane {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        fmt.debug_struct("PositionedPane")
            .field("index", &self.index)
            .field("is_active", &self.is_active)
            .field("left", &self.left)
            .field("top", &self.top)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pane_id", &self.pane.pane_id())
            .finish()
    }
}

/// A leaf of the split tree and the stack of panes it holds. Unlike
/// `PositionedPane`, the position and size are those of the leaf's slot,
/// which includes the header rows above the members.
#[derive(Clone)]
pub struct PositionedStack {
    /// The topological leaf index; the same numbering as
    /// `PositionedPane::index` outside of zoom
    pub index: usize,
    /// true if this is the active leaf at the time the position was computed
    pub is_active: bool,
    /// true if this leaf is zoomed; its slot is then the whole tab
    pub is_zoomed: bool,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this slot, in cells.
    pub left: usize,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this slot, in cells.
    pub top: usize,
    /// The width of this slot in cells
    pub width: usize,
    /// The height of this slot in cells, header rows included
    pub height: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    /// The members of the stack, in tab strip order
    pub panes: Vec<Arc<dyn Pane>>,
    /// The index into `panes` of the visible member
    pub visible: usize,
}

impl std::fmt::Debug for PositionedStack {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        let pane_ids: Vec<PaneId> = self.panes.iter().map(|p| p.pane_id()).collect();
        fmt.debug_struct("PositionedStack")
            .field("index", &self.index)
            .field("is_active", &self.is_active)
            .field("is_zoomed", &self.is_zoomed)
            .field("left", &self.left)
            .field("top", &self.top)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pane_ids", &pane_ids)
            .field("visible", &self.visible)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// The size is of the (first, second) child of the split
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct SplitDirectionAndSize {
    pub direction: SplitDirection,
    pub first: TerminalSize,
    pub second: TerminalSize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SplitSize {
    Cells(usize),
    Percent(u8),
}

impl Default for SplitSize {
    fn default() -> Self {
        Self::Percent(50)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct SplitRequest {
    pub direction: SplitDirection,
    /// Whether the newly created item will be in the second part
    /// of the split (right/bottom)
    pub target_is_second: bool,
    /// Split across the top of the tab rather than the active pane
    pub top_level: bool,
    /// The size of the new item
    pub size: SplitSize,
}

impl Default for SplitRequest {
    fn default() -> Self {
        Self {
            direction: SplitDirection::Horizontal,
            target_is_second: true,
            top_level: false,
            size: SplitSize::default(),
        }
    }
}

impl SplitDirectionAndSize {
    fn top_of_second(&self) -> usize {
        match self.direction {
            SplitDirection::Horizontal => 0,
            SplitDirection::Vertical => self.first.rows as usize + 1,
        }
    }

    fn left_of_second(&self) -> usize {
        match self.direction {
            SplitDirection::Horizontal => self.first.cols as usize + 1,
            SplitDirection::Vertical => 0,
        }
    }

    pub fn width(&self) -> usize {
        if self.direction == SplitDirection::Horizontal {
            self.first.cols + self.second.cols + 1
        } else {
            self.first.cols
        }
    }

    pub fn height(&self) -> usize {
        if self.direction == SplitDirection::Vertical {
            self.first.rows + self.second.rows + 1
        } else {
            self.first.rows
        }
    }

    pub fn size(&self) -> TerminalSize {
        let cell_width = self.first.pixel_width / self.first.cols;
        let cell_height = self.first.pixel_height / self.first.rows;

        let rows = self.height();
        let cols = self.width();

        TerminalSize {
            rows,
            cols,
            pixel_height: cell_height * rows,
            pixel_width: cell_width * cols,
            dpi: self.first.dpi,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PositionedSplit {
    /// The topological node index that can be used to reference this split
    pub index: usize,
    pub direction: SplitDirection,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this split, in cells.
    pub left: usize,
    /// The offset from the top left corner of the containing tab to the top
    /// left corner of this split, in cells.
    pub top: usize,
    /// For Horizontal splits, how tall the split should be, for Vertical
    /// splits how wide it should be
    pub size: usize,
}

fn is_pane(pane: &Arc<dyn Pane>, other: &Option<&Arc<dyn Pane>>) -> bool {
    if let Some(other) = other {
        other.pane_id() == pane.pane_id()
    } else {
        false
    }
}

fn pane_tree(
    tree: &Tree,
    tab_id: TabId,
    window_id: WindowId,
    active: Option<&Arc<dyn Pane>>,
    zoomed: Option<&Arc<dyn Pane>>,
    workspace: &str,
    left_col: usize,
    top_row: usize,
    header_rows: usize,
) -> PaneNode {
    match tree {
        Tree::Empty => PaneNode::Empty,
        Tree::Node { left, right, data } => {
            let data = data.unwrap();
            PaneNode::Split {
                left: Box::new(pane_tree(
                    &*left,
                    tab_id,
                    window_id,
                    active,
                    zoomed,
                    workspace,
                    left_col,
                    top_row,
                    header_rows,
                )),
                right: Box::new(pane_tree(
                    &*right,
                    tab_id,
                    window_id,
                    active,
                    zoomed,
                    workspace,
                    if data.direction == SplitDirection::Vertical {
                        left_col
                    } else {
                        left_col + data.left_of_second()
                    },
                    if data.direction == SplitDirection::Horizontal {
                        top_row
                    } else {
                        top_row + data.top_of_second()
                    },
                    header_rows,
                )),
                node: data,
            }
        }
        Tree::Leaf(stack) => {
            // The whole stack goes on the wire so that a client can rebuild
            // hidden members too. The header rows sit at the top of the slot,
            // so every member starts below them.
            let panes = stack
                .panes()
                .iter()
                .map(|pane| {
                    let dims = pane.get_dimensions();
                    let working_dir = pane.get_current_working_dir(CachePolicy::AllowStale);
                    let cursor_pos = pane.get_cursor_position();

                    PaneEntry {
                        window_id,
                        tab_id,
                        pane_id: pane.pane_id(),
                        title: pane.get_title(),
                        is_active_pane: is_pane(pane, &active),
                        is_zoomed_pane: is_pane(pane, &zoomed),
                        size: TerminalSize {
                            cols: dims.cols,
                            rows: dims.viewport_rows,
                            pixel_height: dims.pixel_height,
                            pixel_width: dims.pixel_width,
                            dpi: dims.dpi,
                        },
                        working_dir: working_dir.map(Into::into),
                        workspace: workspace.to_string(),
                        cursor_pos,
                        physical_top: dims.physical_top,
                        left_col,
                        top_row: top_row + header_rows,
                        tty_name: pane.tty_name(),
                    }
                })
                .collect();
            PaneNode::Leaf(PaneStackEntry {
                panes,
                visible: stack.visible_index(),
                header_rows,
            })
        }
    }
}

fn build_from_pane_tree<F>(
    tree: bintree::Tree<PaneStackEntry, SplitDirectionAndSize>,
    active: &mut Option<Arc<dyn Pane>>,
    zoomed: &mut Option<Arc<dyn Pane>>,
    make_pane: &mut F,
) -> Tree
where
    F: FnMut(PaneEntry) -> Arc<dyn Pane>,
{
    match tree {
        bintree::Tree::Empty => Tree::Empty,
        bintree::Tree::Node { left, right, data } => {
            let left = build_from_pane_tree(*left, active, zoomed, make_pane);
            let right = build_from_pane_tree(*right, active, zoomed, make_pane);
            // An Empty child under a Node would stop bintree's preorder walk
            // and hide the right-hand siblings from `iter_panes`, so collapse
            // the split instead.
            match (left, right) {
                (Tree::Empty, Tree::Empty) => Tree::Empty,
                (Tree::Empty, other) | (other, Tree::Empty) => other,
                (left, right) => Tree::Node {
                    left: Box::new(left),
                    right: Box::new(right),
                    data,
                },
            }
        }
        bintree::Tree::Leaf(stack_entry) => {
            let mut panes = vec![];
            for entry in stack_entry.panes {
                let is_zoomed_pane = entry.is_zoomed_pane;
                let is_active_pane = entry.is_active_pane;
                let pane = make_pane(entry);
                if is_zoomed_pane {
                    zoomed.replace(Arc::clone(&pane));
                }
                if is_active_pane {
                    active.replace(Arc::clone(&pane));
                }
                panes.push(pane);
            }
            match PaneStack::from_parts(panes, stack_entry.visible) {
                Some(stack) => Tree::Leaf(stack),
                None => {
                    // The server never sends this; don't let bad data take the
                    // client down.
                    log::error!("pane tree contains an empty stack; ignoring it");
                    Tree::Empty
                }
            }
        }
    }
}

/// The stacks of `tree` in preorder, which is the order that leaf indices
/// (`TabInner::active`, `PositionedPane::index`) count in.
fn leaves(tree: &Tree) -> Vec<&PaneStack> {
    fn walk<'a>(tree: &'a Tree, out: &mut Vec<&'a PaneStack>) {
        match tree {
            Tree::Empty => {}
            Tree::Node { left, right, .. } => {
                walk(left, out);
                walk(right, out);
            }
            Tree::Leaf(stack) => out.push(stack),
        }
    }
    let mut out = vec![];
    walk(tree, &mut out);
    out
}

/// Like `leaves`, but mutable, so that whole stacks can be swapped.
fn leaves_mut(tree: &mut Tree) -> Vec<&mut PaneStack> {
    fn walk<'a>(tree: &'a mut Tree, out: &mut Vec<&'a mut PaneStack>) {
        match tree {
            Tree::Empty => {}
            Tree::Node { left, right, .. } => {
                walk(left, out);
                walk(right, out);
            }
            Tree::Leaf(stack) => out.push(stack),
        }
    }
    let mut out = vec![];
    walk(tree, &mut out);
    out
}

/// Swaps the contents of leaves `i` and `j`, so a whole stack changes place.
fn swap_stacks(stacks: &mut [&mut PaneStack], i: usize, j: usize) {
    if i == j {
        return;
    }
    let (lo, hi) = (i.min(j), i.max(j));
    let (head, tail) = stacks.split_at_mut(hi);
    std::mem::swap(&mut *head[lo], &mut *tail[0]);
}

/// The position of the leaf at `cursor` and the size of its slot, that is,
/// before its header rows are taken out.
fn slot_geometry(cursor: &Cursor, root_size: TerminalSize) -> (usize, usize, TerminalSize) {
    let mut left = 0usize;
    let mut top = 0usize;
    let mut parent_size = None;
    for (branch, node) in cursor.path_to_root() {
        if let Some(node) = node {
            if parent_size.is_none() {
                parent_size.replace(if branch == PathBranch::IsRight {
                    node.second
                } else {
                    node.first
                });
            }
            if branch == PathBranch::IsRight {
                top += node.top_of_second();
                left += node.left_of_second();
            }
        }
    }
    (left, top, parent_size.unwrap_or(root_size))
}

/// Computes the minimum (x, y) size based on the panes in this portion
/// of the tree. A leaf needs one row for its members plus its header rows.
fn compute_min_size(tree: &mut Tree, header_rows: usize) -> (usize, usize) {
    match tree {
        Tree::Node { data: None, .. } | Tree::Empty => (1, 1),
        Tree::Node {
            left,
            right,
            data: Some(data),
        } => {
            let (left_x, left_y) = compute_min_size(&mut *left, header_rows);
            let (right_x, right_y) = compute_min_size(&mut *right, header_rows);
            match data.direction {
                SplitDirection::Vertical => (left_x.max(right_x), left_y + right_y + 1),
                SplitDirection::Horizontal => (left_x + right_x + 1, left_y.max(right_y)),
            }
        }
        Tree::Leaf(_) => (1, min_leaf_rows(header_rows)),
    }
}

fn adjust_x_size(
    tree: &mut Tree,
    mut x_adjust: isize,
    cell_dimensions: &TerminalSize,
    header_rows: usize,
) {
    let (min_x, _) = compute_min_size(tree, header_rows);
    while x_adjust != 0 {
        match tree {
            Tree::Empty | Tree::Leaf(_) => return,
            Tree::Node { data: None, .. } => return,
            Tree::Node {
                left,
                right,
                data: Some(data),
            } => {
                data.first.dpi = cell_dimensions.dpi;
                data.second.dpi = cell_dimensions.dpi;
                match data.direction {
                    SplitDirection::Vertical => {
                        let new_cols = (data.first.cols as isize)
                            .saturating_add(x_adjust)
                            .max(min_x as isize);
                        x_adjust = new_cols.saturating_sub(data.first.cols as isize);

                        if x_adjust != 0 {
                            adjust_x_size(&mut *left, x_adjust, cell_dimensions, header_rows);
                            data.first.cols = new_cols.try_into().unwrap();
                            data.first.pixel_width =
                                data.first.cols.saturating_mul(cell_dimensions.pixel_width);

                            adjust_x_size(&mut *right, x_adjust, cell_dimensions, header_rows);
                            data.second.cols = data.first.cols;
                            data.second.pixel_width = data.first.pixel_width;
                        }
                        return;
                    }
                    SplitDirection::Horizontal if x_adjust > 0 => {
                        adjust_x_size(&mut *left, 1, cell_dimensions, header_rows);
                        data.first.cols += 1;
                        data.first.pixel_width =
                            data.first.cols.saturating_mul(cell_dimensions.pixel_width);
                        x_adjust -= 1;

                        if x_adjust > 0 {
                            adjust_x_size(&mut *right, 1, cell_dimensions, header_rows);
                            data.second.cols += 1;
                            data.second.pixel_width =
                                data.second.cols.saturating_mul(cell_dimensions.pixel_width);
                            x_adjust -= 1;
                        }
                    }
                    SplitDirection::Horizontal => {
                        // x_adjust is negative.
                        // Only shrink a side that is above its own minimum:
                        // a nested subtree already at its minimum cannot
                        // absorb the -1, and asking it to would loop forever.
                        if data.first.cols > compute_min_size(&mut *left, header_rows).0 {
                            adjust_x_size(&mut *left, -1, cell_dimensions, header_rows);
                            data.first.cols -= 1;
                            data.first.pixel_width =
                                data.first.cols.saturating_mul(cell_dimensions.pixel_width);
                            x_adjust += 1;
                        }
                        if x_adjust < 0
                            && data.second.cols > compute_min_size(&mut *right, header_rows).0
                        {
                            adjust_x_size(&mut *right, -1, cell_dimensions, header_rows);
                            data.second.cols -= 1;
                            data.second.pixel_width =
                                data.second.cols.saturating_mul(cell_dimensions.pixel_width);
                            x_adjust += 1;
                        }
                    }
                }
            }
        }
    }
}

fn adjust_y_size(
    tree: &mut Tree,
    mut y_adjust: isize,
    cell_dimensions: &TerminalSize,
    header_rows: usize,
) {
    let (_, min_y) = compute_min_size(tree, header_rows);
    while y_adjust != 0 {
        match tree {
            Tree::Empty | Tree::Leaf(_) => return,
            Tree::Node { data: None, .. } => return,
            Tree::Node {
                left,
                right,
                data: Some(data),
            } => {
                data.first.dpi = cell_dimensions.dpi;
                data.second.dpi = cell_dimensions.dpi;
                match data.direction {
                    SplitDirection::Horizontal => {
                        let new_rows = (data.first.rows as isize)
                            .saturating_add(y_adjust)
                            .max(min_y as isize);
                        y_adjust = new_rows.saturating_sub(data.first.rows as isize);

                        if y_adjust != 0 {
                            adjust_y_size(&mut *left, y_adjust, cell_dimensions, header_rows);
                            data.first.rows = new_rows.try_into().unwrap();
                            data.first.pixel_height =
                                data.first.rows.saturating_mul(cell_dimensions.pixel_height);

                            adjust_y_size(&mut *right, y_adjust, cell_dimensions, header_rows);
                            data.second.rows = data.first.rows;
                            data.second.pixel_height = data.first.pixel_height;
                        }
                        return;
                    }
                    SplitDirection::Vertical if y_adjust > 0 => {
                        adjust_y_size(&mut *left, 1, cell_dimensions, header_rows);
                        data.first.rows += 1;
                        data.first.pixel_height =
                            data.first.rows.saturating_mul(cell_dimensions.pixel_height);
                        y_adjust -= 1;
                        if y_adjust > 0 {
                            adjust_y_size(&mut *right, 1, cell_dimensions, header_rows);
                            data.second.rows += 1;
                            data.second.pixel_height = data
                                .second
                                .rows
                                .saturating_mul(cell_dimensions.pixel_height);
                            y_adjust -= 1;
                        }
                    }
                    SplitDirection::Vertical => {
                        // y_adjust is negative.
                        // Only shrink a side that is above its own minimum
                        // (which includes header rows): a nested subtree
                        // already at its minimum cannot absorb the -1, and
                        // asking it to would loop forever.
                        if data.first.rows > compute_min_size(&mut *left, header_rows).1 {
                            adjust_y_size(&mut *left, -1, cell_dimensions, header_rows);
                            data.first.rows -= 1;
                            data.first.pixel_height =
                                data.first.rows.saturating_mul(cell_dimensions.pixel_height);
                            y_adjust += 1;
                        }
                        if y_adjust < 0
                            && data.second.rows > compute_min_size(&mut *right, header_rows).1
                        {
                            adjust_y_size(&mut *right, -1, cell_dimensions, header_rows);
                            data.second.rows -= 1;
                            data.second.pixel_height = data
                                .second
                                .rows
                                .saturating_mul(cell_dimensions.pixel_height);
                            y_adjust += 1;
                        }
                    }
                }
            }
        }
    }
}

/// `size` is the slot of `tree`; every leaf's members get the slot minus
/// `header_rows`.
fn apply_sizes_from_splits(tree: &Tree, size: &TerminalSize, header_rows: usize) {
    match tree {
        Tree::Empty => return,
        Tree::Node { data: None, .. } => return,
        Tree::Node {
            left,
            right,
            data: Some(data),
        } => {
            apply_sizes_from_splits(&*left, &data.first, header_rows);
            apply_sizes_from_splits(&*right, &data.second, header_rows);
        }
        Tree::Leaf(stack) => {
            stack.resize_all(member_size(size, header_rows)).ok();
        }
    }
}

fn cell_dimensions(size: &TerminalSize) -> TerminalSize {
    TerminalSize {
        rows: 1,
        cols: 1,
        pixel_width: size.pixel_width / size.cols,
        pixel_height: size.pixel_height / size.rows,
        dpi: size.dpi,
    }
}

/// The `header_rows` that tabs created locally should use. Remote tabs keep
/// the value the server reports instead.
pub fn configured_header_rows() -> usize {
    if configuration().enable_pane_tab_bar {
        1
    } else {
        0
    }
}

impl Tab {
    pub fn new(size: &TerminalSize) -> Self {
        Self::with_header_rows(size, 0)
    }

    /// Creates a tab whose leaves reserve `header_rows` at their top for a
    /// tab strip. Values above `MAX_HEADER_ROWS` are clamped.
    pub fn with_header_rows(size: &TerminalSize, header_rows: usize) -> Self {
        let inner = TabInner::new(size, header_rows.min(MAX_HEADER_ROWS));
        let tab_id = inner.id;
        Self {
            inner: Mutex::new(inner),
            tab_id,
        }
    }

    pub fn header_rows(&self) -> usize {
        self.inner.lock().header_rows
    }

    /// Every pane in the tab, hidden stack members included, in leaf
    /// preorder and then stack order. Ignores zoom.
    pub fn iter_all_panes(&self) -> Vec<Arc<dyn Pane>> {
        self.inner.lock().iter_all_panes()
    }

    /// The index of the leaf whose stack holds `pane_id`, whether or not it
    /// is the visible member. Ignores zoom.
    pub fn leaf_index_of(&self, pane_id: PaneId) -> Option<usize> {
        self.inner.lock().leaf_index_of(pane_id)
    }

    pub fn get_title(&self) -> String {
        self.inner.lock().title.clone()
    }

    pub fn set_title(&self, title: &str) {
        let mut inner = self.inner.lock();
        if inner.title != title {
            inner.title = title.to_string();
            Mux::try_get().map(|mux| {
                mux.notify(MuxNotification::TabTitleChanged {
                    tab_id: inner.id,
                    title: title.to_string(),
                })
            });
        }
    }

    /// Called by the multiplexer client when building a local tab to
    /// mirror a remote tab.  The supplied `root` is the information
    /// about our counterpart in the remote server.
    /// This method builds a local tree based on the remote tree which
    /// then replaces the local tree structure.
    ///
    /// The `make_pane` function is provided by the caller, and its purpose
    /// is to lookup an existing Pane that corresponds to the provided
    /// PaneEntry, or to create a new Pane from that entry.
    /// make_pane is expected to add the pane to the mux if it creates
    /// a new pane, otherwise the pane won't poll/update in the GUI.
    pub fn sync_with_pane_tree<F>(&self, size: TerminalSize, root: PaneNode, make_pane: F)
    where
        F: FnMut(PaneEntry) -> Arc<dyn Pane>,
    {
        self.inner.lock().sync_with_pane_tree(size, root, make_pane)
    }

    pub fn codec_pane_tree(&self) -> PaneNode {
        self.inner.lock().codec_pane_tree()
    }

    /// Returns a count of how many panes are in this tab
    pub fn count_panes(&self) -> Option<usize> {
        self.inner.try_lock().map(|inner| inner.count_panes())
    }

    /// Sets the zoom state, returns the prior state
    pub fn set_zoomed(&self, zoomed: bool) -> bool {
        self.inner.lock().set_zoomed(zoomed)
    }

    pub fn toggle_zoom(&self) {
        self.inner.lock().toggle_zoom()
    }

    /// True if `pane_id` is a member, visible or hidden, of the zoomed
    /// leaf. False while not zoomed.
    pub fn zoomed_leaf_contains(&self, pane_id: PaneId) -> bool {
        self.inner.lock().zoomed_leaf_contains(pane_id)
    }

    /// Applies a request to zoom (`zoomed`) or unzoom `pane`. Zoom belongs
    /// to a leaf: asking to zoom a member of the zoomed leaf brings it to
    /// the front and keeps the zoom, and asking to unzoom any member of it
    /// unzooms. A request about a pane outside the zoomed leaf zooms that
    /// pane's leaf, or does nothing if it asks to unzoom.
    pub fn set_pane_zoomed(&self, pane: &Arc<dyn Pane>, zoomed: bool) {
        self.inner.lock().set_pane_zoomed(pane, zoomed)
    }

    pub fn contains_pane(&self, pane: PaneId) -> bool {
        self.inner.lock().contains_pane(pane)
    }

    /// True if `pane_id` is the visible member of one of the leaves.
    /// Ignores zoom, as `contains_pane` does.
    pub fn contains_visible_pane(&self, pane_id: PaneId) -> bool {
        self.inner.lock().contains_visible_pane(pane_id)
    }

    pub fn iter_panes(&self) -> Vec<PositionedPane> {
        self.inner.lock().iter_panes()
    }

    /// One entry per leaf, with the position and size of its slot. While
    /// zoomed, only the zoomed leaf is returned and its slot is the whole
    /// tab.
    pub fn iter_stacks(&self) -> Vec<PositionedStack> {
        self.inner.lock().iter_stacks()
    }

    pub fn iter_panes_ignoring_zoom(&self) -> Vec<PositionedPane> {
        self.inner.lock().iter_panes_ignoring_zoom()
    }

    pub fn rotate_counter_clockwise(&self) {
        self.inner.lock().rotate_counter_clockwise()
    }

    pub fn rotate_clockwise(&self) {
        self.inner.lock().rotate_clockwise()
    }

    pub fn iter_splits(&self) -> Vec<PositionedSplit> {
        self.inner.lock().iter_splits()
    }

    pub fn tab_id(&self) -> TabId {
        self.tab_id
    }

    pub fn get_size(&self) -> TerminalSize {
        self.inner.lock().get_size()
    }

    /// Apply the new size of the tab to the panes contained within.
    /// The delta between the current and the new size is computed,
    /// and is distributed between the splits.  For small resizes
    /// this algorithm biases towards adjusting the left/top nodes
    /// first.  For large resizes this tends to proportionally adjust
    /// the relative sizes of the elements in a split.
    pub fn resize(&self, size: TerminalSize) {
        self.inner.lock().resize(size)
    }

    /// Called when running in the mux server after an individual pane
    /// has been resized.
    /// Because the split manipulation happened on the GUI we "lost"
    /// the information that would have allowed us to call resize_split_by()
    /// and instead need to back-infer the split size information.
    /// We rely on the client to have resized (or be in the process
    /// of resizing) affected panes consistently with its own Tab
    /// tree model.
    /// This method does a simple tree walk to the leaves to back-propagate
    /// the size of the panes up to their containing node split data.
    /// Without this step, disconnecting and reconnecting would cause
    /// the GUI to use stale size information for the window it spawns
    /// to attach this tab.
    pub fn rebuild_splits_sizes_from_contained_panes(&self) {
        self.inner
            .lock()
            .rebuild_splits_sizes_from_contained_panes()
    }

    /// Given split_index, the topological index of a split returned by
    /// iter_splits() as PositionedSplit::index, revised the split position
    /// by the provided delta; positive values move the split to the right/bottom,
    /// and negative values to the left/top.
    /// The adjusted size is propogated downwards to contained children and
    /// their panes are resized accordingly.
    pub fn resize_split_by(&self, split_index: usize, delta: isize) {
        self.inner.lock().resize_split_by(split_index, delta)
    }

    /// Adjusts the size of the active pane in the specified direction
    /// by the specified amount.
    pub fn adjust_pane_size(&self, direction: PaneDirection, amount: usize) {
        self.inner.lock().adjust_pane_size(direction, amount)
    }

    /// Activate an adjacent pane in the specified direction.
    /// In cases where there are multiple adjacent panes in the
    /// intended direction, we take the pane that has the largest
    /// edge intersection.
    pub fn activate_pane_direction(&self, direction: PaneDirection) {
        self.inner.lock().activate_pane_direction(direction)
    }

    /// Returns an adjacent pane in the specified direction.
    /// In cases where there are multiple adjacent panes in the
    /// intended direction, we take the pane that has the largest
    /// edge intersection.
    pub fn get_pane_direction(&self, direction: PaneDirection, ignore_zoom: bool) -> Option<usize> {
        self.inner.lock().get_pane_direction(direction, ignore_zoom)
    }

    pub fn prune_dead_panes(&self) -> bool {
        self.inner.lock().prune_dead_panes()
    }

    pub fn kill_pane(&self, pane_id: PaneId) -> bool {
        self.inner.lock().kill_pane(pane_id)
    }

    pub fn kill_panes_in_domain(&self, domain: DomainId) -> bool {
        self.inner.lock().kill_panes_in_domain(domain)
    }

    /// Remove pane from tab.
    /// The pane is still live in the mux; the intent is for the pane to
    /// be added to a different tab.
    pub fn remove_pane(&self, pane_id: PaneId) -> Option<Arc<dyn Pane>> {
        self.inner.lock().remove_pane(pane_id)
    }

    pub fn can_close_without_prompting(&self, reason: CloseReason) -> bool {
        self.inner.lock().can_close_without_prompting(reason)
    }

    pub fn is_dead(&self) -> bool {
        self.inner.lock().is_dead()
    }

    pub fn get_active_pane(&self) -> Option<Arc<dyn Pane>> {
        self.inner.lock().get_active_pane()
    }

    #[allow(unused)]
    pub fn get_active_idx(&self) -> usize {
        self.inner.lock().get_active_idx()
    }

    /// Activates the leaf holding `pane`, first making `pane` its visible
    /// member if it is hidden. Within the zoomed leaf the zoom stays and
    /// moves to `pane`.
    pub fn set_active_pane(&self, pane: &Arc<dyn Pane>) {
        self.inner.lock().set_active_pane(pane)
    }

    pub fn set_active_idx(&self, pane_index: usize) {
        self.inner.lock().set_active_idx(pane_index)
    }

    /// Activates the member `delta` places along from the visible one in
    /// the active leaf, wrapping around at either end. Does nothing for a
    /// single-member stack.
    pub fn activate_stack_relative(&self, delta: isize) {
        self.inner.lock().activate_stack_relative(delta)
    }

    /// Adds `pane` to the stack of the leaf holding `target_pane_id`, at
    /// `index` (clamped), or right after the visible member for `None`.
    /// The new member becomes visible and is sized to the leaf's members;
    /// in the zoomed leaf that is the zoom size, and the zoom moves to it.
    /// Fails, leaving the tab untouched, if the tab has no header rows,
    /// `target_pane_id` is not in it, `pane` already is, or `pane` refuses
    /// its size.
    pub fn add_pane_to_stack(
        &self,
        target_pane_id: PaneId,
        index: Option<usize>,
        pane: Arc<dyn Pane>,
    ) -> anyhow::Result<()> {
        self.inner
            .lock()
            .add_pane_to_stack(target_pane_id, index, pane)
    }

    /// Moves `pane_id` into the stack of the leaf holding `target_pane_id`,
    /// at `index` as counted after `pane_id` has been taken out (clamped).
    /// The moved pane ends up visible. Within one leaf this just reorders;
    /// across leaves the target leaf becomes active and a source leaf left
    /// empty is unsplit. Fails if the tab has no header rows or either pane
    /// is not in it.
    pub fn move_pane_to_stack(
        &self,
        pane_id: PaneId,
        target_pane_id: PaneId,
        index: usize,
    ) -> anyhow::Result<()> {
        self.inner
            .lock()
            .move_pane_to_stack(pane_id, target_pane_id, index)
    }

    /// Assigns the root pane.
    /// This is suitable when creating a new tab and then assigning
    /// the initial pane
    pub fn assign_pane(&self, pane: &Arc<dyn Pane>) {
        self.inner.lock().assign_pane(pane)
    }

    /// Swap the active pane with the specified pane_index
    pub fn swap_active_with_index(&self, pane_index: usize, keep_focus: bool) -> Option<()> {
        self.inner
            .lock()
            .swap_active_with_index(pane_index, keep_focus)
    }

    /// Computes the size of the pane that would result if the specified
    /// pane was split in a particular direction.
    /// The intent is to call this prior to spawning the new pane so that
    /// you can create it with the correct size.
    /// May return None if the specified pane_index is invalid.
    pub fn compute_split_size(
        &self,
        pane_index: usize,
        request: SplitRequest,
    ) -> Option<SplitDirectionAndSize> {
        self.inner.lock().compute_split_size(pane_index, request)
    }

    /// Split the pane that has pane_index in the given direction and assign
    /// the right/bottom pane of the newly created split to the provided Pane
    /// instance.  Returns the resultant index of the newly inserted pane.
    /// Both the split and the inserted pane will be resized.
    pub fn split_and_insert(
        &self,
        pane_index: usize,
        request: SplitRequest,
        pane: Arc<dyn Pane>,
    ) -> anyhow::Result<usize> {
        self.inner
            .lock()
            .split_and_insert(pane_index, request, pane)
    }

    pub fn get_zoomed_pane(&self) -> Option<Arc<dyn Pane>> {
        self.inner.lock().get_zoomed_pane()
    }
}

impl TabInner {
    fn new(size: &TerminalSize, header_rows: usize) -> Self {
        Self {
            id: TAB_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed),
            pane: Some(Tree::new()),
            size: *size,
            size_before_zoom: *size,
            active: 0,
            zoomed: None,
            title: String::new(),
            recency: Recency::default(),
            header_rows,
        }
    }

    fn sync_with_pane_tree<F>(&mut self, size: TerminalSize, root: PaneNode, mut make_pane: F)
    where
        F: FnMut(PaneEntry) -> Arc<dyn Pane>,
    {
        let mut active = None;
        let mut zoomed = None;

        log::debug!("sync_with_pane_tree with size {:?}", size);

        // Must be set before the tree is built and resized, because the
        // members' sizes are derived from it.
        let wire_header_rows = root.header_rows().unwrap_or(0);
        if wire_header_rows > MAX_HEADER_ROWS {
            log::warn!(
                "sync_with_pane_tree: header_rows {} out of range, using {}",
                wire_header_rows,
                MAX_HEADER_ROWS
            );
        }
        self.header_rows = wire_header_rows.min(MAX_HEADER_ROWS);

        let t = build_from_pane_tree(root.into_tree(), &mut active, &mut zoomed, &mut make_pane);
        let mut cursor = t.cursor();

        self.active = 0;
        if let Some(active) = active {
            // Resolve the active pane to its index
            let mut index = 0;
            loop {
                if let Some(stack) = cursor.leaf_mut() {
                    if stack.position_of(active.pane_id()).is_some() {
                        // Found it
                        self.active = index;
                        self.recency.tag(index);
                        break;
                    }
                    index += 1;
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(c) => {
                        // Didn't find it
                        cursor = c;
                        break;
                    }
                }
            }
        }
        self.pane.replace(cursor.tree());
        self.zoomed = zoomed;
        self.size = size;

        self.resize(size);

        log::debug!(
            "sync tab: {:#?} zoomed: {} {:#?}",
            size,
            self.zoomed.is_some(),
            self.iter_panes()
        );
        assert!(self.pane.is_some());
    }

    fn codec_pane_tree(&mut self) -> PaneNode {
        let mux = Mux::get();
        let tab_id = self.id;
        let window_id = match mux.window_containing_tab(tab_id) {
            Some(w) => w,
            None => {
                log::error!("no window contains tab {}", tab_id);
                return PaneNode::Empty;
            }
        };

        let workspace = match mux
            .get_window(window_id)
            .map(|w| w.get_workspace().to_string())
        {
            Some(ws) => ws,
            None => {
                log::error!("window id {} doesn't have a window!?", window_id);
                return PaneNode::Empty;
            }
        };

        let active = self.get_active_pane();
        let zoomed = self.zoomed.as_ref();
        if let Some(root) = self.pane.as_ref() {
            pane_tree(
                root,
                tab_id,
                window_id,
                active.as_ref(),
                zoomed,
                &workspace,
                0,
                0,
                self.header_rows,
            )
        } else {
            PaneNode::Empty
        }
    }

    /// Returns a count of how many panes are in this tab, hidden stack
    /// members included
    fn count_panes(&self) -> usize {
        match &self.pane {
            Some(root) => leaves(root).iter().map(|stack| stack.len()).sum(),
            None => 0,
        }
    }

    fn iter_all_panes(&self) -> Vec<Arc<dyn Pane>> {
        match &self.pane {
            Some(root) => leaves(root)
                .into_iter()
                .flat_map(|stack| stack.panes().iter().cloned())
                .collect(),
            None => vec![],
        }
    }

    fn leaf_index_of(&self, pane_id: PaneId) -> Option<usize> {
        let root = self.pane.as_ref()?;
        leaves(root)
            .iter()
            .position(|stack| stack.position_of(pane_id).is_some())
    }

    /// Runs `f` on the stack of leaf `leaf`; `None` if there is no such leaf.
    fn with_leaf_at<R>(&mut self, leaf: usize, f: impl FnOnce(&mut PaneStack) -> R) -> Option<R> {
        let root = self.pane.as_mut()?;
        leaves_mut(root).into_iter().nth(leaf).map(f)
    }

    /// The slot size of leaf `leaf` in the split layout, ignoring zoom.
    fn slot_of_leaf(&mut self, leaf: usize) -> Option<TerminalSize> {
        let root_size = self.size;
        let cursor = self.pane.take()?.cursor();
        match cursor.go_to_nth_leaf(leaf) {
            Ok(c) => {
                let (_, _, slot) = slot_geometry(&c, root_size);
                self.pane.replace(c.tree());
                Some(slot)
            }
            Err(c) => {
                self.pane.replace(c.tree());
                None
            }
        }
    }

    /// Makes `pane_id` the visible member of leaf `leaf`; does nothing if
    /// it is not a member there. Every change of a stack's visible member
    /// goes through here, so that whatever has to follow such a change is
    /// done in one place. The one exception is removal: `remove_where`
    /// picks the successor itself, and by the time this runs the removed
    /// member is gone from the tree, so `remove_pane_if` hands the zoom
    /// over on its own and only calls this afterwards for the focus rule.
    ///
    /// If `leaf` is the zoomed leaf, the zoom stays and moves to the new
    /// visible member, so that `zoomed` never names a hidden member. All
    /// members of that leaf already have the zoom size. Neither member is
    /// told about it through `Pane::set_zoomed`: on a `ClientPane` that
    /// sends the server a request to zoom or unzoom.
    ///
    /// The member pushed out of view, and the visible member of a leaf
    /// other than the active one, are told directly that they are not
    /// focused: a terminal starts out focused and would otherwise never
    /// flag unseen output. Nobody is told they gained focus and no
    /// `PaneFocused` is sent: the GUI's per-frame focus recording settles
    /// the foreground tab, and a `PaneFocused` from a background tab would
    /// make the GUI switch the window over to it. Callers that move focus
    /// on purpose follow up with `advise_focus_change`, and must set
    /// `self.active` before calling this so the pane they focus is not
    /// told the opposite first.
    fn show_member(&mut self, leaf: usize, pane_id: PaneId) {
        let active = self.active;
        let zoomed_id = self.zoomed.as_ref().map(|p| p.pane_id());
        let shown = self
            .with_leaf_at(leaf, |stack| {
                let index = stack.position_of(pane_id)?;
                let before = Arc::clone(stack.visible_pane());
                stack.set_visible(index);
                let holds_zoom = zoomed_id.map_or(false, |id| stack.position_of(id).is_some());
                Some((before, Arc::clone(stack.visible_pane()), holds_zoom))
            })
            .flatten();
        let (before, now, holds_zoom) = match shown {
            Some(shown) => shown,
            None => return,
        };
        if holds_zoom {
            self.zoomed.replace(Arc::clone(&now));
        }
        if before.pane_id() != now.pane_id() {
            before.focus_changed(false);
        }
        if leaf != active {
            now.focus_changed(false);
        }
    }

    /// Runs `f` on the stack of leaf `leaf`, then makes the member that was
    /// visible before visible again. `PaneStack::insert` and `take_at` pick
    /// a new visible member on their own; undoing that leaves the change to
    /// `show_member`, which needs to see the member being replaced.
    fn edit_stack<R>(&mut self, leaf: usize, f: impl FnOnce(&mut PaneStack) -> R) -> Option<R> {
        self.with_leaf_at(leaf, |stack| {
            let shown = stack.visible_pane().pane_id();
            let result = f(stack);
            if let Some(index) = stack.position_of(shown) {
                stack.set_visible(index);
            }
            result
        })
    }

    /// Zoom makes the whole tab the slot of the leaf holding `zoomed`, so
    /// every member of that leaf, hidden ones included, is sized for it.
    fn resize_zoomed_leaf(&mut self, zoomed: &Arc<dyn Pane>, size: TerminalSize) {
        let member = member_size(&size, self.header_rows);
        let resized = self.leaf_index_of(zoomed.pane_id()).and_then(|leaf| {
            self.with_leaf_at(leaf, |stack| {
                stack.resize_all(member).ok();
            })
        });
        if resized.is_none() {
            // Not in the tree; still resize it, as was done before stacks.
            zoomed.resize(member).ok();
        }
    }

    /// Sets the zoom state, returns the prior state
    fn set_zoomed(&mut self, zoomed: bool) -> bool {
        if self.zoomed.is_some() == zoomed {
            // Current zoom state matches intended zoom state,
            // so we have nothing to do.
            return zoomed;
        }
        self.toggle_zoom();
        !zoomed
    }

    fn toggle_zoom(&mut self) {
        let size = self.size;
        if self.zoomed.take().is_some() {
            // We were zoomed, but now we are not.
            // Re-apply the size to the panes
            if let Some(pane) = self.get_active_pane() {
                pane.set_zoomed(false);
            }
            self.size = self.size_before_zoom;
            self.resize(size);
        } else {
            // We weren't zoomed, but now we want to zoom.
            // Locate the active pane
            self.size_before_zoom = size;
            if let Some(pane) = self.get_active_pane() {
                // set_zoomed must reach a ClientPane's server before the
                // resize does: a Resize arriving while the server is not yet
                // zoomed gets written back into its splits as the whole tab
                // size, which then also poisons size_before_zoom.
                pane.set_zoomed(true);
                self.resize_zoomed_leaf(&pane, size);
                self.zoomed.replace(pane);
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn zoomed_leaf_contains(&self, pane_id: PaneId) -> bool {
        let (zoomed_id, root) = match (&self.zoomed, &self.pane) {
            (Some(zoomed), Some(root)) => (zoomed.pane_id(), root),
            _ => return false,
        };
        leaves(root).iter().any(|stack| {
            stack.position_of(zoomed_id).is_some() && stack.position_of(pane_id).is_some()
        })
    }

    fn set_pane_zoomed(&mut self, pane: &Arc<dyn Pane>, zoomed: bool) {
        // With no header rows every leaf holds a single pane, and these arms
        // do what the mux server's SetPaneZoomed handler did before zoom
        // went per leaf: the first arm is then a no-op, as set_active_pane
        // returns early for the pane that is already zoomed.
        match (self.zoomed_leaf_contains(pane.pane_id()), zoomed) {
            (true, true) => self.set_active_pane(pane),
            (true, false) => {
                self.set_zoomed(false);
            }
            (false, true) => {
                self.set_zoomed(false);
                self.set_active_pane(pane);
                self.set_zoomed(true);
            }
            (false, false) => {}
        }
    }

    /// True if any stack holds `pane`, as a visible or hidden member.
    fn contains_pane(&self, pane: PaneId) -> bool {
        self.leaf_index_of(pane).is_some()
    }

    fn contains_visible_pane(&self, pane_id: PaneId) -> bool {
        match &self.pane {
            Some(root) => leaves(root)
                .iter()
                .any(|stack| stack.visible_pane().pane_id() == pane_id),
            None => false,
        }
    }

    /// Walks the pane tree to produce the topologically ordered flattened
    /// list of PositionedPane instances along with their positioning information.
    fn iter_panes(&mut self) -> Vec<PositionedPane> {
        self.iter_panes_impl(true)
    }

    /// Like iter_panes, except that it will include all panes, regardless of
    /// whether one of them is currently zoomed.
    fn iter_panes_ignoring_zoom(&mut self) -> Vec<PositionedPane> {
        self.iter_panes_impl(false)
    }

    /// Each leaf takes the stack of the leaf after it, and the last leaf
    /// takes the first stack. Whole stacks move; the active leaf index stays.
    fn rotate_counter_clockwise(&mut self) {
        {
            let mut stacks = match self.pane.as_mut() {
                Some(root) => leaves_mut(root),
                None => return,
            };
            if stacks.is_empty() {
                return;
            }
            // Bubble the first stack along to the last leaf.
            for i in 1..stacks.len() {
                swap_stacks(&mut stacks, i - 1, i);
            }
        }
        let size = self.size;
        apply_sizes_from_splits(self.pane.as_ref().unwrap(), &size, self.header_rows);
    }

    /// Each leaf takes the stack of the leaf before it, and the first leaf
    /// takes the last stack. Whole stacks move; the active leaf index stays.
    fn rotate_clockwise(&mut self) {
        {
            let mut stacks = match self.pane.as_mut() {
                Some(root) => leaves_mut(root),
                None => return,
            };
            if stacks.is_empty() {
                return;
            }
            // Bubble the last stack back to the first leaf.
            for i in (1..stacks.len()).rev() {
                swap_stacks(&mut stacks, i - 1, i);
            }
        }
        let size = self.size;
        apply_sizes_from_splits(self.pane.as_ref().unwrap(), &size, self.header_rows);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    /// One entry per leaf, describing its visible member. `top`/`height`
    /// are those of the member itself, below the leaf's header rows.
    fn iter_panes_impl(&mut self, respect_zoom_state: bool) -> Vec<PositionedPane> {
        let mut panes = vec![];
        let header_rows = self.header_rows;

        if respect_zoom_state {
            if let Some(zoomed) = self.zoomed.as_ref() {
                // The zoomed leaf's slot is the whole tab and it keeps its
                // header rows at the top.
                let size = member_size(&self.size, header_rows);
                panes.push(PositionedPane {
                    index: 0,
                    is_active: true,
                    is_zoomed: true,
                    left: 0,
                    top: header_rows,
                    width: size.cols.into(),
                    pixel_width: size.pixel_width.into(),
                    height: size.rows.into(),
                    pixel_height: size.pixel_height.into(),
                    pane: Arc::clone(zoomed),
                });
                return panes;
            }
        }

        let active_idx = self.active;
        let zoomed_id = self.zoomed.as_ref().map(|p| p.pane_id());
        let root_size = self.size;
        let mut cursor = self.pane.take().unwrap().cursor();

        loop {
            if cursor.is_leaf() {
                let index = panes.len();
                let (left, top, slot) = slot_geometry(&cursor, root_size);
                let pane = Arc::clone(cursor.leaf_mut().unwrap().visible_pane());
                let dims = member_size(&slot, header_rows);

                panes.push(PositionedPane {
                    index,
                    is_active: index == active_idx,
                    is_zoomed: zoomed_id == Some(pane.pane_id()),
                    left,
                    top: top + header_rows,
                    width: dims.cols as _,
                    height: dims.rows as _,
                    pixel_width: dims.pixel_width as _,
                    pixel_height: dims.pixel_height as _,
                    pane,
                });
            }

            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }

        panes
    }

    fn iter_stacks(&mut self) -> Vec<PositionedStack> {
        let root_size = self.size;

        if let Some(zoomed) = self.zoomed.as_ref() {
            let zoomed_id = zoomed.pane_id();
            let found = self.pane.as_ref().and_then(|root| {
                leaves(root)
                    .into_iter()
                    .enumerate()
                    .find(|(_, stack)| stack.position_of(zoomed_id).is_some())
                    .map(|(index, stack)| (index, stack.panes().to_vec(), stack.visible_index()))
            });
            if let Some((index, panes, visible)) = found {
                // The real leaf index is kept here, where iter_panes reports
                // 0 for the zoomed pane: the tab strip has to name its leaf.
                return vec![PositionedStack {
                    index,
                    is_active: true,
                    is_zoomed: true,
                    left: 0,
                    top: 0,
                    width: root_size.cols,
                    height: root_size.rows,
                    pixel_width: root_size.pixel_width,
                    pixel_height: root_size.pixel_height,
                    panes,
                    visible,
                }];
            }
        }

        let mut stacks = vec![];
        let active_idx = self.active;
        let mut cursor = match self.pane.take() {
            Some(root) => root.cursor(),
            None => return stacks,
        };

        loop {
            if cursor.is_leaf() {
                let index = stacks.len();
                let (left, top, slot) = slot_geometry(&cursor, root_size);
                if let Some(stack) = cursor.leaf_mut() {
                    stacks.push(PositionedStack {
                        index,
                        is_active: index == active_idx,
                        is_zoomed: false,
                        left,
                        top,
                        width: slot.cols,
                        height: slot.rows,
                        pixel_width: slot.pixel_width,
                        pixel_height: slot.pixel_height,
                        panes: stack.panes().to_vec(),
                        visible: stack.visible_index(),
                    });
                }
            }

            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }

        stacks
    }

    fn iter_splits(&mut self) -> Vec<PositionedSplit> {
        let mut dividers = vec![];
        if self.zoomed.is_some() {
            return dividers;
        }

        let mut cursor = self.pane.take().unwrap().cursor();
        let mut index = 0;

        loop {
            if !cursor.is_leaf() {
                let mut left = 0usize;
                let mut top = 0usize;
                for (branch, p) in cursor.path_to_root() {
                    if let Some(p) = p {
                        if branch == PathBranch::IsRight {
                            left += p.left_of_second();
                            top += p.top_of_second();
                        }
                    }
                }
                if let Ok(Some(node)) = cursor.node_mut() {
                    match node.direction {
                        SplitDirection::Horizontal => left += node.first.cols as usize,
                        SplitDirection::Vertical => top += node.first.rows as usize,
                    }

                    dividers.push(PositionedSplit {
                        index,
                        direction: node.direction,
                        left,
                        top,
                        size: if node.direction == SplitDirection::Horizontal {
                            node.height() as usize
                        } else {
                            node.width() as usize
                        },
                    })
                }
                index += 1;
            }

            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }

        dividers
    }

    fn get_size(&self) -> TerminalSize {
        self.size
    }

    fn resize(&mut self, size: TerminalSize) {
        if size.rows == 0 || size.cols == 0 {
            // Ignore "impossible" resize requests
            return;
        }

        if let Some(zoomed) = self.zoomed.clone() {
            self.size = size;
            self.resize_zoomed_leaf(&zoomed, size);
        } else {
            let dims = cell_dimensions(&size);
            let header_rows = self.header_rows;
            let (min_x, min_y) = compute_min_size(self.pane.as_mut().unwrap(), header_rows);
            let current_size = self.size;

            // Constrain the new size to the minimum possible dimensions
            let cols = size.cols.max(min_x);
            let rows = size.rows.max(min_y);
            let size = TerminalSize {
                rows,
                cols,
                pixel_width: cols * dims.pixel_width,
                pixel_height: rows * dims.pixel_height,
                dpi: dims.dpi,
            };

            // Update the split nodes with adjusted sizes
            adjust_x_size(
                self.pane.as_mut().unwrap(),
                cols as isize - current_size.cols as isize,
                &dims,
                header_rows,
            );
            adjust_y_size(
                self.pane.as_mut().unwrap(),
                rows as isize - current_size.rows as isize,
                &dims,
                header_rows,
            );

            self.size = size;

            // And then resize the individual panes to match
            apply_sizes_from_splits(self.pane.as_mut().unwrap(), &size, header_rows);
        }

        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn apply_pane_size(&mut self, pane_size: TerminalSize, cursor: &mut Cursor) {
        let cell_width = pane_size
            .pixel_width
            .checked_div(pane_size.cols)
            .unwrap_or(1);
        let cell_height = pane_size
            .pixel_height
            .checked_div(pane_size.rows)
            .unwrap_or(1);
        if let Ok(Some(node)) = cursor.node_mut() {
            // Adjust the size of the node; we preserve the size of the first
            // child and adjust the second, so if we are split down the middle
            // and the window is made wider, the right column will grow in
            // size, leaving the left at its current width.
            if node.direction == SplitDirection::Horizontal {
                node.first.rows = pane_size.rows;
                node.second.rows = pane_size.rows;

                node.second.cols = pane_size.cols.saturating_sub(1 + node.first.cols);
            } else {
                node.first.cols = pane_size.cols;
                node.second.cols = pane_size.cols;

                node.second.rows = pane_size.rows.saturating_sub(1 + node.first.rows);
            }
            node.first.pixel_width = node.first.cols * cell_width;
            node.first.pixel_height = node.first.rows * cell_height;

            node.second.pixel_width = node.second.cols * cell_width;
            node.second.pixel_height = node.second.rows * cell_height;
        }
    }

    fn rebuild_splits_sizes_from_contained_panes(&mut self) {
        if self.zoomed.is_some() {
            return;
        }

        fn compute_size(node: &mut Tree, header_rows: usize) -> Option<TerminalSize> {
            match node {
                Tree::Empty => None,
                Tree::Leaf(stack) => {
                    // All members share one size; the slot adds the header
                    // rows back on top of it.
                    let dims = stack.visible_pane().get_dimensions();
                    let size = TerminalSize {
                        cols: dims.cols,
                        rows: dims.viewport_rows,
                        pixel_height: dims.pixel_height,
                        pixel_width: dims.pixel_width,
                        dpi: dims.dpi,
                    };
                    Some(slot_size(&size, header_rows))
                }
                Tree::Node { left, right, data } => {
                    if let Some(data) = data {
                        if let Some(first) = compute_size(left, header_rows) {
                            data.first = first;
                        }
                        if let Some(second) = compute_size(right, header_rows) {
                            data.second = second;
                        }
                        Some(data.size())
                    } else {
                        None
                    }
                }
            }
        }

        let header_rows = self.header_rows;
        if let Some(root) = self.pane.as_mut() {
            if let Some(size) = compute_size(root, header_rows) {
                self.size = size;
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn resize_split_by(&mut self, split_index: usize, delta: isize) {
        if self.zoomed.is_some() {
            return;
        }

        let mut cursor = self.pane.take().unwrap().cursor();
        let mut index = 0;

        // Position cursor on the specified split
        loop {
            if !cursor.is_leaf() {
                if index == split_index {
                    // Found it
                    break;
                }
                index += 1;
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    // Didn't find it
                    self.pane.replace(c.tree());
                    return;
                }
            }
        }

        // Now cursor is looking at the split
        self.adjust_node_at_cursor(&mut cursor, delta);
        self.cascade_size_from_cursor(cursor);
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn adjust_node_at_cursor(&mut self, cursor: &mut Cursor, delta: isize) {
        let cell_dimensions = self.cell_dimensions();
        let min_rows = min_leaf_rows(self.header_rows) as isize;
        if let Ok(Some(node)) = cursor.node_mut() {
            match node.direction {
                SplitDirection::Horizontal => {
                    let width = node.width();

                    let mut cols = node.first.cols as isize;
                    cols = cols
                        .saturating_add(delta)
                        .max(1)
                        .min((width as isize).saturating_sub(2));
                    node.first.cols = cols as usize;
                    node.first.pixel_width =
                        node.first.cols.saturating_mul(cell_dimensions.pixel_width);

                    node.second.cols = width.saturating_sub(node.first.cols.saturating_add(1));
                    node.second.pixel_width =
                        node.second.cols.saturating_mul(cell_dimensions.pixel_width);
                }
                SplitDirection::Vertical => {
                    let height = node.height();

                    // Each side keeps at least one member row below its
                    // header rows.
                    let mut rows = node.first.rows as isize;
                    rows = rows
                        .saturating_add(delta)
                        .max(min_rows)
                        .min((height as isize).saturating_sub(1 + min_rows));
                    node.first.rows = rows as usize;
                    node.first.pixel_height =
                        node.first.rows.saturating_mul(cell_dimensions.pixel_height);

                    node.second.rows = height.saturating_sub(node.first.rows.saturating_add(1));
                    node.second.pixel_height = node
                        .second
                        .rows
                        .saturating_mul(cell_dimensions.pixel_height);
                }
            }
        }
    }

    fn cascade_size_from_cursor(&mut self, mut cursor: Cursor) {
        // Now we need to cascade this down to children
        match cursor.preorder_next() {
            Ok(c) => cursor = c,
            Err(c) => {
                self.pane.replace(c.tree());
                return;
            }
        }
        let root_size = self.size;
        let header_rows = self.header_rows;

        loop {
            // Figure out the available size by looking at our immediate parent node.
            // If we are the root, look at the provided new size
            let pane_size = if let Some((branch, Some(parent))) = cursor.path_to_root().next() {
                if branch == PathBranch::IsRight {
                    parent.second
                } else {
                    parent.first
                }
            } else {
                root_size
            };

            if cursor.is_leaf() {
                // Apply our size to the tty
                cursor
                    .leaf_mut()
                    .map(|stack| stack.resize_all(member_size(&pane_size, header_rows)));
            } else {
                self.apply_pane_size(pane_size, &mut cursor);
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    break;
                }
            }
        }
        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
    }

    fn adjust_pane_size(&mut self, direction: PaneDirection, amount: usize) {
        if self.zoomed.is_some() {
            return;
        }
        let active_index = self.active;
        let mut cursor = self.pane.take().unwrap().cursor();
        let mut index = 0;

        // Position cursor on the active leaf
        loop {
            if cursor.is_leaf() {
                if index == active_index {
                    // Found it
                    break;
                }
                index += 1;
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(c) => {
                    // Didn't find it
                    self.pane.replace(c.tree());
                    return;
                }
            }
        }

        // We are on the active leaf.
        // Now we go up until we find the parent node that is
        // aligned with the desired direction.
        let split_direction = match direction {
            PaneDirection::Left | PaneDirection::Right => SplitDirection::Horizontal,
            PaneDirection::Up | PaneDirection::Down => SplitDirection::Vertical,
            PaneDirection::Next | PaneDirection::Prev => unreachable!(),
        };
        let delta = match direction {
            PaneDirection::Down | PaneDirection::Right => amount as isize,
            PaneDirection::Up | PaneDirection::Left => -(amount as isize),
            PaneDirection::Next | PaneDirection::Prev => unreachable!(),
        };
        loop {
            match cursor.go_up() {
                Ok(mut c) => {
                    if let Ok(Some(node)) = c.node_mut() {
                        if node.direction == split_direction {
                            self.adjust_node_at_cursor(&mut c, delta);
                            self.cascade_size_from_cursor(c);
                            return;
                        }
                    }

                    cursor = c;
                }

                Err(c) => {
                    self.pane.replace(c.tree());
                    return;
                }
            }
        }
    }

    fn activate_pane_direction(&mut self, direction: PaneDirection) {
        if self.zoomed.is_some() {
            if !configuration().unzoom_on_switch_pane {
                return;
            }
            self.toggle_zoom();
        }
        if let Some(panel_idx) = self.get_pane_direction(direction, false) {
            self.set_active_idx(panel_idx);
        }
        let mux = Mux::get();
        if let Some(window_id) = mux.window_containing_tab(self.id) {
            mux.notify(MuxNotification::WindowInvalidated(window_id));
        }
    }

    fn get_pane_direction(&mut self, direction: PaneDirection, ignore_zoom: bool) -> Option<usize> {
        let mut panes = if ignore_zoom {
            self.iter_panes_ignoring_zoom()
        } else {
            self.iter_panes()
        };

        // Adjacency is a property of the slots: a pane below another one
        // starts after the lower leaf's header rows, so compare the slots or
        // vertically stacked panes never find each other.
        let header_rows = self.header_rows;
        for pane in panes.iter_mut() {
            pane.top = pane.top.saturating_sub(header_rows);
            pane.height += header_rows;
        }

        let active = match panes.iter().find(|pane| pane.is_active) {
            Some(p) => p,
            None => {
                // No active pane somehow...
                return Some(0);
            }
        };

        if matches!(direction, PaneDirection::Next | PaneDirection::Prev) {
            let max_pane_id = panes.iter().map(|p| p.index).max().unwrap_or(active.index);

            return Some(if direction == PaneDirection::Next {
                if active.index == max_pane_id {
                    0
                } else {
                    active.index + 1
                }
            } else {
                if active.index == 0 {
                    max_pane_id
                } else {
                    active.index - 1
                }
            });
        }

        let mut best = None;

        let recency = &self.recency;

        fn edge_intersects(
            active_start: usize,
            active_size: usize,
            current_start: usize,
            current_size: usize,
        ) -> bool {
            intersects_range(
                &(active_start..active_start + active_size),
                &(current_start..current_start + current_size),
            )
        }

        for pane in &panes {
            let score = match direction {
                PaneDirection::Right => {
                    if pane.left == active.left + active.width + 1
                        && edge_intersects(active.top, active.height, pane.top, pane.height)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Left => {
                    if pane.left + pane.width + 1 == active.left
                        && edge_intersects(active.top, active.height, pane.top, pane.height)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Up => {
                    if pane.top + pane.height + 1 == active.top
                        && edge_intersects(active.left, active.width, pane.left, pane.width)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Down => {
                    if active.top + active.height + 1 == pane.top
                        && edge_intersects(active.left, active.width, pane.left, pane.width)
                    {
                        1 + recency.score(pane.index)
                    } else {
                        0
                    }
                }
                PaneDirection::Next | PaneDirection::Prev => unreachable!(),
            };

            if score > 0 {
                let target = match best.take() {
                    Some((best_score, best_pane)) if best_score > score => (best_score, best_pane),
                    _ => (score, pane),
                };
                best.replace(target);
            }
        }

        if let Some((_, target)) = best.take() {
            return Some(target.index);
        }
        None
    }

    fn prune_dead_panes(&mut self) -> bool {
        let mux = Mux::get();
        !self
            .remove_pane_if(
                |pane| {
                    // If the pane is no longer known to the mux, then its liveness
                    // state isn't guaranteed to be monitored or updated, so let's
                    // consider the pane effectively dead if it isn't in the mux.
                    // <https://github.com/wezterm/wezterm/issues/4030>
                    let in_mux = mux.get_pane(pane.pane_id()).is_some();
                    let dead = pane.is_dead();
                    log::trace!(
                        "prune_dead_panes: pane_id={} dead={} in_mux={}",
                        pane.pane_id(),
                        dead,
                        in_mux
                    );
                    dead || !in_mux
                },
                true,
            )
            .is_empty()
    }

    fn kill_pane(&mut self, pane_id: PaneId) -> bool {
        !self
            .remove_pane_if(|pane| pane.pane_id() == pane_id, true)
            .is_empty()
    }

    fn kill_panes_in_domain(&mut self, domain: DomainId) -> bool {
        !self
            .remove_pane_if(|pane| pane.domain_id() == domain, true)
            .is_empty()
    }

    fn remove_pane(&mut self, pane_id: PaneId) -> Option<Arc<dyn Pane>> {
        let panes = self.remove_pane_if(|pane| pane.pane_id() == pane_id, false);
        for pane in panes {
            return Some(pane);
        }
        None
    }

    /// Removes every pane, visible or hidden, for which `f` returns true.
    /// A leaf is only unsplit once its stack is empty.
    fn remove_pane_if<F>(&mut self, f: F, kill: bool) -> Vec<Arc<dyn Pane>>
    where
        F: Fn(&Arc<dyn Pane>) -> bool,
    {
        let mut dead_panes = vec![];
        let zoomed_pane = self.zoomed.as_ref().map(|p| p.pane_id());

        {
            let root_size = self.size;
            let header_rows = self.header_rows;
            let mut cursor = self.pane.take().unwrap().cursor();
            let mut pane_index = 0;
            let mut removed_indices = vec![];
            let cell_dims = self.cell_dimensions();
            // Set once a leaf has been unsplit; from then on the layout of
            // the rest of the tree has to be re-applied as we walk it.
            let mut layout_changed = false;
            // Members that took over from a removed visible member.
            let mut successors = vec![];

            loop {
                // Figure out the available size by looking at our immediate parent node.
                // If we are the root, look at the tab size
                let pane_size = if let Some((branch, Some(parent))) = cursor.path_to_root().next() {
                    if branch == PathBranch::IsRight {
                        parent.second
                    } else {
                        parent.first
                    }
                } else {
                    root_size
                };

                if cursor.is_leaf() {
                    let stack = cursor.leaf_mut().unwrap();
                    let shown_before = stack.visible_pane().pane_id();
                    let removed = stack.remove_where(&f);
                    let emptied = stack.is_empty();
                    if !emptied && stack.visible_pane().pane_id() != shown_before {
                        successors.push(stack.visible_pane().pane_id());
                    }
                    if let Some(zoomed_id) = zoomed_pane {
                        if removed.iter().any(|p| p.pane_id() == zoomed_id) {
                            if emptied {
                                // The zoomed leaf is going away, and the
                                // zoom with it.
                                self.zoomed.take();
                            } else {
                                // The leaf stays zoomed and the zoom passes
                                // to the member that took over, which already
                                // has the zoom size. This has to happen here:
                                // show_member, run below, no longer finds the
                                // removed pane in the tree to see it go.
                                // Pane::set_zoomed is left alone, as in
                                // show_member.
                                self.zoomed.replace(Arc::clone(stack.visible_pane()));
                            }
                        }
                    }
                    dead_panes.extend(removed);

                    if emptied {
                        removed_indices.push(pane_index);
                        let parent;
                        match cursor.unsplit_leaf() {
                            Ok((c, _empty_stack, p)) => {
                                parent = p.unwrap();
                                cursor = c;
                            }
                            Err(c) => {
                                // We might be the root, for example
                                if c.is_top() && c.is_leaf() {
                                    self.pane.replace(Tree::Empty);
                                } else {
                                    self.pane.replace(c.tree());
                                }
                                break;
                            }
                        };
                        layout_changed = true;

                        // Now we need to increase the size of the current node
                        // and propagate the revised size to its children.
                        let size = TerminalSize {
                            rows: parent.height(),
                            cols: parent.width(),
                            pixel_width: cell_dims.pixel_width * parent.width(),
                            pixel_height: cell_dims.pixel_height * parent.height(),
                            dpi: cell_dims.dpi,
                        };

                        if let Some(unsplit) = cursor.leaf_mut() {
                            unsplit.resize_all(member_size(&size, header_rows)).ok();
                        } else {
                            self.apply_pane_size(size, &mut cursor);
                        }
                    } else if layout_changed {
                        // Apply our revised size to the tty
                        stack.resize_all(member_size(&pane_size, header_rows)).ok();
                    }

                    pane_index += 1;
                } else if layout_changed {
                    self.apply_pane_size(pane_size, &mut cursor);
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(c) => {
                        self.pane.replace(c.tree());
                        break;
                    }
                }
            }

            // Figure out which pane should now be active.
            // If panes earlier than the active pane were closed, then we
            // need to shift the active pane down
            let active_idx = self.active;
            removed_indices.retain(|&idx| idx <= active_idx);
            self.active = active_idx.saturating_sub(removed_indices.len());

            // remove_where already made each successor visible, and the zoom
            // was handed over above; show_member still runs for its focus
            // rule, which can only be applied once the tree is back in place
            // and the active leaf is settled. Header-less tabs have
            // single-member stacks and never get here.
            for pane_id in successors {
                if let Some(leaf) = self.leaf_index_of(pane_id) {
                    self.show_member(leaf, pane_id);
                }
            }
        }

        if !dead_panes.is_empty() && kill {
            let to_kill: Vec<_> = dead_panes.iter().map(|p| p.pane_id()).collect();
            promise::spawn::spawn_into_main_thread(async move {
                let mux = Mux::get();
                for pane_id in to_kill.into_iter() {
                    mux.remove_pane(pane_id);
                }
            })
            .detach();
        }
        dead_panes
    }

    fn can_close_without_prompting(&mut self, reason: CloseReason) -> bool {
        // Hidden stack members count too: closing the tab closes them.
        self.iter_all_panes()
            .iter()
            .all(|pane| pane.can_close_without_prompting(reason))
    }

    fn is_dead(&mut self) -> bool {
        // Make sure we account for all panes, so that we don't
        // kill the whole tab if the zoomed pane is dead!
        // Hidden stack members keep the tab alive just the same.
        self.iter_all_panes().iter().all(|pane| pane.is_dead())
    }

    /// The zoomed pane if any, else the visible member of the active leaf.
    fn get_active_pane(&mut self) -> Option<Arc<dyn Pane>> {
        if let Some(zoomed) = self.zoomed.as_ref() {
            return Some(Arc::clone(zoomed));
        }

        let root = self.pane.as_ref()?;
        leaves(root)
            .get(self.active)
            .map(|stack| Arc::clone(stack.visible_pane()))
    }

    fn get_active_idx(&self) -> usize {
        self.active
    }

    fn set_active_pane(&mut self, pane: &Arc<dyn Pane>) {
        let prior = self.get_active_pane();

        if is_pane(pane, &prior.as_ref()) {
            return;
        }

        let pane_id = pane.pane_id();
        // Switching to another member of the zoomed leaf is not switching
        // panes: show_member below hands the zoom over to it. Without
        // header rows the only member is the zoomed pane itself, which
        // returned above, so this behaves as before.
        if self.zoomed.is_some() && !self.zoomed_leaf_contains(pane_id) {
            if !configuration().unzoom_on_switch_pane {
                return;
            }
            self.toggle_zoom();
        }

        if let Some(leaf) = self.leaf_index_of(pane_id) {
            self.active = leaf;
            self.recency.tag(leaf);
            // Bring a hidden target forward. The leaf is already active, so
            // the target is not told it lost focus just before gaining it;
            // for a visible target this changes nothing.
            self.show_member(leaf, pane_id);
            self.advise_focus_change(prior);
        }
    }

    fn activate_stack_relative(&mut self, delta: isize) {
        let next = self
            .with_leaf_at(self.active, |stack| {
                let len = stack.len();
                if len < 2 {
                    return None;
                }
                // Reduce delta first so that adding it cannot overflow.
                let step = delta.rem_euclid(len as isize) as usize;
                let index = (stack.visible_index() + step) % len;
                stack.panes().get(index).cloned()
            })
            .flatten();
        if let Some(pane) = next {
            self.set_active_pane(&pane);
        }
    }

    fn add_pane_to_stack(
        &mut self,
        target_pane_id: PaneId,
        index: Option<usize>,
        pane: Arc<dyn Pane>,
    ) -> anyhow::Result<()> {
        let header_rows = self.header_rows;
        if header_rows == 0 {
            anyhow::bail!(
                "tab {} has no header rows, so it cannot stack panes",
                self.id
            );
        }
        let leaf = self
            .leaf_index_of(target_pane_id)
            .ok_or_else(|| anyhow::anyhow!("pane {} is not in tab {}", target_pane_id, self.id))?;
        let pane_id = pane.pane_id();
        if self.contains_pane(pane_id) {
            // A pane in two stacks would confuse leaf_index_of and closing.
            anyhow::bail!("pane {} is already in tab {}", pane_id, self.id);
        }
        // The zoomed leaf's slot is the whole tab, whatever the splits say.
        let slot = if self.zoomed_leaf_contains(target_pane_id) {
            self.size
        } else {
            self.slot_of_leaf(leaf)
                .ok_or_else(|| anyhow::anyhow!("tab {} has no leaf {}", self.id, leaf))?
        };
        // Every member of a stack has the same size. Size it before it joins,
        // so that a refusal leaves the tab as it was, as in split_and_insert.
        pane.resize(member_size(&slot, header_rows))?;

        self.edit_stack(leaf, |stack| {
            let index = index.unwrap_or(stack.visible_index() + 1);
            stack.insert(index, pane);
        });
        self.show_member(leaf, pane_id);

        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
        Ok(())
    }

    fn move_pane_to_stack(
        &mut self,
        pane_id: PaneId,
        target_pane_id: PaneId,
        index: usize,
    ) -> anyhow::Result<()> {
        let header_rows = self.header_rows;
        if header_rows == 0 {
            anyhow::bail!(
                "tab {} has no header rows, so it cannot stack panes",
                self.id
            );
        }
        let from_leaf = self
            .leaf_index_of(pane_id)
            .ok_or_else(|| anyhow::anyhow!("pane {} is not in tab {}", pane_id, self.id))?;
        let to_leaf = self
            .leaf_index_of(target_pane_id)
            .ok_or_else(|| anyhow::anyhow!("pane {} is not in tab {}", target_pane_id, self.id))?;

        if from_leaf == to_leaf {
            // A reorder: sizes and the active leaf stay as they are, and the
            // moved pane ends up as the visible member.
            self.edit_stack(to_leaf, |stack| {
                if let Some(pane) = stack.position_of(pane_id).and_then(|i| stack.take_at(i)) {
                    stack.insert(index, pane);
                }
            });
            self.show_member(to_leaf, pane_id);
        } else {
            self.set_zoomed(false);
            // The pane being moved may be the active one; take this before
            // it leaves, so that moving the active pane notifies nobody.
            let prior = self.get_active_pane();
            let pane = self
                .remove_pane(pane_id)
                .ok_or_else(|| anyhow::anyhow!("pane {} vanished from tab {}", pane_id, self.id))?;
            // Taking the pane out may have unsplit its leaf and renumbered
            // the leaves after it.
            let to_leaf = self.leaf_index_of(target_pane_id).ok_or_else(|| {
                anyhow::anyhow!("pane {} vanished from tab {}", target_pane_id, self.id)
            })?;
            // The pane has already left its old leaf, so failing to land it
            // must be reported rather than silently dropping the pane.
            self.edit_stack(to_leaf, |stack| {
                stack.insert(index, Arc::clone(&pane));
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no stack at leaf {} to receive pane {} in tab {}",
                    to_leaf,
                    pane_id,
                    self.id
                )
            })?;
            if let Some(slot) = self.slot_of_leaf(to_leaf) {
                // The pane has already left its old leaf, so a refusal can't
                // undo the move any more; it keeps its old size instead.
                if let Err(err) = pane.resize(member_size(&slot, header_rows)) {
                    log::error!("move_pane_to_stack: resizing pane {}: {:#}", pane_id, err);
                }
            }
            // Activate before showing: otherwise show_member would tell the
            // moved pane, which may be the focused one, that it lost focus,
            // and advise_focus_change sees no change to correct that.
            self.active = to_leaf;
            self.recency.tag(to_leaf);
            self.show_member(to_leaf, pane_id);
            self.advise_focus_change(prior);
        }

        Mux::try_get().map(|mux| mux.notify(MuxNotification::TabResized(self.id)));
        Ok(())
    }

    fn advise_focus_change(&mut self, prior: Option<Arc<dyn Pane>>) {
        // There is no Mux in unit tests; the panes are still told.
        let mux = Mux::try_get();
        let current = self.get_active_pane();
        match (prior, current) {
            (Some(prior), Some(current)) if prior.pane_id() != current.pane_id() => {
                prior.focus_changed(false);
                current.focus_changed(true);
                if let Some(mux) = &mux {
                    mux.notify(MuxNotification::PaneFocused(current.pane_id()));
                }
            }
            (None, Some(current)) => {
                current.focus_changed(true);
                if let Some(mux) = &mux {
                    mux.notify(MuxNotification::PaneFocused(current.pane_id()));
                }
            }
            (Some(prior), None) => {
                prior.focus_changed(false);
            }
            (Some(_), Some(_)) | (None, None) => {
                // no change
            }
        }
    }

    fn set_active_idx(&mut self, pane_index: usize) {
        let prior = self.get_active_pane();
        self.active = pane_index;
        self.recency.tag(pane_index);
        self.advise_focus_change(prior);
    }

    fn assign_pane(&mut self, pane: &Arc<dyn Pane>) {
        match Tree::new()
            .cursor()
            .assign_top(PaneStack::new(Arc::clone(pane)))
        {
            Ok(c) => self.pane = Some(c.tree()),
            Err(_) => panic!("tried to assign root pane to non-empty tree"),
        }
        // The caller sized the pane for the whole tab; make room for the
        // header rows. Header-less tabs skip this to keep the resize count
        // unchanged (a tmux pane forwards every resize to tmux).
        if self.header_rows > 0 {
            pane.resize(member_size(&self.size, self.header_rows)).ok();
        }
    }

    fn cell_dimensions(&self) -> TerminalSize {
        cell_dimensions(&self.size)
    }

    /// Swaps the whole stack of the active leaf with that of leaf
    /// `pane_index`.
    fn swap_active_with_index(&mut self, pane_index: usize, keep_focus: bool) -> Option<()> {
        let active_idx = self.get_active_idx();
        let prior = self.get_active_pane()?;
        log::trace!(
            "swap_active_with_index: pane_index {} active {}",
            pane_index,
            active_idx
        );

        {
            let mut stacks = leaves_mut(self.pane.as_mut()?);
            if pane_index >= stacks.len() {
                log::trace!("didn't find pane {pane_index}");
                return None;
            }
            if active_idx >= stacks.len() {
                log::trace!("didn't find active {active_idx}");
                return None;
            }
            swap_stacks(&mut stacks, pane_index, active_idx);
        }

        // Advise the panes of their new sizes
        let size = self.size;
        apply_sizes_from_splits(self.pane.as_ref().unwrap(), &size, self.header_rows);

        // And update focus
        if keep_focus {
            self.set_active_idx(pane_index);
        } else {
            self.advise_focus_change(Some(prior));
        }
        None
    }

    fn compute_split_size(
        &mut self,
        pane_index: usize,
        request: SplitRequest,
    ) -> Option<SplitDirectionAndSize> {
        let cell_dims = self.cell_dimensions();

        // `min` is the smallest size the new side may have.
        fn split_dimension(dim: usize, request: SplitRequest, min: usize) -> (usize, usize) {
            let target_size = match request.size {
                SplitSize::Cells(n) => n,
                SplitSize::Percent(n) => (dim * (n as usize)) / 100,
            }
            .max(min);

            let remain = dim.saturating_sub(target_size + 1);

            if request.target_is_second {
                (remain, target_size)
            } else {
                (target_size, remain)
            }
        }

        // A leaf needs a member row below its header rows.
        let min_rows = min_leaf_rows(self.header_rows);

        if request.top_level {
            let size = self.size;

            let ((width1, width2), (height1, height2)) = match request.direction {
                SplitDirection::Horizontal => (
                    split_dimension(size.cols as usize, request, 1),
                    (size.rows as usize, size.rows as usize),
                ),
                SplitDirection::Vertical => (
                    (size.cols as usize, size.cols as usize),
                    split_dimension(size.rows as usize, request, min_rows),
                ),
            };

            return Some(SplitDirectionAndSize {
                direction: request.direction,
                first: TerminalSize {
                    rows: height1 as _,
                    cols: width1 as _,
                    pixel_height: cell_dims.pixel_height * height1,
                    pixel_width: cell_dims.pixel_width * width1,
                    dpi: cell_dims.dpi,
                },
                second: TerminalSize {
                    rows: height2 as _,
                    cols: width2 as _,
                    pixel_height: cell_dims.pixel_height * height2,
                    pixel_width: cell_dims.pixel_width * width2,
                    dpi: cell_dims.dpi,
                },
            });
        }

        // Ensure that we're not zoomed, otherwise we'll end up in
        // a bogus split state (https://github.com/wezterm/wezterm/issues/723)
        self.set_zoomed(false);

        // Split the leaf's slot, header rows included, not just its member.
        self.slot_of_leaf(pane_index).map(|slot| {
            let ((width1, width2), (height1, height2)) = match request.direction {
                SplitDirection::Horizontal => (
                    split_dimension(slot.cols, request, 1),
                    (slot.rows, slot.rows),
                ),
                SplitDirection::Vertical => (
                    (slot.cols, slot.cols),
                    split_dimension(slot.rows, request, min_rows),
                ),
            };

            SplitDirectionAndSize {
                direction: request.direction,
                first: TerminalSize {
                    rows: height1 as _,
                    cols: width1 as _,
                    pixel_height: cell_dims.pixel_height * height1,
                    pixel_width: cell_dims.pixel_width * width1,
                    dpi: cell_dims.dpi,
                },
                second: TerminalSize {
                    rows: height2 as _,
                    cols: width2 as _,
                    pixel_height: cell_dims.pixel_height * height2,
                    pixel_width: cell_dims.pixel_width * width2,
                    dpi: cell_dims.dpi,
                },
            }
        })
    }

    fn split_and_insert(
        &mut self,
        pane_index: usize,
        request: SplitRequest,
        pane: Arc<dyn Pane>,
    ) -> anyhow::Result<usize> {
        if self.zoomed.is_some() {
            anyhow::bail!("cannot split while zoomed");
        }

        {
            let split_info = self
                .compute_split_size(pane_index, request)
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid pane_index {}; cannot split!", pane_index)
                })?;

            let tab_size = self.size;
            let header_rows = self.header_rows;
            // Each side needs a member row below its header rows.
            if split_info.first.rows < min_leaf_rows(header_rows)
                || split_info.first.cols == 0
                || split_info.second.rows < min_leaf_rows(header_rows)
                || split_info.second.cols == 0
                || split_info.top_of_second() + split_info.second.rows > tab_size.rows
                || split_info.left_of_second() + split_info.second.cols > tab_size.cols
            {
                log::error!(
                    "No space for split!!! {:#?} height={} width={} top_of_second={} left_of_second={} tab_size={:?}",
                    split_info,
                    split_info.height(),
                    split_info.width(),
                    split_info.top_of_second(),
                    split_info.left_of_second(),
                    tab_size
                );
                anyhow::bail!("No space for split!");
            }

            let needs_resize = if request.top_level {
                self.pane.as_ref().unwrap().num_leaves() > 1
            } else {
                false
            };

            if needs_resize {
                // Pre-emptively resize the tab contents down to
                // match the target size; it's easier to reuse
                // existing resize logic that way
                if request.target_is_second {
                    self.resize(split_info.first.clone());
                } else {
                    self.resize(split_info.second.clone());
                }
            }

            let mut cursor = self.pane.take().unwrap().cursor();

            if request.top_level && !cursor.is_leaf() {
                let result = if request.target_is_second {
                    cursor.split_node_and_insert_right(PaneStack::new(Arc::clone(&pane)))
                } else {
                    cursor.split_node_and_insert_left(PaneStack::new(Arc::clone(&pane)))
                };
                cursor = match result {
                    Ok(c) => {
                        cursor = match c.assign_node(Some(split_info)) {
                            Err(c) | Ok(c) => c,
                        };

                        // The caller sized the new pane for its whole slot.
                        // Header-less tabs skip this to keep the resize count
                        // unchanged.
                        if header_rows > 0 {
                            let slot = if request.target_is_second {
                                &split_info.second
                            } else {
                                &split_info.first
                            };
                            pane.resize(member_size(slot, header_rows)).ok();
                        }

                        self.pane.replace(cursor.tree());

                        let pane_index = if request.target_is_second {
                            self.pane.as_ref().unwrap().num_leaves().saturating_sub(1)
                        } else {
                            0
                        };

                        self.active = pane_index;
                        self.recency.tag(pane_index);
                        return Ok(pane_index);
                    }
                    Err(cursor) => cursor,
                };
            }

            match cursor.go_to_nth_leaf(pane_index) {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    anyhow::bail!("invalid pane_index {}; cannot split!", pane_index);
                }
            };

            let new_stack = PaneStack::new(pane);
            let first_member = member_size(&split_info.first, header_rows);
            let second_member = member_size(&split_info.second, header_rows);
            {
                // The whole existing stack moves into its new slot.
                let existing_stack = cursor.leaf_mut().unwrap();
                let (stack1, stack2) = if request.target_is_second {
                    (&*existing_stack, &new_stack)
                } else {
                    (&new_stack, &*existing_stack)
                };
                let resized = stack1
                    .resize_all(first_member)
                    .and_then(|()| stack2.resize_all(second_member));
                if let Err(err) = resized {
                    // The tree was taken out of self.pane above; put it back
                    // unsplit before failing, or the next take().unwrap()
                    // on this tab panics (in the mux server, that takes down
                    // every session).
                    self.pane.replace(cursor.tree());
                    return Err(err);
                }
            }

            let result = if request.target_is_second {
                cursor.split_leaf_and_insert_right(new_stack)
            } else {
                cursor.split_leaf_and_insert_left(new_stack)
            };
            match result {
                Ok(c) => cursor = c,
                Err(c) => {
                    self.pane.replace(c.tree());
                    anyhow::bail!("invalid pane_index {}; cannot split!", pane_index);
                }
            };

            // cursor now points to the newly created split node;
            // we need to populate its split information
            match cursor.assign_node(Some(split_info)) {
                Err(c) | Ok(c) => self.pane.replace(c.tree()),
            };

            if request.target_is_second {
                self.active = pane_index + 1;
                self.recency.tag(pane_index + 1);
            }
        }

        log::debug!("split info after split: {:#?}", self.iter_splits());
        log::debug!("pane info after split: {:#?}", self.iter_panes());

        Ok(if request.target_is_second {
            pane_index + 1
        } else {
            pane_index
        })
    }

    fn get_zoomed_pane(&self) -> Option<Arc<dyn Pane>> {
        self.zoomed.clone()
    }
}

/// This type is used directly by the codec, take care to bump
/// the codec version if you change this
#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub enum PaneNode {
    Empty,
    Split {
        left: Box<PaneNode>,
        right: Box<PaneNode>,
        node: SplitDirectionAndSize,
    },
    Leaf(PaneStackEntry),
}

/// One leaf of the wire layout: every member of a stack, in stack order.
///
/// This type is used directly by the codec, take care to bump
/// the codec version if you change this
#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
pub struct PaneStackEntry {
    pub panes: Vec<PaneEntry>,
    /// Index into `panes`; untrusted, so readers must clamp it
    pub visible: usize,
    pub header_rows: usize,
}

impl PaneNode {
    pub fn into_tree(self) -> bintree::Tree<PaneStackEntry, SplitDirectionAndSize> {
        match self {
            PaneNode::Empty => bintree::Tree::Empty,
            PaneNode::Split { left, right, node } => bintree::Tree::Node {
                left: Box::new((*left).into_tree()),
                right: Box::new((*right).into_tree()),
                data: Some(node),
            },
            PaneNode::Leaf(e) => bintree::Tree::Leaf(e),
        }
    }

    /// All entries in preorder leaf order, then stack order. For callers
    /// that only look panes up by id and don't care about the layout.
    pub fn into_pane_entries(self) -> Vec<PaneEntry> {
        fn walk(node: PaneNode, out: &mut Vec<PaneEntry>) {
            match node {
                PaneNode::Empty => {}
                PaneNode::Split { left, right, .. } => {
                    walk(*left, out);
                    walk(*right, out);
                }
                PaneNode::Leaf(stack) => out.extend(stack.panes),
            }
        }
        let mut out = vec![];
        walk(self, &mut out);
        out
    }

    pub fn root_size(&self) -> Option<TerminalSize> {
        match self {
            PaneNode::Empty => None,
            PaneNode::Split { node, .. } => Some(node.size()),
            PaneNode::Leaf(stack) => {
                // A bad `visible` falls back to the first member, as
                // `PaneStack::from_parts` does when the tree is built.
                let entry = stack.panes.get(stack.visible).or(stack.panes.first())?;
                // Clamped so that wire data cannot overflow the arithmetic.
                Some(slot_size(
                    &entry.size,
                    stack.header_rows.min(MAX_HEADER_ROWS),
                ))
            }
        }
    }

    pub fn window_and_tab_ids(&self) -> Option<(WindowId, TabId)> {
        match self {
            PaneNode::Empty => None,
            PaneNode::Split { left, right, .. } => match left.window_and_tab_ids() {
                Some(res) => Some(res),
                None => right.window_and_tab_ids(),
            },
            PaneNode::Leaf(stack) => stack
                .panes
                .first()
                .map(|entry| (entry.window_id, entry.tab_id)),
        }
    }

    /// The `header_rows` of the first leaf; all leaves of a tab agree.
    pub fn header_rows(&self) -> Option<usize> {
        match self {
            PaneNode::Empty => None,
            PaneNode::Split { left, right, .. } => left.header_rows().or(right.header_rows()),
            PaneNode::Leaf(stack) => Some(stack.header_rows),
        }
    }
}

/// This type is used directly by the codec, take care to bump
/// the codec version if you change this
#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
pub struct PaneEntry {
    pub window_id: WindowId,
    pub tab_id: TabId,
    pub pane_id: PaneId,
    pub title: String,
    pub size: TerminalSize,
    pub working_dir: Option<SerdeUrl>,
    pub is_active_pane: bool,
    pub is_zoomed_pane: bool,
    pub workspace: String,
    pub cursor_pos: StableCursorPosition,
    pub physical_top: StableRowIndex,
    pub top_row: usize,
    pub left_col: usize,
    pub tty_name: Option<String>,
}

#[derive(Deserialize, Clone, Serialize, PartialEq, Debug)]
#[serde(try_from = "String", into = "String")]
pub struct SerdeUrl {
    pub url: Url,
}

impl std::convert::TryFrom<String> for SerdeUrl {
    type Error = url::ParseError;
    fn try_from(s: String) -> Result<SerdeUrl, url::ParseError> {
        let url = Url::parse(&s)?;
        Ok(SerdeUrl { url })
    }
}

impl From<Url> for SerdeUrl {
    fn from(url: Url) -> SerdeUrl {
        SerdeUrl { url }
    }
}

impl Into<Url> for SerdeUrl {
    fn into(self) -> Url {
        self.url
    }
}

impl Into<String> for SerdeUrl {
    fn into(self) -> String {
        self.url.as_str().into()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_pane::*;
    #[test]
    fn tab_splitting() {
        let size = TerminalSize {
            rows: 24,
            cols: 80,
            pixel_width: 800,
            pixel_height: 600,
            dpi: 96,
        };

        let tab = Tab::new(&size);
        tab.assign_pane(&FakePane::new(1, size));

        let panes = tab.iter_panes();
        assert_eq!(1, panes.len());
        assert_eq!(0, panes[0].index);
        assert_eq!(true, panes[0].is_active);
        assert_eq!(0, panes[0].left);
        assert_eq!(0, panes[0].top);
        assert_eq!(80, panes[0].width);
        assert_eq!(24, panes[0].height);

        assert!(tab
            .compute_split_size(
                1,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                }
            )
            .is_none());

        let horz_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            horz_size,
            SplitDirectionAndSize {
                direction: SplitDirection::Horizontal,
                second: TerminalSize {
                    rows: 24,
                    cols: 40,
                    pixel_width: 400,
                    pixel_height: 600,
                    dpi: 96,
                },
                first: TerminalSize {
                    rows: 24,
                    cols: 39,
                    pixel_width: 390,
                    pixel_height: 600,
                    dpi: 96,
                },
            }
        );

        let vert_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Vertical,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            vert_size,
            SplitDirectionAndSize {
                direction: SplitDirection::Vertical,
                second: TerminalSize {
                    rows: 12,
                    cols: 80,
                    pixel_width: 800,
                    pixel_height: 300,
                    dpi: 96,
                },
                first: TerminalSize {
                    rows: 11,
                    cols: 80,
                    pixel_width: 800,
                    pixel_height: 275,
                    dpi: 96,
                }
            }
        );

        let new_index = tab
            .split_and_insert(
                0,
                SplitRequest {
                    direction: SplitDirection::Horizontal,
                    ..Default::default()
                },
                FakePane::new(2, horz_size.second),
            )
            .unwrap();
        assert_eq!(new_index, 1);

        let panes = tab.iter_panes();
        assert_eq!(2, panes.len());

        assert_eq!(0, panes[0].index);
        assert_eq!(false, panes[0].is_active);
        assert_eq!(0, panes[0].left);
        assert_eq!(0, panes[0].top);
        assert_eq!(39, panes[0].width);
        assert_eq!(24, panes[0].height);
        assert_eq!(390, panes[0].pixel_width);
        assert_eq!(600, panes[0].pixel_height);
        assert_eq!(1, panes[0].pane.pane_id());

        assert_eq!(1, panes[1].index);
        assert_eq!(true, panes[1].is_active);
        assert_eq!(40, panes[1].left);
        assert_eq!(0, panes[1].top);
        assert_eq!(40, panes[1].width);
        assert_eq!(24, panes[1].height);
        assert_eq!(400, panes[1].pixel_width);
        assert_eq!(600, panes[1].pixel_height);
        assert_eq!(2, panes[1].pane.pane_id());

        let vert_size = tab
            .compute_split_size(
                0,
                SplitRequest {
                    direction: SplitDirection::Vertical,
                    ..Default::default()
                },
            )
            .unwrap();
        let new_index = tab
            .split_and_insert(
                0,
                SplitRequest {
                    direction: SplitDirection::Vertical,
                    top_level: false,
                    target_is_second: true,
                    size: Default::default(),
                },
                FakePane::new(3, vert_size.second),
            )
            .unwrap();
        assert_eq!(new_index, 1);

        let panes = tab.iter_panes();
        assert_eq!(3, panes.len());

        assert_eq!(0, panes[0].index);
        assert_eq!(false, panes[0].is_active);
        assert_eq!(0, panes[0].left);
        assert_eq!(0, panes[0].top);
        assert_eq!(39, panes[0].width);
        assert_eq!(11, panes[0].height);
        assert_eq!(390, panes[0].pixel_width);
        assert_eq!(275, panes[0].pixel_height);
        assert_eq!(1, panes[0].pane.pane_id());

        assert_eq!(1, panes[1].index);
        assert_eq!(true, panes[1].is_active);
        assert_eq!(0, panes[1].left);
        assert_eq!(12, panes[1].top);
        assert_eq!(39, panes[1].width);
        assert_eq!(12, panes[1].height);
        assert_eq!(390, panes[1].pixel_width);
        assert_eq!(300, panes[1].pixel_height);
        assert_eq!(3, panes[1].pane.pane_id());

        assert_eq!(2, panes[2].index);
        assert_eq!(false, panes[2].is_active);
        assert_eq!(40, panes[2].left);
        assert_eq!(0, panes[2].top);
        assert_eq!(40, panes[2].width);
        assert_eq!(24, panes[2].height);
        assert_eq!(400, panes[2].pixel_width);
        assert_eq!(600, panes[2].pixel_height);
        assert_eq!(2, panes[2].pane.pane_id());

        tab.resize_split_by(1, 1);
        let panes = tab.iter_panes();
        assert_eq!(39, panes[0].width);
        assert_eq!(12, panes[0].height);
        assert_eq!(390, panes[0].pixel_width);
        assert_eq!(300, panes[0].pixel_height);

        assert_eq!(39, panes[1].width);
        assert_eq!(11, panes[1].height);
        assert_eq!(390, panes[1].pixel_width);
        assert_eq!(275, panes[1].pixel_height);

        assert_eq!(40, panes[2].width);
        assert_eq!(24, panes[2].height);
        assert_eq!(400, panes[2].pixel_width);
        assert_eq!(600, panes[2].pixel_height);
    }

    /// An 80x24 tab with `h` header rows, split once vertically at the
    /// default 50%: pane 1 on top, pane 2 (active) below. With `h = 1` the
    /// slots are 11 and 12 rows.
    fn vertical_pair(h: usize) -> Tab {
        let tab = Tab::with_header_rows(&size(24, 80), h);
        tab.assign_pane(&FakePane::new(1, size(24, 80)));
        let vert = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, vert).unwrap();
        assert_eq!(
            tab.split_and_insert(0, vert, FakePane::new(2, split.second))
                .unwrap(),
            1
        );
        tab
    }

    #[test]
    fn header_row_geometry() {
        let tab = Tab::with_header_rows(&size(24, 80), 1);
        let p1 = FakePane::new(1, size(24, 80));
        tab.assign_pane(&p1);
        assert_eq!(current_size(&p1), size(23, 80));
        let panes = tab.iter_panes();
        assert_eq!(
            (panes[0].top, panes[0].height, panes[0].pixel_height),
            (1, 23, 575)
        );

        let vert = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, vert).unwrap();
        assert_eq!((split.first.rows, split.second.rows), (11, 12)); // slots
        assert_eq!(
            tab.split_and_insert(0, vert, FakePane::new(2, split.second))
                .unwrap(),
            1
        );

        let panes = tab.iter_panes();
        assert_eq!(
            (panes[0].top, panes[0].height, panes[0].pixel_height),
            (1, 10, 250)
        );
        assert_eq!(
            (panes[1].top, panes[1].height, panes[1].pixel_height),
            (13, 11, 275)
        );
        assert_eq!(tab.get_pane_direction(PaneDirection::Up, false), Some(0));
        tab.set_active_idx(0);
        assert_eq!(tab.get_pane_direction(PaneDirection::Down, false), Some(1));
    }

    #[test]
    fn header_row_min_size() {
        let tab = vertical_pair(1); // helper: h=1, 80x24, one vertical split (slots 11/12)
        tab.resize(size(3, 80));
        assert_eq!(tab.get_size().rows, 5); // 2 + 1 + 2
        assert!(tab.iter_panes().iter().all(|p| p.height >= 1));
    }

    #[test]
    fn header_row_min_size_nested() {
        // slots 11 / (5 / 6): the lower subtree reaches its minimum first.
        let tab = vertical_pair(1);
        let vert = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        let split = tab.compute_split_size(1, vert).unwrap();
        tab.split_and_insert(1, vert, FakePane::new(3, split.second))
            .unwrap();
        tab.resize(size(3, 80));
        assert_eq!(tab.get_size().rows, 8); // 2 + 1 + 2 + 1 + 2
        assert!(tab.iter_panes().iter().all(|p| p.height == 1));
    }

    #[test]
    fn header_row_split_needs_two_rows() {
        let vert = SplitRequest {
            direction: SplitDirection::Vertical,
            ..Default::default()
        };
        for (h, ok) in [(0, true), (1, false)] {
            let tab = Tab::with_header_rows(&size(4, 80), h);
            tab.assign_pane(&FakePane::new(1, size(4, 80)));
            let split = tab.compute_split_size(0, vert).unwrap();
            assert_eq!(
                tab.split_and_insert(0, vert, FakePane::new(2, split.second))
                    .is_ok(),
                ok
            );
        }
    }

    #[test]
    fn header_row_split_drag_keeps_two_rows() {
        let tab = vertical_pair(1);
        tab.resize_split_by(0, -100);
        let p = tab.iter_panes();
        assert_eq!((p[0].height, p[1].height), (1, 20));
        tab.resize_split_by(0, 100);
        let p = tab.iter_panes();
        assert_eq!((p[0].height, p[1].height), (20, 1));
    }

    #[test]
    fn header_row_rebuild_is_fixed_point() {
        let tab = vertical_pair(1);
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        tab.split_and_insert(0, horz, FakePane::new(3, split.second))
            .unwrap();
        let geometry = |t: &Tab| -> Vec<_> {
            t.iter_panes()
                .iter()
                .map(|p| (p.index, p.left, p.top, p.width, p.height))
                .collect()
        };
        let (before, splits, sz) = (geometry(&tab), tab.iter_splits(), tab.get_size());
        tab.rebuild_splits_sizes_from_contained_panes();
        assert_eq!(
            (geometry(&tab), tab.iter_splits(), tab.get_size()),
            (before, splits, sz)
        );
    }

    #[test]
    fn failed_split_resize_keeps_the_tab_usable() {
        // The refusing new pane is resized second when it goes right and
        // first when it goes left, so both early returns are covered.
        for target_is_second in [true, false] {
            let tab = Tab::new(&size(24, 80));
            tab.assign_pane(&FakePane::new(1, size(24, 80)));
            let vert = SplitRequest {
                direction: SplitDirection::Vertical,
                target_is_second,
                ..Default::default()
            };
            let split = tab.compute_split_size(0, vert).unwrap();
            let refusing = FakePane::with_failing_resize(2, split.second);
            assert!(tab.split_and_insert(0, vert, refusing).is_err());

            let panes = tab.iter_panes();
            assert_eq!(panes.len(), 1);
            assert_eq!(panes[0].pane.pane_id(), 1);
        }
    }

    /// An 80x24 tab with one header row whose single leaf stacks panes
    /// 1, 2 and 3, with pane 3 visible.
    fn stacked_tab() -> (Tab, [Arc<dyn Pane>; 3]) {
        let tab = Tab::with_header_rows(&size(24, 80), 1);
        let p1 = FakePane::new(1, size(24, 80));
        let p2 = FakePane::new(2, size(24, 80));
        let p3 = FakePane::new(3, size(24, 80));
        tab.assign_pane(&p1);
        tab.add_pane_to_stack(1, None, Arc::clone(&p2)).unwrap();
        tab.add_pane_to_stack(2, None, Arc::clone(&p3)).unwrap();
        (tab, [p1, p2, p3])
    }

    fn entry(
        pane_id: PaneId,
        top_row: usize,
        left_col: usize,
        rows: usize,
        cols: usize,
    ) -> PaneEntry {
        PaneEntry {
            window_id: 0,
            tab_id: 0,
            pane_id,
            title: String::new(),
            size: size(rows, cols),
            working_dir: None,
            is_active_pane: false,
            is_zoomed_pane: false,
            workspace: "default".to_string(),
            cursor_pos: Default::default(),
            physical_top: 0,
            top_row,
            left_col,
            tty_name: None,
        }
    }

    #[test]
    fn pane_tree_round_trip_keeps_stacks() {
        let (tab, _) = stacked_tab(); // leaf 0: [1, 2, 3], pane 3 visible
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        tab.split_and_insert(0, horz, FakePane::new(4, split.second))
            .unwrap(); // leaf 1 active
        let node = {
            let mut inner = tab.inner.lock();
            let active = inner.get_active_pane();
            pane_tree(
                inner.pane.as_ref().unwrap(),
                tab.tab_id(),
                7,
                active.as_ref(),
                None,
                "default",
                0,
                0,
                1,
            )
        };
        assert_eq!(node.header_rows(), Some(1));
        assert_eq!(node.root_size(), Some(size(24, 80)));

        let mirror = Tab::new(&size(24, 80));
        mirror.sync_with_pane_tree(size(24, 80), node, |e| {
            FakePane::new(e.pane_id + 100, e.size)
        });
        assert_eq!(mirror.header_rows(), 1);
        let s = mirror.iter_stacks();
        assert_eq!(
            (ids(&s[0].panes), s[0].visible, ids(&s[1].panes)),
            (vec![101, 102, 103], 2, vec![104])
        );
        assert_eq!(mirror.get_active_pane().unwrap().pane_id(), 104);
        let geo = |t: &Tab| -> Vec<_> {
            t.iter_panes()
                .iter()
                .map(|p| (p.left, p.top, p.width, p.height))
                .collect()
        };
        assert_eq!(geo(&mirror), geo(&tab));
    }

    #[test]
    fn pane_node_entries_and_sizes() {
        let node = PaneNode::Leaf(PaneStackEntry {
            panes: vec![entry(1, 1, 0, 23, 80), entry(2, 1, 0, 23, 80)],
            visible: 1,
            header_rows: 1,
        });
        assert_eq!(
            node.root_size().map(|s| (s.rows, s.pixel_height)),
            Some((24, 600))
        );
        assert_eq!(
            node.into_pane_entries()
                .iter()
                .map(|e| e.pane_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn pane_node_tolerates_bad_stack_entry() {
        let bad = PaneNode::Leaf(PaneStackEntry {
            panes: vec![entry(1, 1, 0, 23, 80), entry(2, 1, 0, 23, 80)],
            visible: 5,
            header_rows: 7,
        });
        let tab = Tab::new(&size(24, 80));
        tab.sync_with_pane_tree(size(24, 80), bad, |e| FakePane::new(e.pane_id, e.size));
        assert_eq!((tab.header_rows(), tab.iter_stacks()[0].visible), (1, 0));

        let empty = PaneNode::Leaf(PaneStackEntry {
            panes: vec![],
            visible: 0,
            header_rows: 1,
        });
        let tab = Tab::new(&size(24, 80));
        tab.sync_with_pane_tree(size(24, 80), empty, |e| FakePane::new(e.pane_id, e.size));
        assert!(tab.iter_all_panes().is_empty());
    }

    #[test]
    fn empty_stack_under_split_collapses_the_split() {
        // Malformed wire data: an empty stack on one side of a split must not
        // leave an Empty under a Node, which would hide the siblings to its
        // right from the preorder walk behind `iter_panes`.
        let node = PaneNode::Split {
            left: Box::new(PaneNode::Leaf(PaneStackEntry {
                panes: vec![],
                visible: 0,
                header_rows: 1,
            })),
            right: Box::new(PaneNode::Leaf(PaneStackEntry {
                panes: vec![entry(2, 1, 40, 23, 40)],
                visible: 0,
                header_rows: 1,
            })),
            node: SplitDirectionAndSize {
                direction: SplitDirection::Horizontal,
                first: size(24, 39),
                second: size(24, 40),
            },
        };
        let tab = Tab::new(&size(24, 80));
        tab.sync_with_pane_tree(size(24, 80), node, |e| FakePane::new(e.pane_id, e.size));
        assert_eq!(
            tab.iter_panes()
                .iter()
                .map(|p| p.pane.pane_id())
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(ids(&tab.iter_all_panes()), vec![2]);
    }

    #[test]
    fn stack_add_shows_new_pane() {
        let (tab, [p1, p2, p3]) = stacked_tab();
        let panes = tab.iter_panes();
        assert_eq!((panes.len(), panes[0].pane.pane_id()), (1, 3));
        assert_eq!(ids(&tab.iter_all_panes()), vec![1, 2, 3]);
        assert_eq!(
            (tab.leaf_index_of(1), tab.count_panes()),
            (Some(0), Some(3))
        );
        assert!(
            tab.contains_pane(1) && !tab.contains_visible_pane(1) && tab.contains_visible_pane(3)
        );
        assert_eq!(current_size(&p2), size(23, 80));
        // Newly hidden members are told directly; the new visible one gets focus
        // from the GUI's per-frame focus recording, not from the mux.
        assert_eq!(
            (last_focus(&p1), last_focus(&p2), last_focus(&p3)),
            (Some(false), Some(false), None)
        );
        assert!(tab.add_pane_to_stack(1, None, Arc::clone(&p2)).is_err()); // already in this tab
        assert_eq!(tab.iter_all_panes().len(), 3);
    }

    #[test]
    fn stack_add_requires_header_row() {
        let tab = Tab::new(&size(24, 80));
        tab.assign_pane(&FakePane::new(1, size(24, 80)));
        assert!(tab
            .add_pane_to_stack(1, None, FakePane::new(2, size(24, 80)))
            .is_err());
        assert_eq!(tab.iter_all_panes().len(), 1);
    }

    #[test]
    fn stack_activate_hidden_pane() {
        let (tab, [p1, _p2, p3]) = stacked_tab();
        tab.set_active_pane(&p1);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 1);
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 1);
        assert_eq!(
            (last_focus(&p3), last_focus(&p1)),
            (Some(false), Some(true))
        );
        assert_eq!(current_size(&p1), size(23, 80)); // switching does not resize
    }

    #[test]
    fn stack_close_visible_takes_right_neighbor() {
        let (tab, [_p1, p2, p3]) = stacked_tab();
        tab.set_active_pane(&p2);
        assert_eq!(tab.remove_pane(2).unwrap().pane_id(), 2);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 3);
        // Removal never hands out focus: a background tab must not steal the window.
        assert_eq!(last_focus(&p3), Some(false));
        tab.remove_pane(3);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 1);
        assert_eq!(ids(&tab.iter_all_panes()), vec![1]);
    }

    #[test]
    fn pane_added_to_inactive_leaf_is_unfocused() {
        let (tab, _) = stacked_tab();
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        tab.split_and_insert(0, horz, FakePane::new(4, split.second))
            .unwrap(); // leaf 1 active
        let p5 = FakePane::new(5, size(23, 39));
        tab.add_pane_to_stack(1, None, Arc::clone(&p5)).unwrap(); // into inactive leaf 0
        assert_eq!(last_focus(&p5), Some(false));
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 4);
    }

    #[test]
    fn stack_split_targets_hidden_member() {
        let (tab, _) = stacked_tab();
        let leaf = tab.leaf_index_of(1).unwrap(); // pane 1 is hidden
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(leaf, horz).unwrap();
        tab.split_and_insert(leaf, horz, FakePane::new(4, split.second))
            .unwrap();
        let stacks = tab.iter_stacks();
        assert_eq!(
            (ids(&stacks[0].panes), ids(&stacks[1].panes)),
            (vec![1, 2, 3], vec![4])
        );
    }

    #[test]
    fn stack_move_into_neighbor_collapses_leaf() {
        let tab = Tab::with_header_rows(&size(24, 80), 1);
        tab.assign_pane(&FakePane::new(1, size(24, 80)));
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        let p2 = FakePane::new(2, split.second);
        tab.split_and_insert(0, horz, Arc::clone(&p2)).unwrap();
        tab.move_pane_to_stack(2, 1, 1).unwrap();
        let stacks = tab.iter_stacks();
        assert_eq!(
            (stacks.len(), ids(&stacks[0].panes), stacks[0].visible),
            (1, vec![1, 2], 1)
        );
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 2);
        assert_eq!(current_size(&p2), size(23, 80));
    }

    #[test]
    fn moving_the_focused_pane_keeps_its_focus() {
        let tab = Tab::with_header_rows(&size(24, 80), 1);
        let p1 = FakePane::new(1, size(24, 80));
        tab.assign_pane(&p1);
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        let p2 = FakePane::new(2, split.second);
        tab.split_and_insert(0, horz, Arc::clone(&p2)).unwrap();
        let p3 = FakePane::new(3, size(24, 80));
        tab.add_pane_to_stack(2, None, Arc::clone(&p3)).unwrap();
        tab.set_active_pane(&p1);
        tab.set_active_pane(&p3); // p3 focused, in leaf 1 with hidden p2
        tab.move_pane_to_stack(3, 1, 0).unwrap();
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 3);
        // The focused pane changed leaves, not focus: nobody may tell it
        // otherwise, as advise_focus_change sees nothing to correct.
        assert_eq!(
            (last_focus(&p3), last_focus(&p1)),
            (Some(true), Some(false))
        );
    }

    #[test]
    fn stack_move_from_earlier_leaf_into_later_leaf() {
        // Source leaf before the target leaf: removing the pane collapses
        // leaf 0, so the target moves from leaf index 1 to 0 and has to be
        // looked up again.
        let tab = Tab::with_header_rows(&size(24, 80), 1);
        let p1 = FakePane::new(1, size(24, 80));
        tab.assign_pane(&p1);
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        tab.split_and_insert(0, horz, FakePane::new(2, split.second))
            .unwrap();
        assert_eq!(tab.iter_stacks().len(), 2);

        tab.move_pane_to_stack(1, 2, 1).unwrap();

        let stacks = tab.iter_stacks();
        assert_eq!(
            (stacks.len(), ids(&stacks[0].panes), stacks[0].visible),
            (1, vec![2, 1], 1)
        );
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 1);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 1);
        assert_eq!(
            current_size(&p1),
            member_size(&size(24, 80), 1),
            "moved pane is sized for the whole tab"
        );
    }

    #[test]
    fn stack_move_reorders_within_stack() {
        let (tab, _) = stacked_tab();
        tab.move_pane_to_stack(1, 3, 2).unwrap();
        assert_eq!(ids(&tab.iter_stacks()[0].panes), vec![2, 3, 1]);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 1);
        tab.move_pane_to_stack(1, 1, 99).unwrap(); // clamps to the end
        assert_eq!(ids(&tab.iter_stacks()[0].panes), vec![2, 3, 1]);
        tab.move_pane_to_stack(1, 1, 0).unwrap();
        assert_eq!(ids(&tab.iter_stacks()[0].panes), vec![1, 2, 3]);
    }

    #[test]
    fn stack_move_rejects_bad_ids() {
        let (tab, _) = stacked_tab();
        assert!(tab.move_pane_to_stack(99, 1, 0).is_err());
        assert!(tab.move_pane_to_stack(1, 99, 0).is_err());
        let flat = Tab::new(&size(24, 80));
        flat.assign_pane(&FakePane::new(7, size(24, 80)));
        assert!(flat.move_pane_to_stack(7, 7, 0).is_err());
    }

    #[test]
    fn stack_activate_relative_wraps() {
        let (tab, _) = stacked_tab(); // visible: 3
        tab.activate_stack_relative(1);
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 1);
        tab.activate_stack_relative(-1);
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 3);
    }

    #[test]
    fn stack_positions_are_slots() {
        let (tab, _) = stacked_tab();
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        tab.split_and_insert(0, horz, FakePane::new(4, split.second))
            .unwrap();
        let s = tab.iter_stacks();
        assert_eq!(
            (s[0].left, s[0].top, s[0].width, s[0].height, s[0].is_active),
            (0, 0, 39, 24, false)
        );
        assert_eq!(
            (s[1].left, s[1].top, s[1].width, s[1].height, s[1].is_active),
            (40, 0, 40, 24, true)
        );
    }

    #[test]
    fn swap_moves_whole_stack() {
        let (tab, _) = stacked_tab();
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        tab.split_and_insert(0, horz, FakePane::new(4, split.second))
            .unwrap();
        tab.swap_active_with_index(0, true);
        let s = tab.iter_stacks();
        assert_eq!(
            (ids(&s[0].panes), ids(&s[1].panes)),
            (vec![4], vec![1, 2, 3])
        );
    }

    /// `stacked_tab` split horizontally, so that leaf 1 holds pane 4; then
    /// leaf 0 (panes 1, 2 and 3, with pane 3 visible) is activated and
    /// zoomed.
    fn zoomable_tab() -> (Tab, [Arc<dyn Pane>; 4]) {
        let (tab, [p1, p2, p3]) = stacked_tab();
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        let p4 = FakePane::new(4, split.second);
        tab.split_and_insert(0, horz, Arc::clone(&p4)).unwrap();
        tab.set_active_pane(&p3);
        tab.set_zoomed(true);
        (tab, [p1, p2, p3, p4])
    }

    #[test]
    fn zoom_covers_leaf_and_keeps_header() {
        let (tab, [p1, p2, p3, p4]) = zoomable_tab();
        let panes = tab.iter_panes();
        assert_eq!(
            (
                panes.len(),
                panes[0].pane.pane_id(),
                panes[0].top,
                panes[0].height,
                panes[0].width
            ),
            (1, 3, 1, 23, 80)
        );
        for p in [&p1, &p2, &p3] {
            assert_eq!(current_size(p), size(23, 80));
        }
        assert_eq!(current_size(&p4).cols, 40);
        assert_eq!(tab.iter_stacks().len(), 1);
    }

    #[test]
    fn zoom_survives_switch_inside_leaf() {
        let (tab, [p1, ..]) = zoomable_tab();
        tab.set_active_pane(&p1);
        assert_eq!(tab.get_zoomed_pane().unwrap().pane_id(), 1);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 1);
    }

    #[test]
    fn zoom_survives_closing_visible() {
        let (tab, [p1, ..]) = zoomable_tab();
        tab.set_active_pane(&p1);
        tab.remove_pane(1);
        assert_eq!(tab.get_zoomed_pane().unwrap().pane_id(), 2);
    }

    #[test]
    fn zoom_ends_when_leaf_goes() {
        let (tab, _) = zoomable_tab();
        for id in [1, 2, 3] {
            tab.remove_pane(id);
        }
        assert!(tab.get_zoomed_pane().is_none());
        let panes = tab.iter_panes();
        assert_eq!(
            (
                panes.len(),
                panes[0].pane.pane_id(),
                panes[0].width,
                panes[0].height
            ),
            (1, 4, 80, 23)
        );
    }

    #[test]
    fn set_pane_zoomed_semantics() {
        let (tab, [p1, p2, _p3, p4]) = zoomable_tab();
        assert!(tab.zoomed_leaf_contains(1) && !tab.zoomed_leaf_contains(4));
        tab.set_pane_zoomed(&p1, true); // hidden member: brought to front, zoom kept
        assert_eq!(tab.get_zoomed_pane().unwrap().pane_id(), 1);
        tab.set_pane_zoomed(&p4, false); // other leaf: nothing
        assert!(tab.get_zoomed_pane().is_some());
        tab.set_pane_zoomed(&p2, false); // hidden member of the zoomed leaf: unzoom
        assert!(tab.get_zoomed_pane().is_none());
        tab.set_pane_zoomed(&p4, true);
        assert_eq!(tab.get_zoomed_pane().unwrap().pane_id(), 4);
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 4);
    }

    #[test]
    fn switch_to_other_leaf_unzooms() {
        let (tab, [.., p4]) = zoomable_tab();
        tab.set_active_pane(&p4); // unzoom_on_switch_pane defaults to true
        assert!(tab.get_zoomed_pane().is_none());
        assert_eq!(tab.get_active_pane().unwrap().pane_id(), 4);
    }

    #[test]
    fn zoom_follows_pane_added_to_zoomed_leaf() {
        let (tab, _) = zoomable_tab();
        let p5 = FakePane::new(5, size(23, 39));
        tab.add_pane_to_stack(1, None, Arc::clone(&p5)).unwrap();
        assert_eq!(tab.get_zoomed_pane().unwrap().pane_id(), 5);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 5);
        assert_eq!(current_size(&p5), size(23, 80)); // zoom size, not the leaf's slot
    }

    #[test]
    fn zoom_follows_reorder_inside_zoomed_leaf() {
        let (tab, _) = zoomable_tab(); // zoomed leaf [1, 2, 3], pane 3 visible
        tab.move_pane_to_stack(1, 3, 0).unwrap(); // pane 1 becomes visible
        assert_eq!(tab.get_zoomed_pane().unwrap().pane_id(), 1);
        assert_eq!(tab.iter_panes()[0].pane.pane_id(), 1);
    }

    #[test]
    fn h0_adds_no_resize_calls() {
        // With header_rows = 0 every pane must see exactly the resize calls
        // the upstream algorithm makes (tmux panes turn each one into a
        // command). Expected counts, derived by reading upstream tab.rs at
        // 80de2bac5 for panes p1 (left) and p2 (right, active):
        //  - assign_pane: upstream assigns the root without resizing: p1 0.
        //  - split_and_insert: pane1.resize(first), pane2.resize(second):
        //    p1 1, p2 1.
        //  - resize_split_by: cascade_size_from_cursor resizes each leaf
        //    below the split once: p1 2, p2 2.
        //  - set_zoomed(true): toggle_zoom resizes only the active pane
        //    (p2) to the tab size: p2 3.
        //  - set_zoomed(false): Tab::resize -> apply_sizes_from_splits
        //    resizes every leaf once: p1 3, p2 4.
        //  - remove_pane(2): the pane removed is not resized; the leaf that
        //    takes over the parent's slot is resized once (`unsplit.resize`):
        //    p1 4, p2 4.
        let tab = Tab::new(&size(24, 80));
        let p1 = FakePane::new(1, size(24, 80));
        tab.assign_pane(&p1);
        assert_eq!(resize_count(&p1), 0);

        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        let split = tab.compute_split_size(0, horz).unwrap();
        let p2 = FakePane::new(2, split.second);
        tab.split_and_insert(0, horz, Arc::clone(&p2)).unwrap();
        assert_eq!((resize_count(&p1), resize_count(&p2)), (1, 1));

        tab.resize_split_by(0, 5);
        assert_eq!((resize_count(&p1), resize_count(&p2)), (2, 2));

        tab.set_zoomed(true);
        assert_eq!((resize_count(&p1), resize_count(&p2)), (2, 3));

        tab.set_zoomed(false);
        assert_eq!((resize_count(&p1), resize_count(&p2)), (3, 4));

        tab.remove_pane(2);
        assert_eq!((resize_count(&p1), resize_count(&p2)), (4, 4));
    }

    /// An 80x24 tab with one header row and leaves [1, 2] (pane 2 visible),
    /// [3] and [4], split side by side.
    fn three_leaf_tab() -> Tab {
        let tab = Tab::with_header_rows(&size(24, 80), 1);
        tab.assign_pane(&FakePane::new(1, size(24, 80)));
        tab.add_pane_to_stack(1, None, FakePane::new(2, size(24, 80)))
            .unwrap();
        let horz = SplitRequest {
            direction: SplitDirection::Horizontal,
            ..Default::default()
        };
        for (leaf, id) in [(0, 3), (1, 4)] {
            let split = tab.compute_split_size(leaf, horz).unwrap();
            tab.split_and_insert(leaf, horz, FakePane::new(id, split.second))
                .unwrap();
        }
        tab
    }

    #[test]
    fn rotate_moves_whole_stacks() {
        let layout = |tab: &Tab| -> Vec<(Vec<PaneId>, usize)> {
            tab.iter_stacks()
                .iter()
                .map(|s| (ids(&s.panes), s.visible))
                .collect()
        };
        let (s0, s1, s2) = ((vec![1, 2], 1), (vec![3], 0), (vec![4], 0));
        let tab = three_leaf_tab();
        assert_eq!(layout(&tab), vec![s0.clone(), s1.clone(), s2.clone()]);

        // Same movement as upstream applies to single panes.
        tab.rotate_counter_clockwise();
        assert_eq!(layout(&tab), vec![s1.clone(), s2.clone(), s0.clone()]);

        let tab = three_leaf_tab();
        tab.rotate_clockwise();
        assert_eq!(layout(&tab), vec![s2, s0, s1]);
    }

    fn is_send_and_sync<T: Send + Sync>() -> bool {
        true
    }

    #[test]
    fn tab_is_send_and_sync() {
        assert!(is_send_and_sync::<Tab>());
    }
}
