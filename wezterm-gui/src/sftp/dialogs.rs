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

/// The window a dialog belongs to: while it is open that window takes no
/// input, so the dialog can't end up behind it, or be opened twice. A plain
/// number so that it can go to the dialog's thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct Owner(pub isize);

/// How a dialog ended.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
enum Outcome {
    Picked,
    Cancelled,
    Failed(String),
}

/// `returned` is what GetOpenFileNameW/GetSaveFileNameW returned and
/// `extended_error` what CommDlgExtendedError said after it: zero for a
/// plain cancel.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn dialog_outcome(returned: i32, extended_error: u32) -> Outcome {
    const FNERR_BUFFERTOOSMALL: u32 = 0x3003;
    match (returned, extended_error) {
        (r, _) if r != 0 => Outcome::Picked,
        (_, 0) => Outcome::Cancelled,
        (_, FNERR_BUFFERTOOSMALL) => Outcome::Failed("选的文件太多，请分几次上传".to_string()),
        (_, code) => Outcome::Failed(format!("文件对话框出错（代码 {code:#x}）")),
    }
}

/// Whether CoInitializeEx's result means this thread holds a reference to
/// release: S_OK and S_FALSE do, a failure such as RPC_E_CHANGED_MODE
/// doesn't.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn com_taken(hresult: i32) -> bool {
    hresult >= 0
}

/// Files picked for upload; empty when the dialog is cancelled, and the
/// reason when it failed.
#[cfg(windows)]
pub async fn pick_open_files(owner: Owner, multi: bool) -> Result<Vec<PathBuf>, String> {
    let picked =
        promise::spawn::spawn_into_new_thread(move || Ok(win::open_files(owner, multi))).await;
    match picked {
        Ok(result) => result,
        Err(err) => {
            log::error!("open file dialog: {err:#}");
            Err(format!("{err:#}"))
        }
    }
}

#[cfg(not(windows))]
pub async fn pick_open_files(_owner: Owner, _multi: bool) -> Result<Vec<PathBuf>, String> {
    Ok(vec![])
}

/// Whether there is a dialog to pick files with.
pub fn can_pick_files() -> bool {
    cfg!(windows)
}

/// Where to save a download; None when the dialog is cancelled.
#[cfg(windows)]
pub async fn pick_save_file(owner: Owner, suggested: &str) -> Option<PathBuf> {
    let suggested = suggested.to_string();
    match promise::spawn::spawn_into_new_thread(move || Ok(win::save_file(owner, &suggested))).await
    {
        Ok(path) => path,
        Err(err) => {
            log::error!("save file dialog: {err:#}");
            None
        }
    }
}

#[cfg(not(windows))]
pub async fn pick_save_file(_owner: Owner, suggested: &str) -> Option<PathBuf> {
    dirs_next::download_dir().map(|dir| dir.join(suggested))
}

#[cfg(windows)]
mod win {
    use super::{com_taken, dialog_outcome, parse_multi_select, Outcome, Owner};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_ALLOWMULTISELECT,
        OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OPENFILENAMEW,
    };

    /// Room for the chosen names: a multi-selection is all of them in one
    /// buffer.
    const BUFFER_CHARS: usize = 32 * 1024;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// COM for the shell's dialog, for as long as this thread needs it;
    /// released only if this thread took it.
    struct Com {
        taken: bool,
    }

    impl Com {
        fn init() -> Self {
            let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
            Com {
                taken: com_taken(hr),
            }
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.taken {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn dialog(
        owner: Owner,
        buf: &mut [u16],
        flags: u32,
        initial_dir: Option<&[u16]>,
    ) -> OPENFILENAMEW {
        let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = owner.0 as _;
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.Flags = flags;
        if let Some(dir) = initial_dir {
            ofn.lpstrInitialDir = dir.as_ptr();
        }
        ofn
    }

    pub fn open_files(owner: Owner, multi: bool) -> Result<Vec<PathBuf>, String> {
        let _com = Com::init();
        let mut buf = vec![0u16; BUFFER_CHARS];
        let mut flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
        if multi {
            flags |= OFN_ALLOWMULTISELECT;
        }
        let mut ofn = dialog(owner, &mut buf, flags, None);
        let returned = unsafe { GetOpenFileNameW(&mut ofn) };
        let extended = if returned == 0 {
            unsafe { CommDlgExtendedError() }
        } else {
            0
        };
        match dialog_outcome(returned, extended) {
            Outcome::Picked => Ok(parse_multi_select(&buf)),
            Outcome::Cancelled => Ok(vec![]),
            Outcome::Failed(why) => Err(why),
        }
    }

    pub fn save_file(owner: Owner, suggested: &str) -> Option<PathBuf> {
        let _com = Com::init();
        let mut buf = vec![0u16; BUFFER_CHARS];
        let name = wide(Path::new(suggested).as_os_str());
        let n = name.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&name[..n]);
        let dir = dirs_next::download_dir().map(|d| wide(d.as_os_str()));
        let mut ofn = dialog(
            owner,
            &mut buf,
            OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR,
            dir.as_deref(),
        );
        let returned = unsafe { GetSaveFileNameW(&mut ofn) };
        let extended = if returned == 0 {
            unsafe { CommDlgExtendedError() }
        } else {
            0
        };
        match dialog_outcome(returned, extended) {
            Outcome::Picked => parse_multi_select(&buf).into_iter().next(),
            Outcome::Cancelled => None,
            Outcome::Failed(why) => {
                log::error!("save file dialog: {why}");
                None
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn a_dialog_that_returns_nothing_was_cancelled_unless_it_says_otherwise() {
        assert_eq!(dialog_outcome(1, 0), Outcome::Picked);
        assert_eq!(dialog_outcome(0, 0), Outcome::Cancelled);
        // FNERR_BUFFERTOOSMALL: more files picked than the buffer holds.
        assert_eq!(
            dialog_outcome(0, 0x3003),
            Outcome::Failed("选的文件太多，请分几次上传".to_string())
        );
        assert_eq!(
            dialog_outcome(0, 0x0002),
            Outcome::Failed("文件对话框出错（代码 0x2）".to_string())
        );
    }

    #[test]
    fn com_is_released_only_if_this_thread_took_it() {
        assert!(com_taken(0)); // S_OK
        assert!(com_taken(1)); // S_FALSE: already initialised the same way
        assert!(!com_taken(0x80010106u32 as i32)); // RPC_E_CHANGED_MODE
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
