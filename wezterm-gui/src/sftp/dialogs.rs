//! The system's open and save dialogs, for picking files to upload and where
//! to put a download.
//!
//! Each dialog runs modally on a thread of its own, so the window keeps
//! painting while it is open.

use std::path::PathBuf;

/// The paths in a GetOpenFileNameW buffer: one full path, or with
/// OFN_ALLOWMULTISELECT and several files chosen, the directory followed by
/// each name; NUL separated, ending with two NULs.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn parse_multi_select(buf: &[u16]) -> Vec<PathBuf> {
    let mut parts = vec![];
    for part in buf.split(|&c| c == 0) {
        if part.is_empty() {
            break;
        }
        parts.push(String::from_utf16_lossy(part));
    }
    match parts.len() {
        0 => vec![],
        1 => vec![PathBuf::from(&parts[0])],
        _ => {
            let dir = PathBuf::from(&parts[0]);
            parts[1..].iter().map(|name| dir.join(name)).collect()
        }
    }
}

/// Files picked for upload; empty when the dialog is cancelled.
#[cfg(windows)]
pub async fn pick_open_files(multi: bool) -> Vec<PathBuf> {
    let picked = promise::spawn::spawn_into_new_thread(move || Ok(win::open_files(multi))).await;
    match picked {
        Ok(paths) => paths,
        Err(err) => {
            log::error!("open file dialog: {err:#}");
            vec![]
        }
    }
}

#[cfg(not(windows))]
pub async fn pick_open_files(_multi: bool) -> Vec<PathBuf> {
    vec![]
}

/// Whether there is a dialog to pick files with.
pub fn can_pick_files() -> bool {
    cfg!(windows)
}

/// Where to save a download; None when the dialog is cancelled.
#[cfg(windows)]
pub async fn pick_save_file(suggested: &str) -> Option<PathBuf> {
    let suggested = suggested.to_string();
    match promise::spawn::spawn_into_new_thread(move || Ok(win::save_file(&suggested))).await {
        Ok(path) => path,
        Err(err) => {
            log::error!("save file dialog: {err:#}");
            None
        }
    }
}

#[cfg(not(windows))]
pub async fn pick_save_file(suggested: &str) -> Option<PathBuf> {
    dirs_next::download_dir().map(|dir| dir.join(suggested))
}

#[cfg(windows)]
mod win {
    use super::parse_multi_select;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST,
        OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OPENFILENAMEW,
    };

    /// Room for the chosen names: a multi-selection is all of them in one
    /// buffer.
    const BUFFER_CHARS: usize = 32 * 1024;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// COM for the shell's dialog, for as long as this thread needs it.
    struct Com;

    impl Com {
        fn init() -> Self {
            unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
            Com
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    fn dialog(buf: &mut [u16], flags: u32, initial_dir: Option<&[u16]>) -> OPENFILENAMEW {
        let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = std::ptr::null_mut();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.Flags = flags;
        if let Some(dir) = initial_dir {
            ofn.lpstrInitialDir = dir.as_ptr();
        }
        ofn
    }

    pub fn open_files(multi: bool) -> Vec<PathBuf> {
        let _com = Com::init();
        let mut buf = vec![0u16; BUFFER_CHARS];
        let mut flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
        if multi {
            flags |= OFN_ALLOWMULTISELECT;
        }
        let mut ofn = dialog(&mut buf, flags, None);
        if unsafe { GetOpenFileNameW(&mut ofn) } == 0 {
            return vec![];
        }
        parse_multi_select(&buf)
    }

    pub fn save_file(suggested: &str) -> Option<PathBuf> {
        let _com = Com::init();
        let mut buf = vec![0u16; BUFFER_CHARS];
        let name = wide(Path::new(suggested).as_os_str());
        let n = name.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&name[..n]);
        let dir = dirs_next::download_dir().map(|d| wide(d.as_os_str()));
        let mut ofn = dialog(
            &mut buf,
            OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR,
            dir.as_deref(),
        );
        if unsafe { GetSaveFileNameW(&mut ofn) } == 0 {
            return None;
        }
        parse_multi_select(&buf).into_iter().next()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn parse_multi_select_single_and_multi() {
        // One file: the buffer is its full path.
        assert_eq!(
            parse_multi_select(&wide("C:\\data\\a.txt\0\0")),
            vec![PathBuf::from("C:\\data\\a.txt")]
        );
        // Several: the directory, then each name.
        assert_eq!(
            parse_multi_select(&wide("C:\\data\0a.txt\0b 文件.txt\0\0garbage")),
            vec![
                PathBuf::from("C:\\data\\a.txt"),
                PathBuf::from("C:\\data\\b 文件.txt")
            ]
        );
        // Nothing chosen.
        assert_eq!(parse_multi_select(&wide("\0\0")), Vec::<PathBuf>::new());
    }
}
