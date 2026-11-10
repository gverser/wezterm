//! Tests against a real sshd, for what only a server can show: what a rename
//! onto an existing name does, what a cancelled upload leaves behind.
//!
//! They are ignored by default. To run them, give them a server that takes a
//! key and has a directory they may write in:
//!
//! ```text
//! WEZTERM_LIVE_SFTP=user@host:port
//! WEZTERM_LIVE_SFTP_KEY=<private key file>
//! WEZTERM_LIVE_SFTP_DIR=<remote directory>
//! cargo test -p wezterm-gui --bin wezterm-gui live_ -- --ignored --test-threads=1
//! ```
//!
//! Each test runs once with each ssh backend, in a fresh directory of its own.

use crate::sftp::transfer::{run_upload, CANCELLED, CHUNK};
use smol::io::{AsyncReadExt, AsyncWriteExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wezterm_ssh::{Config, FilePermissions, FileType, Metadata, Session, SessionEvent, Sftp};

const BACKENDS: &[&str] = &["libssh", "ssh2"];

struct Live {
    sftp: Sftp,
    /// A directory of this test's own, empty at the start.
    dir: String,
    _session: Session,
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{} is not set; see sftp/live_test.rs", name))
}

async fn connect(backend: &str, test: &str) -> Live {
    let target = env("WEZTERM_LIVE_SFTP");
    let (user, host_port) = target.split_once('@').expect("user@host:port");
    let (host, port) = host_port.rsplit_once(':').expect("user@host:port");
    let known_hosts = std::env::temp_dir().join(format!(
        "wezterm-live-sftp-known-hosts-{}",
        std::process::id()
    ));

    let mut config = Config::new().for_host(host);
    config.insert("port".to_string(), port.to_string());
    config.insert("user".to_string(), user.to_string());
    config.insert("identityfile".to_string(), env("WEZTERM_LIVE_SFTP_KEY"));
    config.insert("identitiesonly".to_string(), "yes".to_string());
    config.insert(
        "userknownhostsfile".to_string(),
        known_hosts.display().to_string(),
    );
    config.insert("wezterm_ssh_backend".to_string(), backend.to_string());

    let (session, events) = Session::connect(config).expect("connect");
    while let Ok(event) = events.recv().await {
        match event {
            SessionEvent::Banner(_) => {}
            SessionEvent::HostVerify(verify) => verify.answer(true).await.unwrap(),
            SessionEvent::Authenticate(auth) => {
                let answers = vec![String::new(); auth.prompts.len()];
                auth.answer(answers).await.unwrap();
            }
            SessionEvent::HostVerificationFailed(failed) => panic!("{}", failed),
            SessionEvent::Error(err) => panic!("{}: {}", backend, err),
            SessionEvent::Authenticated => break,
        }
    }

    let sftp = session.sftp();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = format!("{}/{test}-{backend}-{stamp}", env("WEZTERM_LIVE_SFTP_DIR"));
    sftp.create_dir(dir.as_str(), 0o755)
        .await
        .expect("test dir");
    Live {
        sftp,
        dir,
        _session: session,
    }
}

impl Live {
    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.dir)
    }

    async fn put(&self, name: &str, bytes: &[u8], mode: u32) {
        let path = self.path(name);
        let mut file = self.sftp.create(path.as_str()).await.unwrap();
        file.write_all(bytes).await.unwrap();
        file.close().await.unwrap();
        self.sftp
            .set_metadata(path.as_str(), mode_only(mode))
            .await
            .unwrap();
    }

    async fn get(&self, name: &str) -> Vec<u8> {
        let mut file = self.sftp.open(self.path(name).as_str()).await.unwrap();
        let mut bytes = vec![];
        file.read_to_end(&mut bytes).await.unwrap();
        bytes
    }

    async fn mode(&self, name: &str) -> u32 {
        self.sftp
            .metadata(self.path(name).as_str())
            .await
            .unwrap()
            .permissions
            .unwrap()
            .to_unix_mode()
    }

    async fn is_symlink(&self, name: &str) -> bool {
        self.sftp
            .symlink_metadata(self.path(name).as_str())
            .await
            .unwrap()
            .is_symlink()
    }

    /// What the directory holds, sorted.
    async fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = crate::sftp::list_dir(&self.sftp, &self.dir)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        names.sort();
        names
    }

    /// Upload `bytes` as `name`. With `cancel_after_first_chunk`, the
    /// transfer is cancelled once the first chunk is on its way.
    async fn upload(
        &self,
        name: &str,
        bytes: &[u8],
        cancel_after_first_chunk: bool,
    ) -> Result<(), String> {
        let local_dir = tempfile::tempdir().unwrap();
        let local = local_dir.path().join("upload.bin");
        std::fs::write(&local, bytes).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        run_upload(
            self.sftp.clone(),
            local,
            self.path(name),
            cancel,
            move |_| {
                if cancel_after_first_chunk {
                    flag.store(true, Ordering::SeqCst);
                }
            },
        )
        .await
    }
}

fn mode_only(mode: u32) -> Metadata {
    Metadata {
        ty: FileType::File,
        permissions: Some(FilePermissions::from_unix_mode(mode)),
        size: None,
        uid: None,
        gid: None,
        accessed: None,
        modified: None,
    }
}

/// Enough for several chunks, so a cancel lands in the middle.
fn new_contents() -> Vec<u8> {
    (0..CHUNK * 4).map(|i| (i % 251) as u8).collect()
}

fn each_backend(test: &str, body: impl Fn(Live) -> futures::future::LocalBoxFuture<'static, ()>) {
    for backend in BACKENDS {
        eprintln!("== {test} with {backend}");
        smol::block_on(async {
            let live = connect(backend, test).await;
            body(live).await;
        });
    }
}

#[test]
#[ignore = "needs a live sshd; see sftp/live_test.rs"]
fn live_cancelled_upload_over_a_file_keeps_the_original() {
    each_backend("cancel-over", |live| {
        Box::pin(async move {
            live.put("app.conf", b"original\n", 0o640).await;
            let result = live.upload("app.conf", &new_contents(), true).await;
            assert_eq!(result, Err(CANCELLED.to_string()));
            assert_eq!(live.get("app.conf").await, b"original\n");
            assert_eq!(live.mode("app.conf").await, 0o640);
            assert_eq!(live.names().await, vec!["app.conf"]);
        })
    });
}

#[test]
#[ignore = "needs a live sshd; see sftp/live_test.rs"]
fn live_cancelled_new_upload_leaves_nothing() {
    each_backend("cancel-new", |live| {
        Box::pin(async move {
            let result = live.upload("new.bin", &new_contents(), true).await;
            assert_eq!(result, Err(CANCELLED.to_string()));
            assert_eq!(live.names().await, Vec::<String>::new());
        })
    });
}

#[test]
#[ignore = "needs a live sshd; see sftp/live_test.rs"]
fn live_upload_over_a_file_replaces_it_and_keeps_its_mode() {
    each_backend("replace", |live| {
        Box::pin(async move {
            live.put("run.sh", b"old\n", 0o750).await;
            let contents = new_contents();
            live.upload("run.sh", &contents, false).await.unwrap();
            assert_eq!(live.get("run.sh").await, contents);
            assert_eq!(live.mode("run.sh").await, 0o750);
            assert_eq!(live.names().await, vec!["run.sh"]);
        })
    });
}

#[test]
#[ignore = "needs a live sshd; see sftp/live_test.rs"]
fn live_upload_through_a_symlink_replaces_the_file_it_points_at() {
    each_backend("symlink", |live| {
        Box::pin(async move {
            live.put("real.txt", b"old\n", 0o644).await;
            live.sftp
                .symlink(
                    live.path("real.txt").as_str(),
                    live.path("link.txt").as_str(),
                )
                .await
                .unwrap();
            assert!(live.is_symlink("link.txt").await, "set up the link");

            let contents = new_contents();
            live.upload("link.txt", &contents, false).await.unwrap();
            assert!(live.is_symlink("link.txt").await, "still a link");
            assert_eq!(live.get("real.txt").await, contents);
            assert_eq!(live.names().await, vec!["link.txt", "real.txt"]);
        })
    });
}

#[test]
#[ignore = "needs a live sshd; see sftp/live_test.rs"]
fn live_rename_onto_an_existing_name_is_refused() {
    each_backend("rename", |live| {
        Box::pin(async move {
            live.put("a.txt", b"a\n", 0o644).await;
            live.put("b.txt", b"b\n", 0o644).await;
            let result =
                crate::sftp::rename_file(&live.sftp, &live.path("a.txt"), &live.path("b.txt"))
                    .await;
            assert!(result.is_err(), "renamed over b.txt: {:?}", result);
            assert_eq!(live.get("a.txt").await, b"a\n");
            assert_eq!(live.get("b.txt").await, b"b\n");

            crate::sftp::rename_file(&live.sftp, &live.path("a.txt"), &live.path("c.txt"))
                .await
                .unwrap();
            assert_eq!(live.names().await, vec!["b.txt", "c.txt"]);
        })
    });
}
