//! The saved session tree, and the ssh hosts some of its nodes connect to.
//!
//! `tree` is the data model, `store` is its persistence. Both are free of
//! rendering concerns: the gui sidebar and the mux-server bridge share them.

pub mod ssh;
pub mod store;
pub mod tree;

pub use ssh::{ssh_hosts_file_name, with_ssh_hosts, SshAuth, SshHost, SshHostStore};
pub use store::{sessions_file_name, SessionStore};
pub use tree::{
    NodeId, NodeKind, Placement, RowKind, SessionNode, SessionSpawn, SessionTree, VisibleRow,
};
