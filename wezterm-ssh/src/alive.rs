//! OpenSSH's ServerAliveCountMax: a connection whose server has sent nothing
//! back for that many keepalives in a row is given up for dead.
//!
//! Neither libssh nor libssh2 says when it last heard from the server, so the
//! socket is asked instead: the system's count of bytes received on it goes up
//! whenever anything arrives, whoever reads it. A keepalive that wants a reply
//! makes an idle but healthy server send something each time.

use crate::config::ConfigMap;
use filedescriptor::SocketDescriptor;

/// OpenSSH's default.
const DEFAULT_COUNT_MAX: u32 = 3;

/// `ServerAliveCountMax` from the config, OpenSSH's 3 when unset or unreadable.
pub(crate) fn server_alive_count_max(config: &ConfigMap) -> u32 {
    config
        .get("serveralivecountmax")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_COUNT_MAX)
}

pub(crate) struct ServerAlive {
    count_max: u32,
    /// Keepalives in a row with nothing received since.
    unanswered: u32,
    /// The socket's total of bytes received when the last keepalive went out.
    received_at_send: Option<u64>,
}

impl ServerAlive {
    pub fn new(count_max: u32) -> Self {
        Self {
            count_max,
            unanswered: 0,
            received_at_send: None,
        }
    }

    /// Call as each keepalive goes out, with the socket's total of bytes
    /// received so far, or None where the system can't say. True once more
    /// than `count_max` keepalives in a row have had nothing come back.
    pub fn gone(&mut self, received: Option<u64>) -> bool {
        match (received, self.received_at_send) {
            (Some(now), Some(then)) if now == then => self.unanswered += 1,
            _ => self.unanswered = 0,
        }
        self.received_at_send = received;
        self.unanswered > self.count_max
    }
}

/// How many bytes the system has received on this TCP socket so far. None
/// where it can't say: not a TCP socket, or a system this isn't done for.
#[cfg(windows)]
pub(crate) fn bytes_received(sock: SocketDescriptor) -> Option<u64> {
    use windows_sys::Win32::Networking::WinSock::{TCP_INFO_v0, WSAIoctl, SIO_TCP_INFO};

    let version: u32 = 0;
    let mut info = TCP_INFO_v0::default();
    let mut returned = 0u32;
    let rc = unsafe {
        WSAIoctl(
            sock,
            SIO_TCP_INFO,
            &version as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
            &mut info as *mut TCP_INFO_v0 as *mut _,
            std::mem::size_of::<TCP_INFO_v0>() as u32,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    (rc == 0).then_some(info.BytesIn)
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn bytes_received(sock: SocketDescriptor) -> Option<u64> {
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            sock,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            &mut info as *mut libc::tcp_info as *mut libc::c_void,
            &mut len,
        )
    };
    // Kernels before 4.1 fill in less, and the count isn't there.
    let end = (&info.tcpi_bytes_received as *const u64 as usize)
        - (&info as *const libc::tcp_info as usize)
        + std::mem::size_of::<u64>();
    (rc == 0 && len as usize >= end).then_some(info.tcpi_bytes_received)
}

#[cfg(not(any(windows, all(target_os = "linux", target_env = "gnu"))))]
pub(crate) fn bytes_received(_sock: SocketDescriptor) -> Option<u64> {
    None
}

#[cfg(test)]
mod test {
    use super::*;

    fn run(alive: &mut ServerAlive, received: &[Option<u64>]) -> Vec<bool> {
        received.iter().map(|r| alive.gone(*r)).collect()
    }

    #[test]
    fn a_server_that_answers_is_never_given_up() {
        let mut alive = ServerAlive::new(3);
        for n in 0..100 {
            assert!(!alive.gone(Some(n * 52)));
        }
    }

    #[test]
    fn more_than_count_max_unanswered_keepalives_mean_the_server_is_gone() {
        let mut alive = ServerAlive::new(3);
        // The first goes out with nothing to judge; the next four find
        // nothing new, and only the fourth of those is one too many.
        assert_eq!(
            run(&mut alive, &[Some(900); 5]),
            [false, false, false, false, true]
        );
    }

    #[test]
    fn anything_received_starts_the_count_again() {
        let mut alive = ServerAlive::new(1);
        assert_eq!(
            run(
                &mut alive,
                &[Some(10), Some(10), Some(36), Some(36), Some(36)]
            ),
            [false, false, false, false, true]
        );
    }

    #[test]
    fn without_a_byte_count_nothing_is_judged() {
        let mut alive = ServerAlive::new(0);
        assert_eq!(run(&mut alive, &[None; 5]), [false; 5]);
        // A count that comes and goes never adds up to a verdict either.
        assert_eq!(
            run(&mut alive, &[Some(5), None, Some(5), None, Some(5)]),
            [false; 5]
        );
    }

    #[test]
    fn count_max_zero_gives_up_at_the_first_unanswered_keepalive() {
        let mut alive = ServerAlive::new(0);
        assert_eq!(run(&mut alive, &[Some(7), Some(7)]), [false, true]);
    }

    #[test]
    fn count_max_comes_from_the_config_or_is_openssh_s_three() {
        let mut config = ConfigMap::new();
        assert_eq!(server_alive_count_max(&config), 3);
        config.insert("serveralivecountmax".to_string(), "5".to_string());
        assert_eq!(server_alive_count_max(&config), 5);
        config.insert("serveralivecountmax".to_string(), "lots".to_string());
        assert_eq!(server_alive_count_max(&config), 3);
    }

    #[cfg(any(windows, all(target_os = "linux", target_env = "gnu")))]
    #[test]
    fn bytes_received_counts_what_arrives_on_a_tcp_socket() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        fn descriptor(stream: &TcpStream) -> SocketDescriptor {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawSocket;
                stream.as_raw_socket() as SocketDescriptor
            }
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                stream.as_raw_fd()
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();

        let before = bytes_received(descriptor(&client)).expect("a TCP socket has a count");
        server.write_all(b"hello").unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).unwrap();
        let after = bytes_received(descriptor(&client)).unwrap();
        assert_eq!(after - before, 5);
    }
}

/// Against a real sshd, through a proxy on 127.0.0.1 that can stop passing
/// anything on while holding both connections open: the server looks hung,
/// and nothing at the TCP level will ever say so. Ignored by default; it
/// takes the same server as wezterm-gui's sftp/live_test.rs:
///
/// ```text
/// WEZTERM_LIVE_SFTP=user@host:port
/// WEZTERM_LIVE_SFTP_KEY=<private key file>
/// cargo test -p wezterm-ssh --lib live_ -- --ignored
/// ```
#[cfg(test)]
mod live_test {
    use crate::{Config, Session, SessionEvent};
    use smol::channel::Receiver;
    use std::io::{ErrorKind, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{} is not set; see alive.rs", name))
    }

    /// Accepts one connection and passes it on to `upstream` until frozen.
    fn proxy(upstream: String) -> (SocketAddr, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let frozen = Arc::new(AtomicBool::new(false));
        let pumps_frozen = Arc::clone(&frozen);
        std::thread::spawn(move || {
            let (client, _) = listener.accept().unwrap();
            let server = TcpStream::connect(upstream).unwrap();
            pump(
                client.try_clone().unwrap(),
                server.try_clone().unwrap(),
                Arc::clone(&pumps_frozen),
            );
            pump(server, client, pumps_frozen);
        });
        (addr, frozen)
    }

    fn pump(mut from: TcpStream, mut to: TcpStream, frozen: Arc<AtomicBool>) {
        std::thread::spawn(move || {
            from.set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut buf = [0u8; 16384];
            loop {
                if frozen.load(Ordering::SeqCst) {
                    // Both sockets stay open; nothing moves.
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                match from.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if to.write_all(&buf[..n]).is_err() {
                            return;
                        }
                    }
                    Err(err)
                        if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => return,
                }
            }
        });
    }

    /// The next event, or None if none comes within `wait`. A session whose
    /// thread has gone without an event (a panic) fails the test here rather
    /// than passing for one still up.
    async fn next_event(events: &Receiver<SessionEvent>, wait: Duration) -> Option<SessionEvent> {
        smol::future::or(
            async {
                let event = events.recv().await;
                Some(event.expect("the session's thread ended without an event"))
            },
            async {
                smol::Timer::after(wait).await;
                None
            },
        )
        .await
    }

    #[test]
    #[ignore = "needs a live sshd; see alive.rs"]
    fn live_a_server_that_stops_answering_ends_the_session() {
        let target = env("WEZTERM_LIVE_SFTP");
        let (user, upstream) = target.split_once('@').expect("user@host:port");
        let (addr, frozen) = proxy(upstream.to_string());
        let known_hosts = std::env::temp_dir().join(format!(
            "wezterm-live-alive-known-hosts-{}",
            std::process::id()
        ));

        let mut config = Config::new().for_host("127.0.0.1");
        for (key, value) in [
            ("port", addr.port().to_string()),
            ("user", user.to_string()),
            ("identityfile", env("WEZTERM_LIVE_SFTP_KEY")),
            ("identitiesonly", "yes".to_string()),
            ("userknownhostsfile", known_hosts.display().to_string()),
            ("wezterm_ssh_backend", "libssh".to_string()),
            ("serveraliveinterval", "1".to_string()),
            ("serveralivecountmax", "1".to_string()),
        ] {
            config.insert(key.to_string(), value);
        }

        let (session, events) = Session::connect(config).expect("connect");
        smol::block_on(async {
            while let Ok(event) = events.recv().await {
                match event {
                    SessionEvent::Banner(_) => {}
                    SessionEvent::HostVerify(verify) => verify.answer(true).await.unwrap(),
                    SessionEvent::Authenticate(auth) => {
                        let answers = vec![String::new(); auth.prompts.len()];
                        auth.answer(answers).await.unwrap();
                    }
                    SessionEvent::HostVerificationFailed(failed) => panic!("{}", failed),
                    SessionEvent::Error(err) => panic!("{}", err),
                    SessionEvent::Authenticated => break,
                }
            }

            // Six keepalives, each answered: the session stays.
            if let Some(SessionEvent::Error(err)) =
                next_event(&events, Duration::from_secs(6)).await
            {
                panic!("ended while the server was answering: {}", err);
            }

            frozen.store(true, Ordering::SeqCst);
            let frozen_at = Instant::now();
            match next_event(&events, Duration::from_secs(10)).await {
                Some(SessionEvent::Error(err)) => {
                    assert!(err.contains("与服务器的连接已断开"), "{}", err);
                    eprintln!("given up {:?} after the proxy froze", frozen_at.elapsed());
                }
                Some(_) => panic!("an event other than the error"),
                None => panic!("still up 10s after the server stopped answering"),
            }
        });
        drop(session);
        std::fs::remove_file(known_hosts).ok();
    }
}
