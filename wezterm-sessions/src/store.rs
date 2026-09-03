//! Persistence for the session tree.
//!
//! The tree lives in `$DATA_DIR/sessions.json`, following the same pattern as
//! the emoji recents in `termwindow/charselect.rs`. Unlike that file, losing
//! this one costs the user real work, so writes are atomic and a file we can't
//! parse is quarantined rather than overwritten.

use crate::tree::SessionTree;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

pub fn sessions_file_name() -> PathBuf {
    config::DATA_DIR.join("sessions.json")
}

fn temp_file_name(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn quarantine_file_name(path: &Path) -> PathBuf {
    path.with_extension("json.bad")
}

/// Load the tree, falling back to an empty one.
///
/// This deliberately cannot fail: a missing file is the normal first-run case,
/// and for anything else we would rather show an empty sidebar than refuse to
/// start. A file that exists but doesn't parse is moved aside to
/// `sessions.json.bad` so that the user can recover it by hand.
pub fn load_from(path: &Path) -> SessionTree {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return SessionTree::new();
        }
        Err(err) => {
            log::error!("failed to read {}: {err:#}", path.display());
            return SessionTree::new();
        }
    };

    match serde_json::from_str::<SessionTree>(&data) {
        Ok(mut tree) => {
            tree.reseed_ids();
            tree
        }
        Err(err) => {
            let bad = quarantine_file_name(path);
            log::error!(
                "{} is not valid session data ({err:#}); preserving it as {} and \
                 starting with an empty session tree",
                path.display(),
                bad.display()
            );
            if let Err(err) = std::fs::rename(path, &bad) {
                log::error!(
                    "additionally, failed to move {} aside to {}: {err:#}. \
                     The file has been left alone; the sidebar will not overwrite it.",
                    path.display(),
                    bad.display()
                );
            }
            SessionTree::new()
        }
    }
}

/// Write the tree out atomically: a crash mid-write must not truncate the
/// user's session list.
pub fn save_to(path: &Path, tree: &SessionTree) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(tree)?;
    let tmp = temp_file_name(path);
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The session tree plus the file it came from.
///
/// All mutation goes through [`SessionStore::mutate`], which persists the
/// result, so that no caller can change the tree and forget to save it.
pub struct SessionStore {
    tree: SessionTree,
    path: PathBuf,
    generation: u64,
}

impl SessionStore {
    pub fn load() -> Self {
        let path = sessions_file_name();
        Self {
            tree: load_from(&path),
            path,
            generation: 0,
        }
    }

    #[cfg(test)]
    fn load_at(path: PathBuf) -> Self {
        Self {
            tree: load_from(&path),
            path,
            generation: 0,
        }
    }

    pub fn tree(&self) -> &SessionTree {
        &self.tree
    }

    /// Bumped by every mutation.
    ///
    /// The sidebar's render cache compares this so that edits made from an
    /// overlay -- which have no way to reach into a particular window's
    /// invalidation path -- still force a redraw.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Mutate the tree and persist it.
    ///
    /// A failed write is logged but not propagated: the in-memory change stands
    /// and the next mutation will try again. Interrupting the user with a modal
    /// because their disk is full would not help them.
    pub fn mutate<R>(&mut self, func: impl FnOnce(&mut SessionTree) -> R) -> R {
        let result = func(&mut self.tree);
        self.generation += 1;
        if let Err(err) = save_to(&self.path, &self.tree) {
            log::error!("failed to save {}: {err:#}", self.path.display());
        }
        result
    }
}

thread_local! {
    static STORE: RefCell<Option<SessionStore>> = RefCell::new(None);
}

/// Access the process-wide session store, loading it on first use.
///
/// The tree is shared by every window rather than copied into each one: two
/// windows holding their own copy would silently overwrite each other's edits
/// the next time either of them saved.
///
/// `func` must not call `with_store` again; the borrow is held for its duration.
pub fn with_store<R>(func: impl FnOnce(&mut SessionStore) -> R) -> R {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        func(store.get_or_insert_with(SessionStore::load))
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::tree::{NodeId, SessionNode, SessionSpawn};

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("to make a temp dir")
    }

    fn tree_with(names: &[&str]) -> SessionTree {
        let mut tree = SessionTree::new();
        for name in names {
            let id = tree.alloc_id();
            tree.roots
                .push(SessionNode::session(id, *name, SessionSpawn::default()));
        }
        tree
    }

    #[test]
    fn missing_file_yields_an_empty_tree() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");
        assert!(load_from(&path).is_empty());
        assert!(
            !quarantine_file_name(&path).exists(),
            "a first run must not leave a .bad file behind"
        );
    }

    #[test]
    fn roundtrips() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");
        let tree = tree_with(&["alpha", "beta"]);

        save_to(&path, &tree).expect("to save");
        let loaded = load_from(&path);

        assert_eq!(loaded.roots, tree.roots);
    }

    #[test]
    fn load_reseeds_the_id_allocator() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");
        let tree = tree_with(&["alpha", "beta", "gamma"]);
        save_to(&path, &tree).expect("to save");

        let mut loaded = load_from(&path);
        let next = loaded.alloc_id();
        assert!(
            loaded.find(next).is_none(),
            "a freshly allocated id must not already be in use"
        );
        assert_eq!(next, NodeId(4));
    }

    #[test]
    fn a_corrupt_file_is_preserved_not_overwritten() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");
        std::fs::write(&path, "{ this is not json").expect("to write");

        let tree = load_from(&path);

        assert!(
            tree.is_empty(),
            "we start empty rather than refusing to run"
        );
        let bad = quarantine_file_name(&path);
        assert_eq!(
            std::fs::read_to_string(&bad).expect("the .bad file to exist"),
            "{ this is not json",
            "the user's data must survive verbatim"
        );
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");
        save_to(&path, &tree_with(&["alpha"])).expect("to save");
        assert!(!temp_file_name(&path).exists());
    }

    #[test]
    fn save_overwrites_a_previous_file() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");

        save_to(&path, &tree_with(&["alpha"])).expect("to save");
        save_to(&path, &tree_with(&["beta", "gamma"])).expect("to save again");

        let loaded = load_from(&path);
        assert_eq!(
            loaded
                .roots
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta", "gamma"]
        );
    }

    #[test]
    fn save_creates_the_data_directory() {
        let dir = scratch();
        let path = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("sessions.json");
        save_to(&path, &tree_with(&["alpha"])).expect("to save into a missing directory");
        assert!(path.exists());
    }

    #[test]
    fn mutate_persists() {
        let dir = scratch();
        let path = dir.path().join("sessions.json");

        let mut store = SessionStore::load_at(path.clone());
        store.mutate(|tree| {
            let id = tree.alloc_id();
            tree.roots
                .push(SessionNode::session(id, "added", SessionSpawn::default()));
        });

        assert_eq!(load_from(&path).roots.len(), 1);
    }
}
