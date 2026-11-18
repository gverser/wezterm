//! The session tree data model.
//!
//! This module is deliberately free of any rendering or windowing concerns so
//! that it can be unit tested; see the tests at the bottom of the file.

use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Identifies a node within a [`SessionTree`].
///
/// Ids are stable across renames and are what we derive the mux workspace name
/// from; using the user-visible name would mean that renaming a node orphaned
/// any terminals already running under it, and that two nodes that happened to
/// share a name would fight over the same workspace.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct NodeId(pub u64);

impl NodeId {
    /// The mux workspace that terminals for this node live in.
    pub fn workspace_name(&self) -> String {
        format!("session:{}", self.0)
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "{}", self.0)
    }
}

/// How to launch the terminal for a session node.
///
/// This mirrors the subset of [`SpawnCommand`] that makes sense for a saved
/// session. We don't reuse `SpawnCommand` directly because it only derives
/// `FromDynamic`/`ToDynamic`; teaching it serde would cascade through
/// `GuiPosition` -> `Dimension` -> `GeometryOrigin`, none of which have serde
/// impls, and its `label`/`position` fields are meaningless here anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSpawn {
    #[serde(default = "default_domain")]
    pub domain: SpawnTabDomain,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// `SpawnTabDomain::default()` is `CurrentPaneDomain`, which for a sidebar node
/// would resolve against whichever pane happened to be focused in the workspace
/// we are switching *away* from. `DefaultDomain` is the sane meaning of "open
/// this saved session".
fn default_domain() -> SpawnTabDomain {
    SpawnTabDomain::DefaultDomain
}

impl Default for SessionSpawn {
    fn default() -> Self {
        Self {
            domain: default_domain(),
            cwd: None,
            args: None,
            env: BTreeMap::new(),
        }
    }
}

impl From<&SessionSpawn> for SpawnCommand {
    fn from(spawn: &SessionSpawn) -> SpawnCommand {
        SpawnCommand {
            label: None,
            args: spawn.args.clone(),
            cwd: spawn.cwd.clone(),
            set_environment_variables: spawn
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            domain: spawn.domain.clone(),
            position: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Folder {
        #[serde(default)]
        children: Vec<SessionNode>,
        #[serde(default)]
        expanded: bool,
    },
    Session {
        #[serde(default)]
        spawn: SessionSpawn,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionNode {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
}

impl SessionNode {
    pub fn folder(id: NodeId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            kind: NodeKind::Folder {
                children: vec![],
                expanded: true,
            },
        }
    }

    pub fn session(id: NodeId, name: impl Into<String>, spawn: SessionSpawn) -> Self {
        Self {
            id,
            name: name.into(),
            kind: NodeKind::Session { spawn },
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, NodeKind::Folder { .. })
    }

    pub fn children(&self) -> &[SessionNode] {
        match &self.kind {
            NodeKind::Folder { children, .. } => children,
            NodeKind::Session { .. } => &[],
        }
    }

    /// This node if it is a session, and every session inside it if it is a
    /// folder: what goes when it is removed.
    pub fn session_ids(&self) -> Vec<NodeId> {
        fn walk(node: &SessionNode, out: &mut Vec<NodeId>) {
            if !node.is_folder() {
                out.push(node.id);
            }
            for child in node.children() {
                walk(child, out);
            }
        }
        let mut out = vec![];
        walk(self, &mut out);
        out
    }
}

/// One row of the flattened, currently-visible view of the tree.
///
/// Borrows from the tree: this is rebuilt on every paint and never stored.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleRow<'a> {
    pub id: NodeId,
    pub name: &'a str,
    pub depth: usize,
    pub kind: RowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Folder { expanded: bool },
    Session,
}

/// Where a node being moved is to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Just before this node, among its siblings.
    Before(NodeId),
    /// Just after this node, among its siblings.
    After(NodeId),
    /// The last child of this folder, or of the top level for None.
    LastIn(Option<NodeId>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionTree {
    #[serde(default)]
    pub roots: Vec<SessionNode>,

    /// Derived from the contents on load rather than persisted, so that a
    /// hand-edited `sessions.json` can't hand us an id that is already in use.
    #[serde(skip)]
    next_id: u64,
}

impl SessionTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute the id allocator. Call after deserializing.
    pub fn reseed_ids(&mut self) {
        fn max_id(nodes: &[SessionNode], acc: &mut u64) {
            for node in nodes {
                *acc = (*acc).max(node.id.0);
                max_id(node.children(), acc);
            }
        }
        let mut max = 0;
        max_id(&self.roots, &mut max);
        self.next_id = max + 1;
    }

    pub fn alloc_id(&mut self) -> NodeId {
        // Guard against `reseed_ids` never having been called on a tree that
        // was built up by hand.
        if self.next_id == 0 {
            self.reseed_ids();
        }
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// `alloc_id`, passing over ids that `taken` says are still in use outside
    /// the tree: a saved ssh host, or an open workspace, left behind by a node
    /// that was deleted. A new node with such an id would inherit them.
    pub fn alloc_id_skipping(&mut self, taken: impl Fn(NodeId) -> bool) -> NodeId {
        loop {
            let id = self.alloc_id();
            if !taken(id) {
                return id;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn find(&self, id: NodeId) -> Option<&SessionNode> {
        fn walk(nodes: &[SessionNode], id: NodeId) -> Option<&SessionNode> {
            for node in nodes {
                if node.id == id {
                    return Some(node);
                }
                if let Some(found) = walk(node.children(), id) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.roots, id)
    }

    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut SessionNode> {
        fn walk(nodes: &mut [SessionNode], id: NodeId) -> Option<&mut SessionNode> {
            for node in nodes {
                if node.id == id {
                    return Some(node);
                }
                if let NodeKind::Folder { children, .. } = &mut node.kind {
                    if let Some(found) = walk(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&mut self.roots, id)
    }

    /// Insert `node` as the last child of `parent`, or at the top level when
    /// `parent` is `None`. Returns false if `parent` doesn't exist or isn't a
    /// folder.
    pub fn insert(&mut self, parent: Option<NodeId>, node: SessionNode) -> bool {
        match parent {
            None => {
                self.roots.push(node);
                true
            }
            Some(parent) => match self.find_mut(parent) {
                Some(SessionNode {
                    kind: NodeKind::Folder { children, .. },
                    ..
                }) => {
                    children.push(node);
                    true
                }
                _ => false,
            },
        }
    }

    /// Detach `id` (and its subtree) from the tree.
    pub fn remove(&mut self, id: NodeId) -> Option<SessionNode> {
        fn walk(nodes: &mut Vec<SessionNode>, id: NodeId) -> Option<SessionNode> {
            if let Some(idx) = nodes.iter().position(|n| n.id == id) {
                return Some(nodes.remove(idx));
            }
            for node in nodes.iter_mut() {
                if let NodeKind::Folder { children, .. } = &mut node.kind {
                    if let Some(found) = walk(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&mut self.roots, id)
    }

    /// Whether `id` can be moved to `to`: both are in the tree, a LastIn names
    /// a folder, and the move would not put a node beside or inside itself --
    /// which for a folder means anywhere among its own contents.
    pub fn can_move(&self, id: NodeId, to: Placement) -> bool {
        let node = match self.find(id) {
            Some(node) => node,
            None => return false,
        };
        let target = match to {
            Placement::Before(target) | Placement::After(target) => target,
            Placement::LastIn(None) => return true,
            Placement::LastIn(Some(folder)) => match self.find(folder) {
                Some(found) if found.is_folder() => folder,
                _ => return false,
            },
        };
        self.find(target).is_some() && !contains(node, target)
    }

    /// Move `id`, along with everything beneath it, to `to`.
    ///
    /// Returns false and leaves the tree alone when [`Self::can_move`] says
    /// no. A folder that the node goes into is opened, so that it does not
    /// just vanish from view.
    pub fn move_node(&mut self, id: NodeId, to: Placement) -> bool {
        if !self.can_move(id, to) {
            return false;
        }
        let node = match self.remove(id) {
            Some(node) => node,
            None => return false,
        };
        // can_move has made sure the target is still there now that the node
        // is out of the tree. Should it not be after all, the node goes to the
        // top level rather than being lost from sessions.json.
        match to {
            Placement::LastIn(None) => self.roots.push(node),
            Placement::LastIn(Some(folder)) => {
                if let Some(SessionNode {
                    kind: NodeKind::Folder { children, expanded },
                    ..
                }) = self.find_mut(folder)
                {
                    children.push(node);
                    *expanded = true;
                } else {
                    self.roots.push(node);
                }
            }
            Placement::Before(target) | Placement::After(target) => {
                let after = matches!(to, Placement::After(_));
                match self.siblings_mut(target) {
                    Some((siblings, idx)) => {
                        siblings.insert(if after { idx + 1 } else { idx }, node)
                    }
                    None => self.roots.push(node),
                }
            }
        }
        true
    }

    /// The list that `id` sits in, and its index there.
    fn siblings_mut(&mut self, id: NodeId) -> Option<(&mut Vec<SessionNode>, usize)> {
        fn walk(
            nodes: &mut Vec<SessionNode>,
            id: NodeId,
        ) -> Option<(&mut Vec<SessionNode>, usize)> {
            if let Some(idx) = nodes.iter().position(|node| node.id == id) {
                return Some((nodes, idx));
            }
            for node in nodes.iter_mut() {
                if let NodeKind::Folder { children, .. } = &mut node.kind {
                    if let Some(found) = walk(children, id) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&mut self.roots, id)
    }

    pub fn rename(&mut self, id: NodeId, name: impl Into<String>) -> bool {
        match self.find_mut(id) {
            Some(node) => {
                node.name = name.into();
                true
            }
            None => false,
        }
    }

    /// Toggle a folder's expanded state. Returns the new state, or None when
    /// `id` is not a folder.
    pub fn toggle_expanded(&mut self, id: NodeId) -> Option<bool> {
        match self.find_mut(id) {
            Some(SessionNode {
                kind: NodeKind::Folder { expanded, .. },
                ..
            }) => {
                *expanded = !*expanded;
                Some(*expanded)
            }
            _ => None,
        }
    }

    /// Every session node paired with its name, in display order. Folders are
    /// skipped: only sessions own a workspace.
    pub fn sessions_with_names(&self) -> Vec<(NodeId, String)> {
        fn walk(nodes: &[SessionNode], out: &mut Vec<(NodeId, String)>) {
            for node in nodes {
                if !node.is_folder() {
                    out.push((node.id, node.name.clone()));
                }
                walk(node.children(), out);
            }
        }
        let mut out = vec![];
        walk(&self.roots, &mut out);
        out
    }

    /// The parent of `id`, or None when it is a root or absent.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        fn walk(nodes: &[SessionNode], id: NodeId, parent: Option<NodeId>) -> Option<NodeId> {
            for node in nodes {
                if node.id == id {
                    return parent;
                }
                if let Some(found) = walk(node.children(), id, Some(node.id)) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.roots, id, None)
    }

    /// The set of nodes to show for `query`: every node whose name matches,
    /// plus all of their ancestors so that the matches remain reachable.
    ///
    /// Returns None when the query is empty, meaning "no filtering".
    pub fn matching_ids(&self, query: &str) -> Option<HashSet<NodeId>> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }

        let mut keep = HashSet::new();

        fn walk(
            nodes: &[SessionNode],
            query: &str,
            ancestors: &mut Vec<NodeId>,
            keep: &mut HashSet<NodeId>,
        ) {
            for node in nodes {
                if subsequence_match(&node.name, query) {
                    keep.insert(node.id);
                    keep.extend(ancestors.iter().copied());
                }
                ancestors.push(node.id);
                walk(node.children(), query, ancestors, keep);
                ancestors.pop();
            }
        }

        walk(&self.roots, query, &mut vec![], &mut keep);
        Some(keep)
    }

    /// Flatten the tree into the rows that should currently be drawn.
    ///
    /// While filtering, folders are treated as expanded so that matches nested
    /// inside collapsed folders are still reachable.
    pub fn visible_rows<'a>(&'a self, filter: Option<&HashSet<NodeId>>) -> Vec<VisibleRow<'a>> {
        fn walk<'a>(
            nodes: &'a [SessionNode],
            depth: usize,
            filter: Option<&HashSet<NodeId>>,
            out: &mut Vec<VisibleRow<'a>>,
        ) {
            for node in nodes {
                if let Some(keep) = filter {
                    if !keep.contains(&node.id) {
                        continue;
                    }
                }

                let (kind, descend) = match &node.kind {
                    NodeKind::Folder { expanded, .. } => {
                        // When filtering we force folders open, but we report
                        // their real state so the disclosure arrow doesn't lie
                        // about what happens when the filter is cleared.
                        (
                            RowKind::Folder {
                                expanded: *expanded,
                            },
                            filter.is_some() || *expanded,
                        )
                    }
                    NodeKind::Session { .. } => (RowKind::Session, false),
                };

                out.push(VisibleRow {
                    id: node.id,
                    name: &node.name,
                    depth,
                    kind,
                });

                if descend {
                    walk(node.children(), depth + 1, filter, out);
                }
            }
        }

        let mut out = vec![];
        walk(&self.roots, 0, filter, &mut out);
        out
    }
}

/// Whether `id` is `node` itself or anywhere beneath it.
fn contains(node: &SessionNode, id: NodeId) -> bool {
    node.id == id || node.children().iter().any(|child| contains(child, id))
}

/// Case-insensitive subsequence match, the same shape of matching the launcher
/// offers. We don't use nucleo here because we only need a yes/no answer and
/// want it to stay a pure function for testing.
fn subsequence_match(haystack: &str, needle: &str) -> bool {
    let mut hay = haystack.chars().flat_map(|c| c.to_lowercase());
    'outer: for want in needle.chars().flat_map(|c| c.to_lowercase()) {
        for have in hay.by_ref() {
            if have == want {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

#[cfg(test)]
mod test {
    use super::*;

    /// roots: [ folder "work" (expanded) [ "alpha", "beta" ], "gamma" ]
    fn sample() -> SessionTree {
        let mut tree = SessionTree::new();
        let work = tree.alloc_id();
        let alpha = tree.alloc_id();
        let beta = tree.alloc_id();
        let gamma = tree.alloc_id();

        let mut folder = SessionNode::folder(work, "work");
        if let NodeKind::Folder { children, .. } = &mut folder.kind {
            children.push(SessionNode::session(
                alpha,
                "alpha",
                SessionSpawn::default(),
            ));
            children.push(SessionNode::session(beta, "beta", SessionSpawn::default()));
        }
        tree.roots.push(folder);
        tree.roots.push(SessionNode::session(
            gamma,
            "gamma",
            SessionSpawn::default(),
        ));
        tree
    }

    /// The names borrow from the tree, not from the row slice, so the lifetime
    /// has to be named rather than elided.
    fn names<'a>(rows: &[VisibleRow<'a>]) -> Vec<(&'a str, usize)> {
        rows.iter().map(|r| (r.name, r.depth)).collect()
    }

    #[test]
    fn session_ids_of_a_removed_folder_include_everything_inside() {
        let mut tree = sample();
        let work = tree.roots[0].id;
        let removed = tree.remove(work).unwrap();
        let names: Vec<NodeId> = removed.session_ids();
        let alpha = removed.children()[0].id;
        let beta = removed.children()[1].id;
        assert_eq!(names, vec![alpha, beta]);

        let gamma = tree.roots[0].id;
        let removed = tree.remove(gamma).unwrap();
        assert_eq!(removed.session_ids(), vec![gamma]);
    }

    #[test]
    fn ids_are_unique_and_monotonic() {
        let mut tree = SessionTree::new();
        let a = tree.alloc_id();
        let b = tree.alloc_id();
        assert_ne!(a, b);
        assert!(b.0 > a.0);
    }

    #[test]
    fn alloc_id_skips_ids_claimed_elsewhere() {
        let mut tree = SessionTree::new();
        tree.roots.push(SessionNode::session(
            NodeId(5),
            "x",
            SessionSpawn::default(),
        ));
        tree.reseed_ids();
        // 6 and 7 still have a saved host or an open workspace from nodes
        // that were deleted; a new node must not inherit either.
        let taken = |id: NodeId| id == NodeId(6) || id == NodeId(7);
        assert_eq!(tree.alloc_id_skipping(taken), NodeId(8));
        // The next one moves on rather than landing on 8 again.
        assert_eq!(tree.alloc_id_skipping(taken), NodeId(9));
    }

    #[test]
    fn reseed_avoids_reusing_ids_from_a_hand_edited_file() {
        let mut tree = SessionTree::new();
        tree.roots.push(SessionNode::session(
            NodeId(42),
            "x",
            SessionSpawn::default(),
        ));
        tree.reseed_ids();
        assert_eq!(tree.alloc_id(), NodeId(43));
    }

    #[test]
    fn workspace_name_is_derived_from_id_not_name() {
        assert_eq!(NodeId(7).workspace_name(), "session:7");
    }

    #[test]
    fn find_locates_nested_nodes() {
        let tree = sample();
        assert_eq!(tree.find(NodeId(2)).map(|n| n.name.as_str()), Some("alpha"));
        assert_eq!(tree.find(NodeId(999)), None);
    }

    #[test]
    fn insert_into_folder() {
        let mut tree = sample();
        let id = tree.alloc_id();
        assert!(tree.insert(
            Some(NodeId(1)),
            SessionNode::session(id, "delta", SessionSpawn::default())
        ));
        assert_eq!(tree.find(NodeId(1)).unwrap().children().len(), 3);
    }

    #[test]
    fn insert_into_a_session_node_is_rejected() {
        let mut tree = sample();
        let id = tree.alloc_id();
        assert!(!tree.insert(
            Some(NodeId(2)),
            SessionNode::session(id, "nope", SessionSpawn::default())
        ));
    }

    #[test]
    fn remove_detaches_the_whole_subtree() {
        let mut tree = sample();
        let removed = tree.remove(NodeId(1)).expect("folder to be removed");
        assert_eq!(removed.children().len(), 2);
        assert_eq!(tree.find(NodeId(2)), None);
        assert_eq!(names(&tree.visible_rows(None)), vec![("gamma", 0)]);
    }

    #[test]
    fn moving_before_a_sibling_reorders_the_folder() {
        let mut tree = sample();
        assert!(tree.move_node(NodeId(3), Placement::Before(NodeId(2))));
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("work", 0), ("beta", 1), ("alpha", 1), ("gamma", 0)]
        );
    }

    #[test]
    fn moving_after_a_node_elsewhere_leaves_the_folder() {
        let mut tree = sample();
        assert!(tree.move_node(NodeId(2), Placement::After(NodeId(4))));
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("work", 0), ("beta", 1), ("gamma", 0), ("alpha", 0)]
        );
    }

    #[test]
    fn moving_into_a_folder_puts_it_last_and_opens_the_folder() {
        let mut tree = sample();
        tree.toggle_expanded(NodeId(1));
        assert!(tree.move_node(NodeId(4), Placement::LastIn(Some(NodeId(1)))));
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("work", 0), ("alpha", 1), ("beta", 1), ("gamma", 1)]
        );
    }

    #[test]
    fn moving_to_the_end_of_the_top_level() {
        let mut tree = sample();
        assert!(tree.move_node(NodeId(2), Placement::LastIn(None)));
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("work", 0), ("beta", 1), ("gamma", 0), ("alpha", 0)]
        );
    }

    #[test]
    fn a_folder_takes_its_contents_along() {
        let mut tree = sample();
        assert!(tree.move_node(NodeId(1), Placement::After(NodeId(4))));
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("gamma", 0), ("work", 0), ("alpha", 1), ("beta", 1)]
        );
    }

    #[test]
    fn a_folder_cannot_go_inside_itself() {
        let mut tree = sample();
        assert!(!tree.can_move(NodeId(1), Placement::LastIn(Some(NodeId(1)))));
        assert!(!tree.can_move(NodeId(1), Placement::Before(NodeId(2))));
        assert!(!tree.move_node(NodeId(1), Placement::After(NodeId(3))));
        assert_eq!(tree, sample(), "a refused move changes nothing");
    }

    #[test]
    fn a_node_cannot_go_beside_itself() {
        let tree = sample();
        assert!(!tree.can_move(NodeId(4), Placement::Before(NodeId(4))));
        assert!(!tree.can_move(NodeId(4), Placement::After(NodeId(4))));
    }

    #[test]
    fn only_a_folder_can_be_moved_into() {
        let tree = sample();
        assert!(!tree.can_move(NodeId(2), Placement::LastIn(Some(NodeId(4)))));
    }

    #[test]
    fn a_missing_node_cannot_move_or_be_moved_to() {
        let tree = sample();
        assert!(!tree.can_move(NodeId(99), Placement::Before(NodeId(2))));
        assert!(!tree.can_move(NodeId(2), Placement::After(NodeId(99))));
        assert!(!tree.can_move(NodeId(2), Placement::LastIn(Some(NodeId(99)))));
    }

    #[test]
    fn rename_preserves_id() {
        let mut tree = sample();
        assert!(tree.rename(NodeId(2), "renamed"));
        assert_eq!(tree.find(NodeId(2)).unwrap().name, "renamed");
        assert_eq!(NodeId(2).workspace_name(), "session:2");
    }

    #[test]
    fn sessions_with_names_skips_folders_and_descends() {
        let tree = sample();
        assert_eq!(
            tree.sessions_with_names(),
            vec![
                (NodeId(2), "alpha".to_string()),
                (NodeId(3), "beta".to_string()),
                (NodeId(4), "gamma".to_string()),
            ]
        );
    }

    #[test]
    fn a_tree_of_folders_has_no_sessions() {
        let mut tree = SessionTree::new();
        let id = tree.alloc_id();
        tree.roots.push(SessionNode::folder(id, "empty"));
        assert!(tree.sessions_with_names().is_empty());
    }

    #[test]
    fn parent_of() {
        let tree = sample();
        assert_eq!(tree.parent_of(NodeId(2)), Some(NodeId(1)));
        assert_eq!(tree.parent_of(NodeId(1)), None);
        assert_eq!(tree.parent_of(NodeId(4)), None);
    }

    #[test]
    fn visible_rows_reports_depth() {
        let tree = sample();
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("work", 0), ("alpha", 1), ("beta", 1), ("gamma", 0)]
        );
    }

    #[test]
    fn collapsed_folders_hide_their_children() {
        let mut tree = sample();
        assert_eq!(tree.toggle_expanded(NodeId(1)), Some(false));
        assert_eq!(
            names(&tree.visible_rows(None)),
            vec![("work", 0), ("gamma", 0)]
        );
    }

    #[test]
    fn toggle_expanded_on_a_session_is_none() {
        let mut tree = sample();
        assert_eq!(tree.toggle_expanded(NodeId(2)), None);
    }

    #[test]
    fn empty_query_means_no_filter() {
        let tree = sample();
        assert_eq!(tree.matching_ids(""), None);
        assert_eq!(tree.matching_ids("   "), None);
    }

    #[test]
    fn filter_keeps_matches_and_their_ancestors() {
        let tree = sample();
        let keep = tree.matching_ids("alpha").unwrap();
        assert!(keep.contains(&NodeId(2)), "the match itself");
        assert!(keep.contains(&NodeId(1)), "its parent folder");
        assert!(!keep.contains(&NodeId(3)));
        assert!(!keep.contains(&NodeId(4)));
    }

    #[test]
    fn filter_reaches_into_collapsed_folders() {
        let mut tree = sample();
        tree.toggle_expanded(NodeId(1));
        let keep = tree.matching_ids("beta").unwrap();
        assert_eq!(
            names(&tree.visible_rows(Some(&keep))),
            vec![("work", 0), ("beta", 1)],
            "a collapsed folder still yields its matching children while filtering"
        );
    }

    #[test]
    fn filter_reports_the_real_collapsed_state() {
        let mut tree = sample();
        tree.toggle_expanded(NodeId(1));
        let keep = tree.matching_ids("beta").unwrap();
        let rows = tree.visible_rows(Some(&keep));
        assert_eq!(rows[0].kind, RowKind::Folder { expanded: false });
    }

    #[test]
    fn subsequence_matching_is_case_insensitive_and_gappy() {
        assert!(subsequence_match("cg-0-150", "cg0"));
        assert!(subsequence_match("Local VM", "lvm"));
        assert!(!subsequence_match("alpha", "alphax"));
    }

    #[test]
    fn session_spawn_defaults_to_the_default_domain() {
        // CurrentPaneDomain would resolve against the workspace we are leaving.
        assert_eq!(
            SessionSpawn::default().domain,
            SpawnTabDomain::DefaultDomain
        );
    }

    #[test]
    fn converts_to_spawn_command() {
        let mut spawn = SessionSpawn::default();
        spawn.cwd = Some(PathBuf::from("/tmp"));
        spawn.args = Some(vec!["bash".to_string()]);
        spawn.env.insert("FOO".to_string(), "bar".to_string());

        let cmd: SpawnCommand = (&spawn).into();
        assert_eq!(cmd.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(cmd.args, Some(vec!["bash".to_string()]));
        assert_eq!(cmd.domain, SpawnTabDomain::DefaultDomain);
        assert_eq!(
            cmd.set_environment_variables.get("FOO").map(|s| s.as_str()),
            Some("bar")
        );
    }
}
