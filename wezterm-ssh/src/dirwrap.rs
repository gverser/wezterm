use crate::sftp::types::Metadata;
use crate::sftp::{SftpChannelError, SftpChannelResult};
use camino::Utf8PathBuf;

/// Whether a failed read just means the directory has no more entries:
/// libssh reports that as an `UnexpectedEof` IO error, libssh2 as the SFTP
/// EOF status.
fn is_end_of_dir(err: &SftpChannelError) -> bool {
    match err {
        SftpChannelError::Sftp(crate::sftp::error::SftpError::Eof) => true,
        SftpChannelError::FileIo(io) => io.kind() == std::io::ErrorKind::UnexpectedEof,
        _ => false,
    }
}

/// Up to `max` entries from `next`, fewer when the directory ends first;
/// empty once it has ended.
fn take_entries<T>(
    max: usize,
    mut next: impl FnMut() -> SftpChannelResult<T>,
) -> SftpChannelResult<Vec<T>> {
    let mut entries = vec![];
    while entries.len() < max {
        match next() {
            Ok(entry) => entries.push(entry),
            Err(err) if is_end_of_dir(&err) => break,
            Err(err) => return Err(err),
        }
    }
    Ok(entries)
}

pub(crate) enum DirWrap {
    #[cfg(feature = "ssh2")]
    Ssh2(ssh2::File),

    #[cfg(feature = "libssh-rs")]
    LibSsh(libssh_rs::SftpDir),
}

impl DirWrap {
    /// Up to `max` entries in one go; empty at the end of the directory.
    pub fn read_dir_many(&mut self, max: usize) -> SftpChannelResult<Vec<(Utf8PathBuf, Metadata)>> {
        take_entries(max, || self.read_dir())
    }

    pub fn read_dir(&mut self) -> SftpChannelResult<(Utf8PathBuf, Metadata)> {
        match self {
            #[cfg(feature = "ssh2")]
            Self::Ssh2(file) => {
                use std::convert::TryFrom;
                file.readdir()
                    .map_err(SftpChannelError::from)
                    .and_then(|(path, stat)| match Utf8PathBuf::try_from(path) {
                        Ok(path) => Ok((path, Metadata::from(stat))),
                        Err(x) => Err(SftpChannelError::from(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            x,
                        ))),
                    })
            }

            #[cfg(feature = "libssh-rs")]
            Self::LibSsh(dir) => loop {
                match dir.read_dir() {
                    None => {
                        break Err(SftpChannelError::from(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "no more files",
                        )))
                    }
                    Some(Err(err)) => break Err(SftpChannelError::from(err)),
                    Some(Ok(metadata)) => {
                        // A name that isn't UTF-8 has no Utf8PathBuf; skip
                        // it rather than panic the session thread.
                        let Some(name) = metadata.name() else {
                            log::warn!("read_dir: skipping an entry whose name is not UTF-8");
                            continue;
                        };
                        let path: Utf8PathBuf = name.into();
                        let md: Metadata = metadata.into();
                        break Ok((path, md));
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::SftpError;

    fn eof_io() -> SftpChannelError {
        SftpChannelError::from(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "no more files",
        ))
    }

    /// A reader over `n` entries that ends the way `end` says.
    fn reader(n: usize, end: fn() -> SftpChannelError) -> impl FnMut() -> SftpChannelResult<usize> {
        let mut i = 0;
        move || {
            i += 1;
            if i <= n {
                Ok(i)
            } else {
                Err(end())
            }
        }
    }

    #[test]
    fn take_entries_stops_at_max() {
        let mut next = reader(250, eof_io);
        assert_eq!(take_entries(100, &mut next).unwrap().len(), 100);
        assert_eq!(take_entries(100, &mut next).unwrap().len(), 100);
        assert_eq!(
            take_entries(100, &mut next).unwrap(),
            (201..=250).collect::<Vec<_>>()
        );
        assert!(take_entries(100, &mut next).unwrap().is_empty());
    }

    #[test]
    fn take_entries_knows_both_ways_a_directory_ends() {
        let mut next = reader(3, || SftpChannelError::Sftp(SftpError::Eof));
        assert_eq!(take_entries(10, &mut next).unwrap(), vec![1, 2, 3]);
        assert!(take_entries(10, &mut next).unwrap().is_empty());
        let mut next = reader(0, eof_io);
        assert!(take_entries(10, &mut next).unwrap().is_empty());
    }

    #[test]
    fn take_entries_reports_a_real_failure() {
        let mut next = reader(2, || SftpChannelError::Sftp(SftpError::PermissionDenied));
        assert!(matches!(
            take_entries(10, &mut next),
            Err(SftpChannelError::Sftp(SftpError::PermissionDenied))
        ));
    }
}
