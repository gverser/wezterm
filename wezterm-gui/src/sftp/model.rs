//! The file page's pure logic: ordering a listing, remote path arithmetic,
//! and when following the terminal should move the page.

use crate::sessions::monitor_proto::human_bytes;
use std::borrow::Cow;
use termwiz::cell::unicode_column_width;
use wezterm_ssh::raw_names;

/// A name as the user should see it. Names keep any bytes that aren't UTF-8
/// (carried as in `raw_names`), so that they can be opened; those are read
/// here as GBK, which is what Chinese names that aren't UTF-8 nearly always
/// are, and failing that whatever can't be read shows as �.
pub fn display_name(name: &str) -> Cow<'_, str> {
    if !raw_names::has_raw_bytes(name) {
        return Cow::Borrowed(name);
    }
    let bytes = raw_names::to_bytes(name);
    match encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(&bytes) {
        Some(text) => Cow::Owned(text.into_owned()),
        None => Cow::Owned(String::from_utf8_lossy(&bytes).into_owned()),
    }
}

/// A path as the user should see it: each part read on its own, since one
/// directory's name being GBK says nothing about the others.
pub fn display_path(path: &str) -> String {
    if !raw_names::has_raw_bytes(path) {
        return path.to_string();
    }
    path.split('/')
        .map(display_name)
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    /// A symbolic link; `is_dir` is false for one, whatever it points at.
    pub is_link: bool,
    /// None for directories.
    pub size: Option<u64>,
}

/// Directories first, then each group by name ignoring case; names that
/// differ only in case keep a fixed order between them.
pub fn sort_entries(v: &mut Vec<FileEntry>) {
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// "/a/b" → "/a", "/a" → "/", "/" → "/".
pub fn remote_parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// Said when files are dropped on the page before it has read a directory.
pub const NO_DIR_YET: &str = "目录还没读出来，这次没有上传，请稍后再拖一次";

/// The directory dropped files go to: the one the page shows, once it has
/// one.
pub fn upload_dir(cwd: Option<&str>) -> Result<&str, &'static str> {
    match cwd {
        Some(dir) if !dir.is_empty() => Ok(dir),
        _ => Err(NO_DIR_YET),
    }
}

/// `dir` and `name` joined by exactly one slash.
pub fn remote_join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// Where the page should go to follow the terminal, if anywhere.
///
/// Only when following is on, the terminal's directory is known, and it is
/// not the one last followed to: browsing elsewhere by hand is left alone
/// until the terminal moves again.
pub fn follow_target(
    follow: bool,
    reported: Option<&str>,
    last_followed: Option<&str>,
) -> Option<String> {
    match reported {
        Some(dir) if follow && Some(dir) != last_followed => Some(dir.to_string()),
        _ => None,
    }
}

/// Why `name` can't be a new file or folder name, if it can't.
pub fn check_file_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        Some("名称不能为空")
    } else if name.contains('/') {
        Some("名称不能包含 /")
    } else if name == "." || name == ".." {
        Some("名称无效")
    } else {
        None
    }
}

/// The directory typed into the path bar: absolute, relative to `cwd`, or
/// under `~`. None when there is nothing to go to.
pub fn resolve_path_input(input: &str, cwd: &str, home: Option<&str>) -> Option<String> {
    let input = input.trim();
    let path = if input.is_empty() {
        return None;
    } else if input == "~" {
        home?.to_string()
    } else if let Some(rest) = input.strip_prefix("~/") {
        remote_join(home?, rest)
    } else if input.starts_with('/') {
        input.to_string()
    } else {
        remote_join(cwd, input)
    };
    let trimmed = path.trim_end_matches('/');
    Some(if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    })
}

fn char_cells(c: char) -> usize {
    let mut buf = [0u8; 4];
    unicode_column_width(c.encode_utf8(&mut buf), None)
}

/// `text` cut down to `max_cells` columns by dropping its start, which is
/// marked with "…". For paths, where the end is what tells them apart.
pub fn elide_left(text: &str, max_cells: usize) -> String {
    if max_cells == 0 {
        return String::new();
    }
    if unicode_column_width(text, None) <= max_cells {
        return text.to_string();
    }
    let budget = max_cells - 1;
    let mut used = 0;
    let mut kept: Vec<char> = vec![];
    for c in text.chars().rev() {
        let w = char_cells(c);
        if used + w > budget {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.reverse();
    format!("…{}", kept.into_iter().collect::<String>())
}

/// `text` cut down to `max_cells` columns by dropping its end, which is
/// marked with "…". For file names, so that they don't run into the size.
pub fn elide_right(text: &str, max_cells: usize) -> String {
    if max_cells == 0 {
        return String::new();
    }
    if unicode_column_width(text, None) <= max_cells {
        return text.to_string();
    }
    let budget = max_cells - 1;
    let mut used = 0;
    let mut kept = String::new();
    for c in text.chars() {
        let w = char_cells(c);
        if used + w > budget {
            break;
        }
        used += w;
        kept.push(c);
    }
    format!("{kept}…")
}

/// `text` broken into lines of at most `cells` columns. The sidebar draws
/// one line per row and clips what doesn't fit, which would cut an error
/// message off mid-sentence.
pub fn wrap_cells(text: &str, cells: usize) -> Vec<String> {
    if cells == 0 {
        return vec![text.to_string()];
    }
    let mut lines = vec![];
    let mut line = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_cells(c);
        if used + w > cells && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push(c);
        used += w;
    }
    lines.push(line);
    lines
}

/// An sftp failure as shown on the page: the libssh backend reports only the
/// protocol's status number, so say what the common ones mean, and keep the
/// original after it.
pub fn explain_sftp_error(text: &str) -> String {
    let code = text
        .rsplit("Sftp error code ")
        .next()
        .filter(|_| text.contains("Sftp error code "))
        .and_then(|rest| rest.trim().parse::<u32>().ok());
    // SSH_FX_* from the sftp protocol draft that OpenSSH implements.
    let meaning = match code {
        Some(1) => "已到末尾",
        Some(2) => "不存在",
        Some(3) => "权限不足",
        Some(4) => "操作失败",
        Some(5) => "请求无效",
        Some(6) | Some(7) => "连接已断开",
        Some(8) => "服务器不支持该操作",
        _ => return text.to_string(),
    };
    format!("{meaning}（{text}）")
}

/// A file's size for the list; nothing for directories.
pub fn display_size(size: Option<u64>) -> String {
    size.map(human_bytes).unwrap_or_default()
}

#[cfg(test)]
mod test {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            is_dir,
            is_link: false,
            size: if is_dir { None } else { Some(1) },
        }
    }

    /// A name as it comes from the server: "测试" in GBK.
    fn gbk_name() -> String {
        wezterm_ssh::raw_names::from_bytes(b"\xb2\xe2\xca\xd4").into_owned()
    }

    #[test]
    fn names_that_are_not_utf8_show_as_gbk() {
        assert_eq!(display_name(&gbk_name()), "测试");
        assert!(matches!(
            display_name("报告.txt"),
            Cow::Borrowed("报告.txt")
        ));
    }

    #[test]
    fn names_that_are_not_gbk_either_show_what_can_be_read() {
        // 0xff is never part of GBK.
        let name = wezterm_ssh::raw_names::from_bytes(b"a\xffb").into_owned();
        assert_eq!(display_name(&name), "a\u{fffd}b");
    }

    #[test]
    fn a_path_reads_each_part_on_its_own() {
        let path = format!("/srv/报告/{}/a.txt", gbk_name());
        assert_eq!(display_path(&path), "/srv/报告/测试/a.txt");
        assert_eq!(display_path("/srv/a"), "/srv/a");
    }

    #[test]
    fn files_dropped_before_the_page_has_a_directory_are_turned_away_aloud() {
        assert_eq!(upload_dir(Some("/srv")), Ok("/srv"));
        assert_eq!(upload_dir(Some("")), Err(NO_DIR_YET));
        assert_eq!(upload_dir(None), Err(NO_DIR_YET));
        assert!(NO_DIR_YET.contains("没有上传"), "{}", NO_DIR_YET);
    }

    #[test]
    fn dirs_first_then_name_case_insensitive() {
        let mut v = vec![
            entry("b.txt", false),
            entry("Zeta", true),
            entry("A.txt", false),
            entry("alpha", true),
            entry("a.txt", false),
            entry(".hidden", false),
        ];
        sort_entries(&mut v);
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "Zeta", ".hidden", "A.txt", "a.txt", "b.txt"]
        );
    }

    #[test]
    fn parent_of_root_is_root() {
        assert_eq!(remote_parent("/"), "/");
        assert_eq!(remote_parent("/a"), "/");
        assert_eq!(remote_parent("/a/b"), "/a");
        assert_eq!(remote_parent("/a/b/"), "/a");
    }

    #[test]
    fn join_avoids_double_slash() {
        assert_eq!(remote_join("/", "etc"), "/etc");
        assert_eq!(remote_join("/var", "log"), "/var/log");
        assert_eq!(remote_join("/var/", "log"), "/var/log");
    }

    #[test]
    fn follow_only_when_changed() {
        assert_eq!(
            follow_target(true, Some("/etc"), None),
            Some("/etc".to_string())
        );
        assert_eq!(
            follow_target(true, Some("/tmp"), Some("/etc")),
            Some("/tmp".to_string())
        );
        assert_eq!(follow_target(true, Some("/etc"), Some("/etc")), None);
    }

    #[test]
    fn follow_off_never_jumps() {
        assert_eq!(follow_target(false, Some("/etc"), None), None);
        assert_eq!(follow_target(false, Some("/tmp"), Some("/etc")), None);
    }

    #[test]
    fn unknown_cwd_never_jumps() {
        assert_eq!(follow_target(true, None, None), None);
        assert_eq!(follow_target(true, None, Some("/etc")), None);
    }

    #[test]
    fn file_name_rules() {
        assert_eq!(check_file_name("logs"), None);
        assert_eq!(check_file_name("  "), Some("名称不能为空"));
        assert_eq!(check_file_name("a/b"), Some("名称不能包含 /"));
        assert_eq!(check_file_name(".."), Some("名称无效"));
        assert_eq!(check_file_name("."), Some("名称无效"));
    }

    #[test]
    fn path_input_resolves_relative_and_home() {
        let home = Some("/home/ty");
        assert_eq!(
            resolve_path_input("/etc/", "/var", home),
            Some("/etc".to_string())
        );
        assert_eq!(resolve_path_input("/", "/var", home), Some("/".to_string()));
        assert_eq!(
            resolve_path_input("log", "/var", home),
            Some("/var/log".to_string())
        );
        assert_eq!(
            resolve_path_input("~", "/var", home),
            Some("/home/ty".to_string())
        );
        assert_eq!(
            resolve_path_input("~/src", "/var", home),
            Some("/home/ty/src".to_string())
        );
        assert_eq!(resolve_path_input("~/src", "/var", None), None);
        assert_eq!(resolve_path_input("  ", "/var", home), None);
    }

    #[test]
    fn long_paths_lose_their_start() {
        assert_eq!(elide_left("/var/log", 20), "/var/log");
        assert_eq!(elide_left("/var/log/nginx/access", 10), "…nx/access");
        assert_eq!(elide_left("/数据/日志", 5), "…日志");
        assert_eq!(elide_left("/var", 0), "");
    }

    #[test]
    fn long_names_lose_their_end() {
        assert_eq!(elide_right("short", 10), "short");
        assert_eq!(elide_right("wezterm-create-existing", 10), "wezterm-c…");
        assert_eq!(elide_right("目录不为空", 5), "目录…");
        assert_eq!(elide_right("abc", 0), "");
    }

    #[test]
    fn wrap_breaks_by_display_width() {
        assert_eq!(wrap_cells("short", 10), vec!["short"]);
        assert_eq!(wrap_cells("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_cells("目录不为空", 4), vec!["目录", "不为", "空"]);
        assert_eq!(wrap_cells("", 4), vec![""]);
        assert_eq!(wrap_cells("abc", 0), vec!["abc"]);
    }

    #[test]
    fn sftp_codes_get_a_meaning() {
        let raw = "Library-specific error: SftpError: Sftp error code 2";
        assert_eq!(explain_sftp_error(raw), format!("不存在（{raw}）"));
        assert_eq!(
            explain_sftp_error("Sftp error code 3"),
            "权限不足（Sftp error code 3）"
        );
        assert_eq!(explain_sftp_error("something else"), "something else");
        assert_eq!(
            explain_sftp_error("Sftp error code 99"),
            "Sftp error code 99"
        );
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(display_size(None), "");
        assert_eq!(display_size(Some(512)), "512B");
        assert_eq!(display_size(Some(2202009)), "2.1M");
    }
}
