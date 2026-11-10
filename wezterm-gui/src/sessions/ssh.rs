//! Session nodes that connect over ssh.
//!
//! Each such node gets its own `RemoteSshDomain`, registered with the mux at
//! runtime under the name `sidebar-ssh:<node id>`. The connection lives in this
//! GUI process -- closing the window closes it, as in MobaXterm -- so the
//! mux-server bridge never sees these panes.

pub use wezterm_sessions::ssh::*;

use crate::sessions::store::with_store;
use crate::sessions::NodeId;
use ::window::WindowOps;
use config::{Shell, SshDomain, SshMultiplexing};
use mux::domain::{Domain, DomainId};
use mux::pane::Pane;
use mux::ssh::{RemoteSshDomain, SshPasswordHelper};
use mux::Mux;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The domain definition for a saved host.
///
/// The port goes in `ssh_option` rather than `remote_address`, because
/// `ssh_domain_to_ssh_config` splits the address on ':' and would misread an
/// IPv6 address. `assume_shell = Posix` makes the remote command line carry
/// `WEZTERM_REMOTE_PANE` itself, independent of the server's `AcceptEnv`; the
/// directory probe relies on finding it.
pub fn ssh_domain_for(id: NodeId, host: &SshHost) -> SshDomain {
    let mut ssh_option = HashMap::new();
    ssh_option.insert("port".to_string(), host.port.to_string());
    // A connection that dies quietly (wifi, VPN, sleep, a NAT timeout) sends
    // nothing back; only something sent out finds it dead. Without this an
    // idle pane looks connected until the next keystroke, then vanishes.
    ssh_option.insert("serveraliveinterval".to_string(), "30".to_string());
    if host.auth == SshAuth::Key {
        ssh_option.insert("identityfile".to_string(), host.key_path.trim().to_string());
        ssh_option.insert("identitiesonly".to_string(), "yes".to_string());
    }
    let user = host.user.trim();
    SshDomain {
        name: ssh_domain_name(id),
        remote_address: host.host.trim().to_string(),
        username: if user.is_empty() {
            None
        } else {
            Some(user.to_string())
        },
        multiplexing: SshMultiplexing::None,
        assume_shell: Shell::Posix,
        ssh_option,
        ..SshDomain::default()
    }
}

/// Supplies a node's saved password to the connection, and saves the one the
/// user typed when they ask it to.
struct NodePasswordHelper {
    id: NodeId,
    saved: Mutex<String>,
    rejected: AtomicBool,
}

impl NodePasswordHelper {
    fn update_saved(&self, password: &str) {
        let mut saved = self.saved.lock().unwrap();
        if *saved != password {
            *saved = password.to_string();
            // A new password deserves a fresh try.
            self.rejected.store(false, Ordering::SeqCst);
        }
    }
}

impl SshPasswordHelper for NodePasswordHelper {
    fn saved_password(&self) -> Option<String> {
        if self.rejected.load(Ordering::SeqCst) {
            return None;
        }
        let saved = self.saved.lock().unwrap();
        if saved.is_empty() {
            None
        } else {
            Some(saved.clone())
        }
    }

    fn has_saved_password(&self) -> bool {
        !self.saved.lock().unwrap().is_empty()
    }

    fn saved_password_rejected(&self) {
        self.rejected.store(true, Ordering::SeqCst);
    }

    fn save_password(&self, password: String) {
        self.update_saved(&password);
        let id = self.id;
        promise::spawn::spawn_into_main_thread(async move {
            with_ssh_hosts(|store| store.set_password(id, password));
            for win in crate::frontend::front_end().gui_windows() {
                win.window.invalidate();
            }
        })
        .detach();
    }
}

/// What a registered domain was built from, so that an edit can be noticed.
struct Registered {
    /// The host with its password blanked: changing only the password must
    /// not replace the domain, or every save would orphan the open panes.
    shape: SshHost,
    domain_id: DomainId,
    helper: Arc<NodePasswordHelper>,
}

thread_local! {
    static REGISTERED: RefCell<HashMap<NodeId, Registered>> = RefCell::new(HashMap::new());
}

fn without_password(host: &SshHost) -> SshHost {
    SshHost {
        password: String::new(),
        ..host.clone()
    }
}

/// Make sure the mux has a domain for this node that matches its saved host.
///
/// An unchanged host reuses the registered domain, and with it the live
/// connection. A changed one registers a new domain under the same name; the
/// old one keeps serving the panes already open on it, so editing a node never
/// disturbs a running session -- the edit applies to the next connection.
pub fn ensure_ssh_domain(id: NodeId) -> anyhow::Result<Arc<dyn Domain>> {
    let host = with_ssh_hosts(|store| store.get(id).cloned())
        .ok_or_else(|| anyhow::anyhow!("session {id} has no ssh host"))?;
    let mux = Mux::get();
    let shape = without_password(&host);

    let existing = REGISTERED.with(|reg| {
        reg.borrow().get(&id).and_then(|r| {
            if r.shape == shape {
                r.helper.update_saved(&host.password);
                mux.get_domain(r.domain_id)
            } else {
                None
            }
        })
    });
    if let Some(domain) = existing {
        return Ok(domain);
    }

    let helper = Arc::new(NodePasswordHelper {
        id,
        saved: Mutex::new(host.password.clone()),
        rejected: AtomicBool::new(false),
    });
    let domain: Arc<dyn Domain> = Arc::new(RemoteSshDomain::with_ssh_domain_and_helper(
        &ssh_domain_for(id, &host),
        Some(Arc::clone(&helper) as Arc<dyn SshPasswordHelper>),
    )?);
    mux.add_domain(&domain);
    REGISTERED.with(|reg| {
        reg.borrow_mut().insert(
            id,
            Registered {
                shape,
                domain_id: domain.domain_id(),
                helper,
            },
        )
    });
    Ok(domain)
}

/// Register a domain for every saved ssh node, so that they can also be
/// reached by name (`wezterm cli spawn --domain-name sidebar-ssh:12`).
///
/// Hosts whose node is not in the tree are left alone: the tree comes back
/// empty when sessions.json can't be read or parsed, and pruning against it
/// would throw away every saved host and password. Hosts are forgotten only
/// when their node is deleted; see [`forget_ssh_hosts`].
pub fn register_all_ssh_domains() {
    let alive: Vec<NodeId> = with_store(|store| {
        store
            .tree()
            .sessions_with_names()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    });
    for id in alive {
        if with_ssh_hosts(|store| store.get(id).is_some()) {
            if let Err(err) = ensure_ssh_domain(id) {
                log::error!("registering ssh domain for session {id}: {err:#}");
            }
        }
    }
}

/// Drop the saved hosts of nodes that have just been deleted; deleting a
/// folder takes every session inside it along.
pub fn forget_ssh_hosts(ids: &[NodeId]) {
    with_ssh_hosts(|store| {
        for id in ids {
            store.remove(*id);
        }
    });
}

/// The sidebar node and domain behind a pane, if it is one of ours.
pub fn ssh_domain_of_pane(pane: &dyn Pane) -> Option<(NodeId, DomainId)> {
    let domain_id = pane.domain_id();
    let domain = Mux::get().get_domain(domain_id)?;
    let id = node_of_domain_name(domain.domain_name())?;
    Some((id, domain_id))
}

/// The domain, if it is a sidebar ssh domain. Callers reach the connection
/// with `downcast_ref::<RemoteSshDomain>()`.
pub fn remote_ssh_domain(domain_id: DomainId) -> Option<Arc<dyn Domain>> {
    let domain = Mux::get().get_domain(domain_id)?;
    node_of_domain_name(domain.domain_name())?;
    if domain.downcast_ref::<RemoteSshDomain>().is_some() {
        Some(domain)
    } else {
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use config::{Shell, SshMultiplexing};

    fn host(user: &str, auth: SshAuth) -> SshHost {
        SshHost {
            host: "10.0.0.5".to_string(),
            port: 2222,
            user: user.to_string(),
            auth,
            password: "pw".to_string(),
            key_path: r"C:\keys\id_ed25519".to_string(),
        }
    }

    #[test]
    fn domain_name_round_trips() {
        assert_eq!(ssh_domain_name(NodeId(12)), "sidebar-ssh:12");
        assert_eq!(
            node_of_domain_name(&ssh_domain_name(NodeId(12))),
            Some(NodeId(12))
        );
        assert_eq!(node_of_domain_name("SSH:x"), None);
        assert_eq!(node_of_domain_name("sidebar-ssh:abc"), None);
    }

    #[test]
    fn password_host_maps_to_posix_domain() {
        let dom = ssh_domain_for(NodeId(3), &host("root", SshAuth::Password));
        assert_eq!(dom.name, "sidebar-ssh:3");
        assert_eq!(dom.remote_address, "10.0.0.5");
        assert_eq!(dom.ssh_option.get("port").map(|s| s.as_str()), Some("2222"));
        assert_eq!(dom.username.as_deref(), Some("root"));
        assert_eq!(dom.assume_shell, Shell::Posix);
        assert_eq!(dom.multiplexing, SshMultiplexing::None);
        assert!(dom.ssh_option.get("identityfile").is_none());
    }

    #[test]
    fn saved_hosts_send_keepalives() {
        let dom = ssh_domain_for(NodeId(3), &host("root", SshAuth::Password));
        assert_eq!(
            dom.ssh_option
                .get("serveraliveinterval")
                .map(|s| s.as_str()),
            Some("30")
        );
    }

    #[test]
    fn key_host_sets_identity() {
        let dom = ssh_domain_for(NodeId(3), &host("root", SshAuth::Key));
        assert_eq!(
            dom.ssh_option.get("identityfile").map(|s| s.as_str()),
            Some(r"C:\keys\id_ed25519")
        );
        assert_eq!(
            dom.ssh_option.get("identitiesonly").map(|s| s.as_str()),
            Some("yes")
        );
    }

    #[test]
    fn ipv6_host_keeps_address_whole() {
        let mut h = host("root", SshAuth::Password);
        h.host = "fe80::1".to_string();
        let dom = ssh_domain_for(NodeId(3), &h);
        assert_eq!(dom.remote_address, "fe80::1");
        assert_eq!(dom.ssh_option.get("port").map(|s| s.as_str()), Some("2222"));
    }

    #[test]
    fn empty_user_leaves_username_unset() {
        let dom = ssh_domain_for(NodeId(3), &host("", SshAuth::Password));
        assert_eq!(dom.username, None);
    }

    #[test]
    fn surrounding_spaces_are_trimmed() {
        let mut h = host(" root ", SshAuth::Password);
        h.host = " 10.0.0.5 ".to_string();
        let dom = ssh_domain_for(NodeId(3), &h);
        assert_eq!(dom.remote_address, "10.0.0.5");
        assert_eq!(dom.username.as_deref(), Some("root"));
    }
}
