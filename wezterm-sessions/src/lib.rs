//! The saved session tree.
//!
//! `tree` is the data model, `store` is its persistence. Both are free of
//! rendering concerns: the gui sidebar and the mux-server bridge share them.

pub mod store;
pub mod tree;

pub use store::{sessions_file_name, SessionStore};
pub use tree::{
    NodeId, NodeKind, Placement, RowKind, SessionNode, SessionSpawn, SessionTree, VisibleRow,
};
