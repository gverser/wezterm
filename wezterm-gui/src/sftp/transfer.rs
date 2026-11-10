//! The file page's transfer queue: one file at a time, in the background,
//! with progress, cancellation, and no half-written files left behind.

use crate::sessions::monitor_proto::human_bytes;
use crate::sftp::model::explain_sftp_error;
use mux::domain::DomainId;
use smol::io::{AsyncReadExt, AsyncWriteExt};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use wezterm_ssh::{FileType, Metadata, RenameOptions, Sftp};

/// Bytes per read. Always the same: `wezterm_ssh`'s sftp `File` sizes its
/// pending read from the first buffer it is polled with.
pub const CHUNK: usize = 32 * 1024;

/// What a cancelled transfer finishes with.
pub const CANCELLED: &str = "已取消";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Upload,
    Download,
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Upload => "上传",
            Self::Download => "下载",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    Upload { local: PathBuf, remote: String },
    Download { remote: String, local: PathBuf },
}

impl Direction {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Upload { .. } => Kind::Upload,
            Self::Download { .. } => Kind::Download,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransferJob {
    pub id: u64,
    pub domain: DomainId,
    pub dir: Direction,
    pub size: Option<u64>,
}

impl TransferJob {
    /// The file's name, for the status line.
    pub fn name(&self) -> String {
        match &self.dir {
            Direction::Upload { local, .. } => local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| local.display().to_string()),
            Direction::Download { remote, .. } => remote
                .rsplit('/')
                .next()
                .unwrap_or(remote.as_str())
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
    pub started: Instant,
}

#[derive(Default)]
pub struct TransferQueue {
    pending: VecDeque<TransferJob>,
    pub active: Option<(TransferJob, Progress, Arc<AtomicBool>)>,
    /// How the last transfer ended.
    pub last_result: Option<(Kind, String, Result<(), String>)>,
    /// Something to say that is not about a transfer, such as a folder that
    /// can't be uploaded; replaced by the next result.
    pub note: Option<String>,
}

impl TransferQueue {
    pub fn push(&mut self, job: TransferJob) {
        self.pending.push_back(job);
    }

    /// The next job, if none is running; its cancel flag comes with it.
    pub fn start_next(&mut self) -> Option<(TransferJob, Arc<AtomicBool>)> {
        if self.active.is_some() {
            return None;
        }
        let job = self.pending.pop_front()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Progress {
            done: 0,
            total: job.size,
            started: Instant::now(),
        };
        self.active = Some((job.clone(), progress, Arc::clone(&cancel)));
        Some((job, cancel))
    }

    pub fn progress(&mut self, id: u64, done: u64) {
        if let Some((job, progress, _)) = self.active.as_mut() {
            if job.id == id {
                progress.done = done;
            }
        }
    }

    pub fn finish(&mut self, id: u64, result: Result<(), String>) {
        match &self.active {
            Some((job, _, _)) if job.id == id => {}
            _ => return,
        }
        let (job, _, _) = self.active.take().expect("checked above");
        self.last_result = Some((job.dir.kind(), job.name(), result));
        self.note = None;
    }

    pub fn cancel_active(&mut self) {
        if let Some((_, _, cancel)) = &self.active {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn note(&mut self, text: String) {
        self.note = Some(text);
        self.last_result = None;
    }

    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    pub fn status_line(&self) -> Option<String> {
        self.status_line_at(Instant::now())
    }

    /// The line at the bottom of the file page, with rates worked out as of
    /// `now`.
    pub fn status_line_at(&self, now: Instant) -> Option<String> {
        let line = if let Some((job, progress, _)) = &self.active {
            let elapsed = now
                .saturating_duration_since(progress.started)
                .as_secs_f64();
            let rate = if elapsed > 0. {
                (progress.done as f64 / elapsed) as u64
            } else {
                0
            };
            let amount = match progress.total {
                Some(total) if total > 0 => format!("{}%", progress.done * 100 / total),
                _ => human_bytes(progress.done),
            };
            format!(
                "{} {} {}  {}/s",
                job.dir.kind().label(),
                job.name(),
                amount,
                human_bytes(rate)
            )
        } else if let Some(note) = &self.note {
            note.clone()
        } else if let Some((kind, name, result)) = &self.last_result {
            match result {
                Ok(()) => format!("{}完成 {name}", kind.label()),
                Err(err) if err == CANCELLED => format!("已取消{} {name}", kind.label()),
                Err(err) => format!("{}失败 {name}：{err}", kind.label()),
            }
        } else {
            return None;
        };
        Some(if self.pending.is_empty() {
            line
        } else {
            format!("{line}（还有 {} 个）", self.pending.len())
        })
    }
}

/// Where a download is written until it is complete.
pub fn partial_download_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn sftp_failure(err: impl std::fmt::Display) -> String {
    explain_sftp_error(&err.to_string())
}

fn io_failure(err: std::io::Error) -> String {
    err.to_string()
}

/// Copy `remote` to `local` by way of a `.part` file, which is renamed into
/// place when complete and removed otherwise. `report` gets the running
/// byte count after each chunk.
pub async fn run_download(
    sftp: Sftp,
    remote: String,
    local: PathBuf,
    cancel: Arc<AtomicBool>,
    mut report: impl FnMut(u64),
) -> Result<(), String> {
    let partial = partial_download_path(&local);
    let result = async {
        let mut src = sftp.open(remote.as_str()).await.map_err(sftp_failure)?;
        let mut dst = smol::fs::File::create(&partial).await.map_err(io_failure)?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(CANCELLED.to_string());
            }
            let n = src.read(&mut buf).await.map_err(sftp_failure)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(io_failure)?;
            done += n as u64;
            report(done);
        }
        dst.flush().await.map_err(io_failure)?;
        dst.sync_all().await.map_err(io_failure)?;
        drop(dst);
        // Replaces an existing file, which the save dialog has already
        // asked about.
        smol::fs::rename(&partial, &local).await.map_err(io_failure)
    }
    .await;
    if result.is_err() {
        let _ = smol::fs::remove_file(&partial).await;
    }
    result
}

/// Where an upload to `remote` is written until it is complete: beside it,
/// so that putting it in place is a rename within one directory.
pub fn partial_upload_path(remote: &str) -> String {
    format!("{remote}.wezterm-part")
}

/// Copy `local` to `remote` by way of a partial file beside it, which is
/// renamed over `remote` only once complete. Until then a file already at
/// `remote` is untouched, so cancelling or losing the connection leaves it as
/// it was; the partial file is removed. A replaced file's mode is kept, and
/// its owner too where the server allows. Uploading to a symlink replaces
/// the file it points at, as writing through it did.
pub async fn run_upload(
    sftp: Sftp,
    local: PathBuf,
    remote: String,
    cancel: Arc<AtomicBool>,
    mut report: impl FnMut(u64),
) -> Result<(), String> {
    let mut src = smol::fs::File::open(&local).await.map_err(io_failure)?;
    let target = match sftp.symlink_metadata(remote.as_str()).await {
        Ok(meta) if meta.is_symlink() => sftp
            .canonicalize(remote.as_str())
            .await
            .map_err(sftp_failure)?
            .into_string(),
        _ => remote,
    };
    let existing = sftp.metadata(target.as_str()).await.ok();
    let partial = partial_upload_path(&target);

    let mut dst = sftp.create(partial.as_str()).await.map_err(sftp_failure)?;
    let written = async {
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(CANCELLED.to_string());
            }
            let n = src.read(&mut buf).await.map_err(io_failure)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(sftp_failure)?;
            done += n as u64;
            report(done);
        }
        dst.close().await.map_err(sftp_failure)?;
        if let Some(existing) = &existing {
            keep_mode_and_owner(&sftp, &partial, existing).await?;
        }
        Ok(())
    }
    .await;
    drop(dst);
    if let Err(err) = written {
        let _ = sftp.remove_file(partial.as_str()).await;
        return Err(err);
    }
    put_in_place(&sftp, &partial, &target, existing.is_some()).await
}

/// Give the finished upload at `partial` the mode of the file it replaces,
/// and its owner and group if they differ, which only root can change; a
/// refusal there is logged and otherwise ignored.
async fn keep_mode_and_owner(
    sftp: &Sftp,
    partial: &str,
    existing: &Metadata,
) -> Result<(), String> {
    let mut attrs = Metadata {
        ty: FileType::File,
        permissions: existing.permissions,
        size: None,
        uid: None,
        gid: None,
        accessed: None,
        modified: None,
    };
    if attrs.permissions.is_some() {
        sftp.set_metadata(partial, attrs)
            .await
            .map_err(sftp_failure)?;
    }
    let mine = sftp.metadata(partial).await.map_err(sftp_failure)?;
    if (mine.uid, mine.gid) != (existing.uid, existing.gid) && existing.uid.is_some() {
        attrs.permissions = None;
        attrs.uid = existing.uid;
        attrs.gid = existing.gid;
        if let Err(err) = sftp.set_metadata(partial, attrs).await {
            log::warn!("upload {partial}: keeping the replaced file's owner: {err}");
        }
    }
    Ok(())
}

/// Rename the finished upload over `target`. OpenSSH replaces an existing
/// file in one step; a server that refuses to rename onto an existing name
/// gets the old file removed first, and if the rename still fails the upload
/// is left at `partial` and the message says so.
async fn put_in_place(
    sftp: &Sftp,
    partial: &str,
    target: &str,
    existed: bool,
) -> Result<(), String> {
    let Err(err) = sftp.rename(partial, target, RenameOptions::default()).await else {
        return Ok(());
    };
    if !existed {
        let _ = sftp.remove_file(partial).await;
        return Err(sftp_failure(err));
    }
    if let Err(err) = sftp.remove_file(target).await {
        let _ = sftp.remove_file(partial).await;
        return Err(sftp_failure(err));
    }
    sftp.rename(partial, target, RenameOptions::default())
        .await
        .map_err(|err| format!("{}；上传的文件留在 {partial}", sftp_failure(err)))
}

#[cfg(test)]
mod test {
    use super::*;
    use std::time::Duration;

    fn upload(id: u64, name: &str, size: Option<u64>) -> TransferJob {
        TransferJob {
            id,
            domain: 0,
            dir: Direction::Upload {
                local: PathBuf::from(format!("C:\\data\\{name}")),
                remote: format!("/srv/{name}"),
            },
            size,
        }
    }

    fn download(id: u64, name: &str, size: Option<u64>) -> TransferJob {
        TransferJob {
            id,
            domain: 0,
            dir: Direction::Download {
                remote: format!("/var/{name}"),
                local: PathBuf::from(format!("C:\\Downloads\\{name}")),
            },
            size,
        }
    }

    #[test]
    fn queue_runs_one_at_a_time() {
        let mut q = TransferQueue::default();
        q.push(upload(1, "a.tar", Some(10)));
        q.push(upload(2, "b.tar", Some(10)));
        let (first, _) = q.start_next().unwrap();
        assert_eq!(first.id, 1);
        assert!(q.start_next().is_none(), "one is already running");
        q.finish(1, Ok(()));
        let (second, _) = q.start_next().unwrap();
        assert_eq!(second.id, 2);
        q.finish(2, Ok(()));
        assert!(q.start_next().is_none());
    }

    #[test]
    fn finish_of_another_job_is_ignored() {
        let mut q = TransferQueue::default();
        q.push(upload(1, "a.tar", Some(10)));
        q.start_next().unwrap();
        q.finish(7, Ok(()));
        assert!(q.active.is_some());
    }

    #[test]
    fn cancel_marks_flag() {
        let mut q = TransferQueue::default();
        q.push(upload(1, "a.tar", Some(10)));
        let (_, cancel) = q.start_next().unwrap();
        assert!(!cancel.load(Ordering::SeqCst));
        q.cancel_active();
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn status_line_formats() {
        let mut q = TransferQueue::default();
        assert_eq!(q.status_line(), None);

        // in progress, with the rate over the time since it started
        q.push(upload(1, "a.tar", Some(100)));
        q.start_next().unwrap();
        q.progress(1, 45);
        let started = q.active.as_ref().unwrap().1.started;
        let now = started + Duration::from_secs(1);
        assert_eq!(q.status_line_at(now).unwrap(), "上传 a.tar 45%  45B/s");

        // more waiting behind it
        q.push(download(2, "b.bin", None));
        q.push(download(3, "c.bin", None));
        assert_eq!(
            q.status_line_at(now).unwrap(),
            "上传 a.tar 45%  45B/s（还有 2 个）"
        );

        // done
        q.finish(1, Ok(()));
        assert_eq!(
            q.status_line_at(now).unwrap(),
            "上传完成 a.tar（还有 2 个）"
        );

        // unknown size shows the amount so far
        q.start_next().unwrap();
        q.progress(2, 2202009);
        let started = q.active.as_ref().unwrap().1.started;
        assert_eq!(
            q.status_line_at(started + Duration::from_secs(2)).unwrap(),
            "下载 b.bin 2.1M  1.0M/s（还有 1 个）"
        );

        // failed, with the original message
        q.finish(2, Err("Sftp error code 3".to_string()));
        assert_eq!(
            q.status_line().unwrap(),
            "下载失败 b.bin：Sftp error code 3（还有 1 个）"
        );

        // cancelled
        q.start_next().unwrap();
        q.cancel_active();
        q.finish(3, Err(CANCELLED.to_string()));
        assert_eq!(q.status_line().unwrap(), "已取消下载 c.bin");

        // a note replaces the last result
        q.note("暂不支持上传文件夹：photos".to_string());
        assert_eq!(q.status_line().unwrap(), "暂不支持上传文件夹：photos");
    }

    #[test]
    fn partial_path_appends_suffix() {
        assert_eq!(
            partial_download_path(Path::new("C:\\Downloads\\a.tar.gz")),
            PathBuf::from("C:\\Downloads\\a.tar.gz.part")
        );
    }
}
