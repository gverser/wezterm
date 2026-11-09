//! The sidebar's file page: an sftp browser over the active pane's ssh
//! connection.
//!
//! What each connection is showing lives on the window (`TermWindow.files`),
//! one `FilesConn` per sidebar ssh domain, so switching to a pane on another
//! server and back finds the page where it was left. The requests themselves
//! run on the main thread's executor and report back through
//! `TermWindowNotif::Apply`.

pub mod model;

use crate::sftp::model::FileEntry;
use mux::domain::DomainId;
use mux::pane::PaneId;
use std::collections::HashMap;
use wezterm_ssh::Sftp;

/// The buttons along the top of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesAction {
    Up,
    Refresh,
    Home,
    NewFolder,
    Upload,
}

impl FilesAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Up => "↑",
            Self::Refresh => "⟳",
            Self::Home => "⌂",
            Self::NewFolder => "+",
            Self::Upload => "⇪上传",
        }
    }
}

/// The page's right-click menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesMenuAction {
    Download,
    Rename,
    Delete,
    CopyPath,
    NewFolder,
    Refresh,
}

impl FilesMenuAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Download => "下载",
            Self::Rename => "重命名",
            Self::Delete => "删除",
            Self::CopyPath => "复制路径",
            Self::NewFolder => "新建文件夹",
            Self::Refresh => "刷新",
        }
    }
}

pub struct FilesMenu {
    /// The row that was clicked, or None for the empty area below the list.
    pub target: Option<usize>,
    pub x: f32,
    pub y: f32,
    pub items: Vec<FilesMenuAction>,
}

/// One connection's page.
pub struct FilesConn {
    /// `RemoteSshDomain::authenticated_session`'s counter: a reconnect
    /// starts the page afresh.
    pub generation: u64,
    pub sftp: Sftp,
    /// The directory listed; empty until the home directory is known.
    pub cwd: String,
    pub home: Option<String>,
    pub entries: Vec<FileEntry>,
    pub loading: bool,
    /// Why the current directory could not be listed.
    pub error: Option<String>,
    /// The last operation's failure (rename, delete, ...), shown above the
    /// list without hiding it.
    pub notice: Option<String>,
    pub follow: bool,
    pub last_followed: Option<String>,
    pub selected: Option<usize>,
    pub scroll: usize,
    /// Bumped per listing request; only the latest one's answer is used.
    pub listing_seq: u64,
}

#[derive(Default)]
pub struct FilesUi {
    pub conns: HashMap<DomainId, FilesConn>,
    /// The path bar's text while it is being edited.
    pub path_edit: Option<String>,
    /// The path bar was just clicked and still holds the whole current path
    /// as if selected: the first thing typed replaces it, as in a file
    /// manager's address bar. A relative path still goes below the current
    /// directory.
    pub path_fresh: bool,
    pub menu: Option<FilesMenu>,
    /// Bumped whenever anything the page draws changes; part of the
    /// sidebar's cache key.
    pub generation: u64,
    /// What following was last judged against: the directory counter and
    /// the active pane. Either changing means another look.
    pub follow_seen: Option<(u64, Option<(DomainId, PaneId)>)>,
}
