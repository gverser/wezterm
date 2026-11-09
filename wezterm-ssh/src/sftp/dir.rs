use super::{Metadata, SessionRequest, SessionSender, SftpChannelResult, SftpRequest};
use camino::Utf8PathBuf;
use smol::channel::{bounded, Sender};
use std::fmt;

pub(crate) type DirId = usize;

/// A file handle to an SFTP connection.
pub struct Dir {
    pub(crate) dir_id: DirId,
    tx: Option<SessionSender>,
}

impl fmt::Debug for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dir").field("dir_id", &self.dir_id).finish()
    }
}

#[derive(Debug)]
pub(crate) enum DirRequest {
    Close(DirId, Sender<SftpChannelResult<()>>),
    ReadDir(DirId, Sender<SftpChannelResult<(Utf8PathBuf, Metadata)>>),
    ReadDirMany(
        DirId,
        usize,
        Sender<SftpChannelResult<Vec<(Utf8PathBuf, Metadata)>>>,
    ),
}

impl Drop for Dir {
    /// Attempts to close the file that exists in the dedicated ssh2 thread
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let (reply, _) = bounded(1);
            let _ = tx.try_send(SessionRequest::Sftp(SftpRequest::Dir(DirRequest::Close(
                self.dir_id,
                reply,
            ))));
        }
    }
}

impl Dir {
    pub(crate) fn new(dir_id: DirId) -> Self {
        Self { dir_id, tx: None }
    }

    pub(crate) fn initialize_sender(&mut self, sender: SessionSender) {
        self.tx.replace(sender);
    }

    /// Reads a block of data from a handle and returns file entry information for the next entry,
    /// if any.
    ///
    /// Note that this provides raw access to the readdir function from libssh2. This will return
    /// an error when there are no more files to read, and files such as . and .. will be included
    /// in the return values.
    ///
    /// Also note that the return paths will not be absolute paths, they are the filenames of the
    /// files in this directory.
    ///
    /// See [`ssh2::Dir::readdir`] for more information.
    pub async fn read_dir(&self) -> anyhow::Result<(Utf8PathBuf, Metadata)> {
        let (reply, rx) = bounded(1);
        self.tx
            .as_ref()
            .unwrap()
            .send(SessionRequest::Sftp(SftpRequest::Dir(DirRequest::ReadDir(
                self.dir_id,
                reply,
            ))))
            .await?;
        let result = rx.recv().await??;
        Ok(result)
    }

    /// Up to `max` entries, as [`Dir::read_dir`] returns them, in one
    /// request; an empty list once the directory has no more.
    ///
    /// Each request is a full turn of the session thread, which costs about
    /// half a millisecond whatever the link: reading a directory of 20000
    /// files an entry at a time took eleven seconds even on a local network.
    /// A batch of about a hundred -- what an OpenSSH server sends per
    /// READDIR reply -- keeps that small while still letting the session's
    /// other channels in between batches.
    pub async fn read_dir_many(&self, max: usize) -> anyhow::Result<Vec<(Utf8PathBuf, Metadata)>> {
        let (reply, rx) = bounded(1);
        self.tx
            .as_ref()
            .unwrap()
            .send(SessionRequest::Sftp(SftpRequest::Dir(
                DirRequest::ReadDirMany(self.dir_id, max, reply),
            )))
            .await?;
        let result = rx.recv().await??;
        Ok(result)
    }
}
