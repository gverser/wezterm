//! SSH connection details for session nodes, kept in `$DATA_DIR/ssh_hosts.json`.
//!
//! They live in a file of their own, keyed by [`NodeId`], rather than inside
//! the nodes in `sessions.json`: an older build rewrites the whole tree on its
//! first edit and would silently drop any field it doesn't know. Here it never
//! touches this file, so rolling back loses nothing.
//!
//! Passwords are stored in the clear on purpose: the user wants the data
//! directory to be portable between machines, and asked for no master
//! password.

use crate::store::{quarantine_file_name, temp_file_name};
use crate::tree::NodeId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SshAuth {
    #[default]
    Password,
    /// An OpenSSH-format private key file.
    Key,
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshHost {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Empty means "let wezterm-ssh decide", as plain `ssh host` would.
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub auth: SshAuth,
    /// Empty means no password has been saved.
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub key_path: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SshHosts {
    #[serde(
        default,
        serialize_with = "serialize_hosts",
        deserialize_with = "deserialize_hosts"
    )]
    pub hosts: BTreeMap<NodeId, SshHost>,
}

/// JSON object keys must be strings, so write node ids as `"12"`.
fn serialize_hosts<S: Serializer>(
    hosts: &BTreeMap<NodeId, SshHost>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let by_string: BTreeMap<String, &SshHost> =
        hosts.iter().map(|(id, h)| (id.0.to_string(), h)).collect();
    by_string.serialize(serializer)
}

fn deserialize_hosts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<NodeId, SshHost>, D::Error> {
    let by_string = BTreeMap::<String, SshHost>::deserialize(deserializer)?;
    by_string
        .into_iter()
        .map(|(key, host)| {
            key.parse::<u64>()
                .map(|id| (NodeId(id), host))
                .map_err(|_| serde::de::Error::custom(format!("bad node id {key:?}")))
        })
        .collect()
}

pub fn ssh_hosts_file_name() -> PathBuf {
    config::DATA_DIR.join("ssh_hosts.json")
}

/// Load the hosts, falling back to none. Mirrors `store::load_from`: a missing
/// file is normal, and one that won't parse is moved aside to `.json.bad`.
pub fn load_ssh_hosts_from(path: &Path) -> SshHosts {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return SshHosts::default();
        }
        Err(err) => {
            log::error!("failed to read {}: {err:#}", path.display());
            return SshHosts::default();
        }
    };

    match serde_json::from_str::<SshHosts>(&data) {
        Ok(hosts) => hosts,
        Err(err) => {
            let bad = quarantine_file_name(path);
            log::error!(
                "{} is not valid ssh host data ({err:#}); preserving it as {} and \
                 starting with no saved ssh hosts",
                path.display(),
                bad.display()
            );
            if let Err(err) = std::fs::rename(path, &bad) {
                log::error!(
                    "additionally, failed to move {} aside to {}: {err:#}",
                    path.display(),
                    bad.display()
                );
            }
            SshHosts::default()
        }
    }
}

/// Write the hosts out atomically.
pub fn save_ssh_hosts_to(path: &Path, hosts: &SshHosts) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(hosts)?;
    let tmp = temp_file_name(path);
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The saved hosts plus the file they came from. Every change is persisted
/// immediately, like `SessionStore::mutate`.
pub struct SshHostStore {
    hosts: SshHosts,
    path: PathBuf,
    generation: u64,
}

impl SshHostStore {
    pub fn load() -> Self {
        Self::load_at(ssh_hosts_file_name())
    }

    pub(crate) fn load_at(path: PathBuf) -> Self {
        Self {
            hosts: load_ssh_hosts_from(&path),
            path,
            generation: 0,
        }
    }

    pub fn get(&self, id: NodeId) -> Option<&SshHost> {
        self.hosts.hosts.get(&id)
    }

    /// Bumped by every change, so that render caches can notice.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set(&mut self, id: NodeId, host: SshHost) {
        self.hosts.hosts.insert(id, host);
        self.changed();
    }

    /// Returns false, changing nothing, if `id` has no saved host.
    pub fn set_password(&mut self, id: NodeId, password: String) -> bool {
        match self.hosts.hosts.get_mut(&id) {
            Some(host) => {
                host.password = password;
                self.changed();
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: NodeId) -> bool {
        let removed = self.hosts.hosts.remove(&id).is_some();
        if removed {
            self.changed();
        }
        removed
    }

    /// Drop hosts whose node no longer exists in the session tree.
    pub fn retain_ids(&mut self, alive: &HashSet<NodeId>) {
        let before = self.hosts.hosts.len();
        self.hosts.hosts.retain(|id, _| alive.contains(id));
        if self.hosts.hosts.len() != before {
            self.changed();
        }
    }

    fn changed(&mut self) {
        self.generation += 1;
        if let Err(err) = save_ssh_hosts_to(&self.path, &self.hosts) {
            log::error!("failed to save {}: {err:#}", self.path.display());
        }
    }
}

thread_local! {
    static SSH_HOSTS: RefCell<Option<SshHostStore>> = RefCell::new(None);
}

/// Access the process-wide ssh host store, loading it on first use.
///
/// `func` must not call `with_ssh_hosts` again; the borrow is held for its
/// duration. It may call `store::with_store`, which is a separate cell.
pub fn with_ssh_hosts<R>(func: impl FnOnce(&mut SshHostStore) -> R) -> R {
    SSH_HOSTS.with(|store| {
        let mut store = store.borrow_mut();
        func(store.get_or_insert_with(SshHostStore::load))
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashSet;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn sample_host() -> SshHost {
        SshHost {
            host: "10.0.0.5".to_string(),
            port: 2222,
            user: "root".to_string(),
            auth: SshAuth::Password,
            password: "密码!@#".to_string(),
            key_path: String::new(),
        }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = scratch();
        let hosts = load_ssh_hosts_from(&dir.path().join("ssh_hosts.json"));
        assert!(hosts.hosts.is_empty());
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let dir = scratch();
        let path = dir.path().join("ssh_hosts.json");
        let mut hosts = SshHosts::default();
        hosts.hosts.insert(NodeId(12), sample_host());
        hosts.hosts.insert(
            NodeId(3),
            SshHost {
                auth: SshAuth::Key,
                key_path: r"C:\keys\id_ed25519".to_string(),
                password: String::new(),
                ..sample_host()
            },
        );
        save_ssh_hosts_to(&path, &hosts).unwrap();
        assert_eq!(load_ssh_hosts_from(&path), hosts);
    }

    #[test]
    fn corrupt_file_is_quarantined() {
        let dir = scratch();
        let path = dir.path().join("ssh_hosts.json");
        std::fs::write(&path, "{not json").unwrap();
        let hosts = load_ssh_hosts_from(&path);
        assert!(hosts.hosts.is_empty());
        assert!(dir.path().join("ssh_hosts.json.bad").exists());
        assert!(!path.exists());
    }

    #[test]
    fn node_id_keys_are_strings() {
        let dir = scratch();
        let path = dir.path().join("ssh_hosts.json");
        let mut hosts = SshHosts::default();
        hosts.hosts.insert(NodeId(12), sample_host());
        save_ssh_hosts_to(&path, &hosts).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"hosts\": {"), "{text}");
        assert!(text.contains("\"12\":"), "{text}");
    }

    #[test]
    fn missing_fields_take_defaults() {
        let dir = scratch();
        let path = dir.path().join("ssh_hosts.json");
        std::fs::write(&path, r#"{"hosts": {"7": {"host": "example.com"}}}"#).unwrap();
        let hosts = load_ssh_hosts_from(&path);
        let host = &hosts.hosts[&NodeId(7)];
        assert_eq!(host.port, 22);
        assert_eq!(host.auth, SshAuth::Password);
        assert_eq!(host.user, "");
        assert_eq!(host.password, "");
    }

    #[test]
    fn set_password_only_touches_known_nodes() {
        let dir = scratch();
        let mut store = SshHostStore::load_at(dir.path().join("ssh_hosts.json"));
        assert!(!store.set_password(NodeId(1), "x".to_string()));
        store.set(NodeId(1), sample_host());
        let before = store.generation();
        assert!(store.set_password(NodeId(1), "new".to_string()));
        assert_eq!(store.get(NodeId(1)).unwrap().password, "new");
        assert!(store.generation() > before);
        let reloaded = SshHostStore::load_at(dir.path().join("ssh_hosts.json"));
        assert_eq!(reloaded.get(NodeId(1)).unwrap().password, "new");
    }

    #[test]
    fn retain_ids_drops_orphans() {
        let dir = scratch();
        let mut store = SshHostStore::load_at(dir.path().join("ssh_hosts.json"));
        store.set(NodeId(1), sample_host());
        store.set(NodeId(2), sample_host());
        let alive: HashSet<NodeId> = [NodeId(2)].into_iter().collect();
        store.retain_ids(&alive);
        assert!(store.get(NodeId(1)).is_none());
        assert!(store.get(NodeId(2)).is_some());
        let reloaded = SshHostStore::load_at(dir.path().join("ssh_hosts.json"));
        assert!(reloaded.get(NodeId(1)).is_none());
    }

    #[test]
    fn remove_reports_whether_anything_went() {
        let dir = scratch();
        let mut store = SshHostStore::load_at(dir.path().join("ssh_hosts.json"));
        store.set(NodeId(4), sample_host());
        assert!(store.remove(NodeId(4)));
        assert!(!store.remove(NodeId(4)));
    }

    #[test]
    fn sessions_file_untouched() {
        let dir = scratch();
        let sessions = dir.path().join("sessions.json");
        let original = br#"{"roots":[{"id":1,"name":"x","kind":{"Session":{"spawn":{}}}}]}"#;
        std::fs::write(&sessions, original).unwrap();
        let mut store = SshHostStore::load_at(dir.path().join("ssh_hosts.json"));
        store.set(NodeId(1), sample_host());
        store.set_password(NodeId(1), "p".to_string());
        store.remove(NodeId(1));
        assert_eq!(std::fs::read(&sessions).unwrap(), original.to_vec());
    }
}
