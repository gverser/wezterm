//! The sidebar's file page: an sftp browser over the active pane's ssh
//! connection.
//!
//! What each connection is showing lives on the window (`TermWindow.files`),
//! one `FilesConn` per sidebar ssh domain, so switching to a pane on another
//! server and back finds the page where it was left. The requests themselves
//! run on the main thread's executor and report back through
//! `TermWindowNotif::Apply`.

pub mod dialogs;
#[cfg(test)]
mod live_test;
pub mod model;
pub mod transfer;

use crate::sftp::model::{explain_sftp_error, FileEntry};
use crate::sftp::transfer::{TransferJob, TransferQueue};
use mux::domain::DomainId;
use mux::pane::PaneId;
use std::collections::{HashMap, VecDeque};
use wezterm_ssh::{RenameOptions, Sftp};

fn describe(err: impl std::fmt::Display) -> String {
    explain_sftp_error(&err.to_string())
}

/// How many entries to ask the session thread for at a time.
const LIST_BATCH: usize = 100;

/// The entries of `path`, a batch at a time.
///
/// `Sftp::read_dir` reads a whole directory in one request, and the session
/// thread serves nothing else while it does: over a 50ms link, a directory of
/// 20000 files froze every terminal on the connection for ten seconds.
/// Between batches the terminals' traffic gets through.
pub async fn list_dir(sftp: &Sftp, path: &str) -> Result<Vec<FileEntry>, String> {
    let dir = sftp.open_dir(path).await.map_err(describe)?;
    let mut entries = vec![];
    loop {
        let batch = dir.read_dir_many(LIST_BATCH).await.map_err(describe)?;
        if batch.is_empty() {
            return Ok(entries);
        }
        for (name, meta) in batch {
            let name = name
                .file_name()
                .map(str::to_string)
                .unwrap_or_else(|| name.to_string());
            if name == "." || name == ".." {
                continue;
            }
            entries.push(FileEntry {
                name,
                is_dir: meta.ty.is_dir(),
                is_link: meta.ty.is_symlink(),
                size: if meta.ty.is_dir() { None } else { meta.size },
            });
        }
    }
}

/// Whether `path` has nothing in it but `.` and `..`; stops at the first
/// entry rather than reading the whole directory.
pub async fn dir_is_empty(sftp: &Sftp, path: &str) -> Result<bool, String> {
    let dir = sftp.open_dir(path).await.map_err(describe)?;
    loop {
        let batch = dir.read_dir_many(3).await.map_err(describe)?;
        if batch.is_empty() {
            return Ok(true);
        }
        let only_dots = batch.iter().all(|(name, _)| {
            let name = name.file_name().unwrap_or(name.as_str());
            name == "." || name == ".."
        });
        if !only_dots {
            return Ok(false);
        }
    }
}

/// Rename `old` to `new`, refusing if something already has the new name.
///
/// Asking the server not to overwrite isn't enough: libssh ignores the
/// options and renames with OpenSSH's posix-rename, which replaces whatever
/// was there. So look first; a file that appears in between is the one case
/// this misses.
pub async fn rename_file(sftp: &Sftp, old: &str, new: &str) -> Result<(), String> {
    if sftp.symlink_metadata(new).await.is_ok() {
        let name = new.rsplit('/').next().unwrap_or(new);
        let name = crate::sftp::model::display_name(name);
        return Err(format!("重命名失败：已有同名的「{name}」"));
    }
    let opts = RenameOptions {
        overwrite: false,
        atomic: false,
        native: false,
    };
    sftp.rename(old, new, opts)
        .await
        .map_err(|err| format!("重命名失败：{}", describe(err)))
}

/// The buttons along the top of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesAction {
    Up,
    Refresh,
    Home,
    NewFolder,
    Upload,
    /// The ✕ by the transfer line: cancel the running transfer, or dismiss
    /// the line once it has finished.
    CancelTransfer,
}

impl FilesAction {
    pub fn icon(&self) -> &'static str {
        use crate::termwindow::theme::icon;
        match self {
            Self::Up => icon::UP,
            Self::Refresh => icon::REFRESH,
            Self::Home => icon::HOME,
            Self::NewFolder => icon::NEW_FOLDER,
            Self::Upload => icon::UPLOAD,
            Self::CancelTransfer => icon::CLOSE,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Up => "↑",
            Self::Refresh => "⟳",
            Self::Home => "⌂",
            Self::NewFolder => "+",
            Self::Upload => "⇪上传",
            Self::CancelTransfer => "✕",
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
    pub fn icon(&self) -> &'static str {
        use crate::termwindow::theme::icon;
        match self {
            Self::Download => icon::DOWNLOAD,
            Self::Rename => icon::EDIT,
            Self::Delete => icon::TRASH,
            Self::CopyPath => icon::COPY,
            Self::NewFolder => icon::NEW_FOLDER,
            Self::Refresh => icon::REFRESH,
        }
    }

    /// The menu keeps its order; these only say where the lines go.
    pub fn group(&self) -> u8 {
        match self {
            Self::Download => 0,
            Self::Rename | Self::Delete => 1,
            Self::CopyPath => 2,
            Self::NewFolder | Self::Refresh => 3,
        }
    }

    pub fn danger(&self) -> bool {
        *self == Self::Delete
    }

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
    pub transfers: TransferQueue,
    /// Uploads that would replace a remote file, waiting for the user to say
    /// whether to; asked about one at a time.
    pub overwrite_pending: VecDeque<TransferJob>,
    pub next_transfer_id: u64,
    /// The user has been told what closing the window would interrupt and
    /// closed it anyway.
    pub close_confirmed: bool,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn the_file_menu_groups_keep_its_order_and_only_delete_is_dangerous() {
        use FilesMenuAction::*;
        let all = [Download, Rename, Delete, CopyPath, NewFolder, Refresh];
        assert_eq!(all.map(|a| a.group()), [0, 1, 1, 2, 3, 3]);
        assert_eq!(
            all.map(|a| a.danger()),
            [false, false, true, false, false, false]
        );
    }

    #[test]
    fn every_toolbar_button_has_an_icon() {
        use crate::termwindow::theme::icon;
        assert_eq!(FilesAction::Upload.icon(), icon::UPLOAD);
        assert_eq!(FilesAction::CancelTransfer.icon(), icon::CLOSE);
    }
}
