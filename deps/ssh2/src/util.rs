use std::borrow::Cow;
use std::path::{Path, PathBuf};

use ssh_raw_names;
use {raw, Error, ErrorCode};

// Names that aren't UTF-8 (a server whose files are named in GBK, say) travel
// as in libssh-rs: each such byte as one character of a private use range
// (see ssh_raw_names), turned back into that byte on the way to the server.
// Published, mkpath panicked on such a name on Windows, which took the whole
// session thread down with it.

#[cfg(unix)]
pub fn path2bytes(p: &Path) -> Result<Cow<'_, [u8]>, Error> {
    use std::ffi::OsStr;
    use std::os::unix::prelude::*;
    match p.to_str() {
        Some(s) => check(ssh_raw_names::to_bytes(s)),
        None => {
            let s: &OsStr = p.as_ref();
            check(Cow::Borrowed(s.as_bytes()))
        }
    }
}
#[cfg(windows)]
pub fn path2bytes(p: &Path) -> Result<Cow<'_, [u8]>, Error> {
    p.to_str()
        .map(ssh_raw_names::to_bytes)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Session(raw::LIBSSH2_ERROR_INVAL),
                "only unicode paths on windows may be used",
            )
        })
        .map(|bytes| {
            if bytes.contains(&b'\\') {
                // Normalize to Unix-style path separators
                let mut bytes = bytes.into_owned();
                for b in &mut bytes {
                    if *b == b'\\' {
                        *b = b'/';
                    }
                }
                Cow::Owned(bytes)
            } else {
                bytes
            }
        })
        .and_then(check)
}

pub fn mkpath(bytes: &[u8]) -> PathBuf {
    PathBuf::from(ssh_raw_names::from_bytes(bytes).into_owned())
}

fn check(b: Cow<[u8]>) -> Result<Cow<[u8]>, Error> {
    if b.iter().any(|b| *b == 0) {
        Err(Error::new(
            ErrorCode::Session(raw::LIBSSH2_ERROR_INVAL),
            "path provided contains a 0 byte",
        ))
    } else {
        Ok(b)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_name_that_is_not_utf8_comes_back_exactly() {
        // "测试.txt" in GBK, as a server named in that encoding lists it.
        let gbk: &[u8] = b"\xb2\xe2\xca\xd4.txt";
        let path = mkpath(gbk);
        assert_eq!(&*path2bytes(&path).unwrap(), gbk);
        // In a directory, too.
        let nested: &[u8] = b"/home/u/\xb2\xe2\xca\xd4/a";
        assert_eq!(&*path2bytes(&mkpath(nested)).unwrap(), nested);
    }

    #[test]
    fn utf8_names_are_unchanged() {
        let name = "中文/a.txt";
        assert_eq!(mkpath(name.as_bytes()).to_str(), Some(name));
        assert_eq!(&*path2bytes(Path::new(name)).unwrap(), name.as_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn windows_separators_still_become_slashes() {
        assert_eq!(&*path2bytes(Path::new(r"a\b")).unwrap(), b"a/b");
    }
}
