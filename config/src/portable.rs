//! A portable install keeps its data next to the executable.
//!
//! On Windows a `wezterm.lua` beside `wezterm.exe` already means "run from
//! this directory, not from the user's profile" for the config file. This
//! extends the same meaning to the files wezterm writes for itself, so that
//! the whole install can be carried around or backed up as one directory.

// Only the Windows build calls into this module; the tests exercise it
// everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Written once the legacy data has been copied, so that deleting a file
/// from the portable data dir later is not undone by copying it back.
pub const MIGRATED_MARKER: &str = ".migrated";

/// The data dir a portable install keeps next to its executable, or None
/// when this is not a portable install or that dir cannot be written.
///
/// Only the presence of `wezterm.lua` counts, not which config file this
/// process ends up loading: `DATA_DIR` can be needed before the config has
/// been read at all.
pub fn portable_data_dir(exe_dir: &Path) -> Option<PathBuf> {
    if !exe_dir.join("wezterm.lua").is_file() {
        return None;
    }

    let data = exe_dir.join("data");
    if let Err(err) = std::fs::create_dir_all(&data) {
        log::warn!(
            "portable install, but {} cannot be created ({err:#}); \
             using the per-user data dir instead",
            data.display()
        );
        return None;
    }
    // An install under Program Files has the directory but no right to
    // write in it, and create_dir_all does not notice that.
    if let Err(err) = probe_writable(&data) {
        log::warn!(
            "portable install, but {} cannot be written ({err:#}); \
             using the per-user data dir instead",
            data.display()
        );
        return None;
    }

    Some(data)
}

/// The directories that may hold the portable install this process belongs
/// to, most specific first.
///
/// The executable's own directory, and failing that the directory of the
/// config file named by WEZTERM_CONFIG_FILE when that is an install
/// directory. The second case is a program started from a pane of a
/// portable install -- a development build run with `cargo run`, say. It
/// inherits WEZTERM_CONFIG_FILE, loads that install's config, and through
/// its serve_command talks to that install's mux server, which reads the
/// same sessions.json. Keeping its data anywhere else would split the
/// session tree between the two.
pub fn candidate_install_dirs(exe_dir: Option<&Path>, config_file: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(exe_dir) = exe_dir {
        dirs.push(exe_dir.to_path_buf());
    }

    if let Some(config_file) = config_file {
        if config_file.file_name() == Some(std::ffi::OsStr::new("wezterm.lua")) {
            if let Some(config_dir) = config_file.parent() {
                let is_install_dir = config_dir.join("wezterm-gui.exe").is_file()
                    || config_dir.join("wezterm-mux-server.exe").is_file();
                if is_install_dir && !dirs.iter().any(|dir| dir == config_dir) {
                    dirs.push(config_dir.to_path_buf());
                }
            }
        }
    }

    dirs
}

fn probe_writable(dir: &Path) -> std::io::Result<()> {
    use std::io::Write;

    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    // A previous process with the same pid may have crashed before it could
    // remove its own probe. Clear it first -- ignoring the error, since the
    // common case is that there is nothing there -- so that a reused pid
    // does not make create_new below fail with AlreadyExists and read as
    // "not writable".
    let _ = std::fs::remove_file(&probe);

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let written = file.write_all(b"x");
    drop(file);
    written?;

    // Whether we can clean up after ourselves is not what this probe is
    // asking; a leftover empty file does not stop the directory from being
    // usable, so log and move on rather than reporting "not writable".
    if let Err(err) = std::fs::remove_file(&probe) {
        log::warn!("could not remove write probe {}: {err:#}", probe.display());
    }
    Ok(())
}

/// Where the data a portable install should start from lives today, paired
/// with the name it gets inside the portable data dir.
///
/// The hook token is the odd one out: the Feishu service has always kept it
/// in the home directory rather than in wezterm's data dir.
pub fn legacy_sources(legacy_data_dir: &Path, home_dir: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut sources: Vec<(PathBuf, &'static str)> = [
        "sessions.json",
        "bridge-token",
        "recent-commands.json",
        "recent-emoji.json",
        "repl-history",
    ]
    .iter()
    .map(|name| (legacy_data_dir.join(name), *name))
    .collect();
    sources.push((
        home_dir.join(".wezterm-bridge-hook-token"),
        "bridge-hook-token",
    ));
    sources
}

/// Copy whatever the portable data dir does not have yet, the first time
/// it is used.
///
/// Copied, not moved: the tokens keep their values, so anything still
/// reading the old paths goes on working. A failed copy is logged and
/// otherwise ignored -- a missing recent-commands list is no reason not to
/// start -- but it also holds back the `.migrated` marker, so the next
/// start tries that file again instead of accepting the miss forever.
/// Retrying is safe because placement never replaces an existing file.
pub fn migrate_once(data_dir: &Path, sources: &[(PathBuf, &'static str)]) {
    let marker = data_dir.join(MIGRATED_MARKER);
    if marker.exists() {
        return;
    }

    let mut all_copied = true;
    for (source, name) in sources {
        let dest = data_dir.join(name);
        if dest.exists() || !source.is_file() {
            continue;
        }
        match copy_without_replacing(source, data_dir, name) {
            Ok(()) => log::info!("copied {} to {}", source.display(), dest.display()),
            Err(err) => {
                all_copied = false;
                log::warn!(
                    "could not copy {} to {}: {err:#}",
                    source.display(),
                    dest.display()
                )
            }
        }
    }

    if !all_copied {
        log::warn!(
            "not all files could be migrated to {}; will retry on the next start",
            data_dir.display()
        );
        return;
    }

    if let Err(err) = std::fs::write(&marker, b"") {
        log::warn!("could not write {}: {err:#}", marker.display());
    }
}

/// Put a copy of `source` at `data_dir/name` unless something is already
/// there.
///
/// The GUI and the mux server can both reach this at the same moment. A
/// file written in place could be read half-finished by the other one, and
/// a half-finished sessions.json fails to parse and gets renamed aside as
/// `.bad`. So the copy goes to a temporary name first and is then linked
/// into place, which either succeeds whole or finds the name taken; rename
/// would instead silently replace a file the other process may already
/// have changed.
fn copy_without_replacing(source: &Path, data_dir: &Path, name: &str) -> std::io::Result<()> {
    let dest = data_dir.join(name);
    let tmp = data_dir.join(format!("{name}.migrate-{}", std::process::id()));
    std::fs::copy(source, &tmp)?;

    let placed = match std::fs::hard_link(&tmp, &dest) {
        Ok(()) => Ok(()),
        // Someone else got there first, with the same content.
        Err(_) if dest.exists() => Ok(()),
        // FAT and exFAT, where a thumb drive install is likely to live, have
        // no hard links.
        Err(_) => std::fs::rename(&tmp, &dest),
    };
    // Gone already when rename was used; that error means nothing.
    let _ = std::fs::remove_file(&tmp);
    placed
}

/// The portable data dir for the running executable, migrated and ready,
/// or None to keep using `legacy_data_dir`.
#[cfg(windows)]
pub fn resolve(legacy_data_dir: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let config_file = std::env::var_os("WEZTERM_CONFIG_FILE").map(PathBuf::from);
    let data = candidate_install_dirs(
        exe.as_deref().and_then(Path::parent),
        config_file.as_deref(),
    )
    .iter()
    .find_map(|dir| portable_data_dir(dir))?;
    migrate_once(&data, &legacy_sources(legacy_data_dir, &crate::HOME_DIR));
    log::info!("portable install: data dir is {}", data.display());
    Some(data)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn not_portable_without_a_config_next_to_the_exe() {
        let exe_dir = tempfile::tempdir().unwrap();
        assert_eq!(portable_data_dir(exe_dir.path()), None);
        assert!(
            !exe_dir.path().join("data").exists(),
            "deciding must not leave a data dir behind in an ordinary install"
        );
    }

    #[test]
    fn portable_with_a_config_next_to_the_exe() {
        let exe_dir = tempfile::tempdir().unwrap();
        fs::write(exe_dir.path().join("wezterm.lua"), "return {}").unwrap();

        let data = exe_dir.path().join("data");
        assert_eq!(portable_data_dir(exe_dir.path()), Some(data.clone()));
        assert!(data.is_dir());
        assert!(
            names_in(&data).is_empty(),
            "the write probe must clean up after itself"
        );
    }

    #[test]
    fn not_portable_when_the_data_dir_cannot_be_made() {
        let exe_dir = tempfile::tempdir().unwrap();
        fs::write(exe_dir.path().join("wezterm.lua"), "return {}").unwrap();
        // A plain file where the directory should go.
        fs::write(exe_dir.path().join("data"), "").unwrap();

        assert_eq!(portable_data_dir(exe_dir.path()), None);
    }

    #[test]
    fn legacy_sources_name_every_file_worth_keeping() {
        let legacy = Path::new("legacy");
        let home = Path::new("home");
        assert_eq!(
            legacy_sources(legacy, home),
            vec![
                (legacy.join("sessions.json"), "sessions.json"),
                (legacy.join("bridge-token"), "bridge-token"),
                (legacy.join("recent-commands.json"), "recent-commands.json"),
                (legacy.join("recent-emoji.json"), "recent-emoji.json"),
                (legacy.join("repl-history"), "repl-history"),
                (home.join(".wezterm-bridge-hook-token"), "bridge-hook-token"),
            ]
        );
    }

    #[test]
    fn migration_copies_what_is_missing_and_keeps_the_source() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = legacy.path().join("sessions.json");
        fs::write(&source, "{\"roots\":[]}").unwrap();

        migrate_once(data.path(), &[(source.clone(), "sessions.json")]);

        assert_eq!(
            fs::read_to_string(data.path().join("sessions.json")).unwrap(),
            "{\"roots\":[]}"
        );
        assert!(source.exists(), "the legacy copy stays as a backup");
    }

    #[test]
    fn migration_never_replaces_an_existing_file() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = legacy.path().join("bridge-token");
        fs::write(&source, "old").unwrap();
        fs::write(data.path().join("bridge-token"), "current").unwrap();

        migrate_once(data.path(), &[(source, "bridge-token")]);

        assert_eq!(
            fs::read_to_string(data.path().join("bridge-token")).unwrap(),
            "current"
        );
    }

    #[test]
    fn migration_skips_sources_that_do_not_exist() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();

        migrate_once(
            data.path(),
            &[(legacy.path().join("repl-history"), "repl-history")],
        );

        assert_eq!(names_in(data.path()), vec![MIGRATED_MARKER.to_string()]);
    }

    #[test]
    fn a_failed_copy_is_retried_next_time() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = legacy.path().join("sessions.json");
        fs::write(&source, "{\"roots\":[]}").unwrap();
        let sources = [(source, "sessions.json")];

        // A directory sitting at the tmp path copy_without_replacing wants to
        // use makes std::fs::copy into it fail, standing in for whatever
        // makes a real copy fail (disk full, permissions, a vanished source).
        let blocker = data
            .path()
            .join(format!("sessions.json.migrate-{}", std::process::id()));
        fs::create_dir_all(&blocker).unwrap();

        migrate_once(data.path(), &sources);
        assert!(
            !data.path().join(MIGRATED_MARKER).exists(),
            "a failed copy must not be recorded as migrated"
        );
        assert!(!data.path().join("sessions.json").exists());

        fs::remove_dir(&blocker).unwrap();
        migrate_once(data.path(), &sources);
        assert_eq!(
            fs::read_to_string(data.path().join("sessions.json")).unwrap(),
            "{\"roots\":[]}",
            "retried once the obstruction is gone"
        );
        assert!(data.path().join(MIGRATED_MARKER).exists());
    }

    #[test]
    fn copy_without_replacing_keeps_a_file_that_appeared_meanwhile() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = legacy.path().join("bridge-token");
        fs::write(&source, "ours").unwrap();
        fs::write(data.path().join("bridge-token"), "theirs").unwrap();

        let result = copy_without_replacing(&source, data.path(), "bridge-token");

        assert!(result.is_ok());
        assert_eq!(
            fs::read_to_string(data.path().join("bridge-token")).unwrap(),
            "theirs",
            "whoever placed it first wins"
        );
        assert_eq!(
            names_in(data.path()),
            vec!["bridge-token".to_string()],
            "no *.migrate-* leftovers"
        );
    }

    #[test]
    fn migration_happens_once() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let source = legacy.path().join("sessions.json");
        fs::write(&source, "{}").unwrap();
        let sources = [(source, "sessions.json")];

        migrate_once(data.path(), &sources);
        fs::remove_file(data.path().join("sessions.json")).unwrap();
        migrate_once(data.path(), &sources);

        assert!(
            !data.path().join("sessions.json").exists(),
            "a file the user removed after migrating must not come back"
        );
    }

    #[test]
    fn a_stale_probe_does_not_block_detection() {
        let exe_dir = tempfile::tempdir().unwrap();
        fs::write(exe_dir.path().join("wezterm.lua"), "return {}").unwrap();
        let data = exe_dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let stale_probe = data.join(format!(".write-probe-{}", std::process::id()));
        fs::write(&stale_probe, "leftover from a crashed process").unwrap();

        assert_eq!(portable_data_dir(exe_dir.path()), Some(data.clone()));
        assert!(
            !stale_probe.exists(),
            "a stale probe with our own reused pid must not block detection, \
             and must not survive it either"
        );
    }

    #[test]
    fn the_executable_dir_comes_first() {
        let exe_dir = tempfile::tempdir().unwrap();
        assert_eq!(
            candidate_install_dirs(Some(exe_dir.path()), None),
            vec![exe_dir.path().to_path_buf()]
        );
    }

    #[test]
    fn a_config_file_in_another_install_dir_is_a_candidate() {
        let exe_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        fs::write(config_dir.path().join("wezterm.lua"), "return {}").unwrap();
        fs::write(config_dir.path().join("wezterm-gui.exe"), "").unwrap();
        let config_file = config_dir.path().join("wezterm.lua");

        assert_eq!(
            candidate_install_dirs(Some(exe_dir.path()), Some(&config_file)),
            vec![
                exe_dir.path().to_path_buf(),
                config_dir.path().to_path_buf()
            ]
        );
    }

    #[test]
    fn a_mux_server_alone_marks_an_install_dir() {
        let exe_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        fs::write(config_dir.path().join("wezterm.lua"), "return {}").unwrap();
        fs::write(config_dir.path().join("wezterm-mux-server.exe"), "").unwrap();
        let config_file = config_dir.path().join("wezterm.lua");

        assert_eq!(
            candidate_install_dirs(Some(exe_dir.path()), Some(&config_file)),
            vec![
                exe_dir.path().to_path_buf(),
                config_dir.path().to_path_buf()
            ]
        );
    }

    #[test]
    fn a_config_file_outside_an_install_dir_is_not_a_candidate() {
        let exe_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        fs::write(config_dir.path().join("wezterm.lua"), "return {}").unwrap();
        let config_file = config_dir.path().join("wezterm.lua");

        assert_eq!(
            candidate_install_dirs(Some(exe_dir.path()), Some(&config_file)),
            vec![exe_dir.path().to_path_buf()]
        );
    }

    #[test]
    fn only_a_file_named_wezterm_lua_counts() {
        let exe_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        fs::write(config_dir.path().join("wezterm-gui.exe"), "").unwrap();
        let config_file = config_dir.path().join("other.lua");

        assert_eq!(
            candidate_install_dirs(Some(exe_dir.path()), Some(&config_file)),
            vec![exe_dir.path().to_path_buf()]
        );
    }

    #[test]
    fn the_same_dir_is_not_listed_twice() {
        let exe_dir = tempfile::tempdir().unwrap();
        fs::write(exe_dir.path().join("wezterm.lua"), "return {}").unwrap();
        fs::write(exe_dir.path().join("wezterm-gui.exe"), "").unwrap();
        let config_file = exe_dir.path().join("wezterm.lua");

        assert_eq!(
            candidate_install_dirs(Some(exe_dir.path()), Some(&config_file)),
            vec![exe_dir.path().to_path_buf()]
        );
    }

    #[test]
    fn migration_leaves_no_temporary_files() {
        let legacy = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let a = legacy.path().join("sessions.json");
        let b = legacy.path().join("bridge-token");
        fs::write(&a, "{}").unwrap();
        fs::write(&b, "t").unwrap();

        migrate_once(data.path(), &[(a, "sessions.json"), (b, "bridge-token")]);

        assert_eq!(
            names_in(data.path()),
            vec![
                MIGRATED_MARKER.to_string(),
                "bridge-token".to_string(),
                "sessions.json".to_string(),
            ]
        );
    }
}
