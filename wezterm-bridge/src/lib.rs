//! A local IPC service that exposes mux panes to an external client.
//!
//! The bridge holds no state proportional to output: `PaneOutput` carries
//! only a pane id and the content is already in the scrollback, so a dirty
//! flag and a seqno watermark are all that is kept. Memory is flat whether
//! the client is away for a minute or for hours.

pub mod commands;
pub mod feishu;
pub mod keys;
pub mod mux_access;
pub mod proto;
pub mod server;
pub mod service;
pub mod state;
pub mod token;

pub use proto::{Inbound, Outbound, PaneInfo};
pub use feishu::FeishuService;
pub use state::{BridgeState, PaneAccess, PaneState};
