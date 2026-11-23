#[cfg(not(any(feature = "libssh-rs", feature = "ssh2")))]
compile_error!("Either libssh-rs or ssh2 must be enabled!");

#[cfg(feature = "libssh-rs")]
mod alive;
mod auth;
mod channelwrap;
mod config;
mod dirwrap;
mod filewrap;
mod host;
mod pty;
mod session;
mod sessioninner;
mod sessionwrap;
mod sftp;
mod sftpwrap;

pub use auth::*;
pub use config::*;
pub use host::*;
pub use pty::*;
pub use session::*;
pub use sessioninner::describe_error;
pub use sftp::error::*;
pub use sftp::types::*;
pub use sftp::*;

// NOTE: Re-exported as is exposed in a public API of this crate
pub use camino::{Utf8Path, Utf8PathBuf};
pub use filedescriptor::FileDescriptor;
/// How sftp paths carry bytes that aren't UTF-8 (libssh backend only; ssh2
/// still leaves such names out).
#[cfg(feature = "libssh-rs")]
pub use libssh_rs::raw_names;
pub use portable_pty::{Child, ChildKiller, MasterPty, PtySize};
