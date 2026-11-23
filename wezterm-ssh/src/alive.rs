//! OpenSSH's ServerAliveCountMax: a connection whose server has sent nothing
//! back for that many keepalives in a row is given up for dead.
//!
//! Neither libssh nor libssh2 says when it last heard from the server, so the
//! socket is asked instead: the system's count of bytes received on it goes up
//! whenever anything arrives, whoever reads it. A keepalive that wants a reply
//! makes an idle but healthy server send something each time.
//!
//! Requests are waited on in blocking mode, with no keepalives going out
//! meanwhile, so they get the same allowance: the library is told to give up
//! on a reply after as long as the keepalives would take. With libssh, a
//! request it gave up on with nothing at all received in the meantime ends
//! the session too; with libssh2, any it gave up on (see sessioninner.rs).

use crate::config::ConfigMap;
use filedescriptor::SocketDescriptor;
use std::time::Duration;

/// OpenSSH's default.
const DEFAULT_COUNT_MAX: u32 = 3;

/// The longest libssh is told to wait for a reply. It takes the timeout as a
/// C long of microseconds, 32 bits on Windows (about 35 minutes), and refuses
/// one that doesn't fit.
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How much earlier than Instant libssh may find a timeout up. On Windows it
/// times with GetSystemTimeAsFileTime (libssh-rs-sys doesn't build it with
/// clock_gettime there), which moves in steps of about 16ms, so its start can
/// be stale by that much. A request that needs no reply is over long before
/// this close to the timeout, and one that got its reply received something.
const LIBSSH_CLOCK_SLACK: Duration = Duration::from_millis(100);

/// `ServerAliveCountMax` from the config, OpenSSH's 3 when unset or unreadable.
pub(crate) fn server_alive_count_max(config: &ConfigMap) -> u32 {
    config
        .get("serveralivecountmax")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_COUNT_MAX)
}

// Only libssh's keepalives and requests are judged here; ssh2 goes by
// libssh2's own word that it gave up (sessioninner.rs).
#[cfg_attr(not(feature = "libssh-rs"), allow(dead_code))]
pub(crate) struct ServerAlive {
    count_max: u32,
    /// See `request_timeout`.
    request_timeout: Option<Duration>,
    /// Keepalives in a row with nothing received since.
    unanswered: u32,
    /// The socket's total of bytes received when the last keepalive went out.
    received_at_send: Option<u64>,
}

impl ServerAlive {
    /// `interval` is ServerAliveInterval, None when no keepalives are sent.
    pub fn new(count_max: u32, interval: Option<Duration>) -> Self {
        Self {
            count_max,
            request_timeout: interval.map(|interval| {
                interval
                    .saturating_mul(count_max.saturating_add(1))
                    .min(MAX_REQUEST_TIMEOUT)
            }),
            unanswered: 0,
            received_at_send: None,
        }
    }

    /// How long a request waited on in blocking mode may go unanswered: as
    /// long as it takes the keepalives to give up, `count_max` intervals and
    /// one more. None without keepalives, where nothing is given up.
    pub fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    /// Whether a request waited on in blocking mode, which took `took`, ran
    /// out of `request_timeout` with nothing at all received meanwhile;
    /// `before` and `after` are the socket's totals around it.
    #[cfg_attr(not(feature = "libssh-rs"), allow(dead_code))]
    pub fn request_unanswered(
        &self,
        before: Option<u64>,
        after: Option<u64>,
        took: Duration,
    ) -> bool {
        match (self.request_timeout, before, after) {
            (Some(timeout), Some(before), Some(after)) => {
                took >= timeout.saturating_sub(LIBSSH_CLOCK_SLACK) && before == after
            }
            _ => false,
        }
    }

    /// Call as each keepalive goes out, with the socket's total of bytes
    /// received so far, or None where the system can't say. True once more
    /// than `count_max` keepalives in a row have had nothing come back.
    #[cfg_attr(not(feature = "libssh-rs"), allow(dead_code))]
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
        let mut alive = ServerAlive::new(3, None);
        for n in 0..100 {
            assert!(!alive.gone(Some(n * 52)));
        }
    }

    #[test]
    fn more_than_count_max_unanswered_keepalives_mean_the_server_is_gone() {
        let mut alive = ServerAlive::new(3, None);
        // The first goes out with nothing to judge; the next four find
        // nothing new, and only the fourth of those is one too many.
        assert_eq!(
            run(&mut alive, &[Some(900); 5]),
            [false, false, false, false, true]
        );
    }

    #[test]
    fn anything_received_starts_the_count_again() {
        let mut alive = ServerAlive::new(1, None);
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
        let mut alive = ServerAlive::new(0, None);
        assert_eq!(run(&mut alive, &[None; 5]), [false; 5]);
        // A count that comes and goes never adds up to a verdict either.
        assert_eq!(
            run(&mut alive, &[Some(5), None, Some(5), None, Some(5)]),
            [false; 5]
        );
    }

    #[test]
    fn count_max_zero_gives_up_at_the_first_unanswered_keepalive() {
        let mut alive = ServerAlive::new(0, None);
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

    #[test]
    fn a_request_gets_as_long_as_the_keepalives_would_take() {
        let s = Duration::from_secs;
        // The sidebar's 30 seconds and OpenSSH's three.
        assert_eq!(
            ServerAlive::new(3, Some(s(30))).request_timeout(),
            Some(s(120))
        );
        assert_eq!(
            ServerAlive::new(0, Some(s(30))).request_timeout(),
            Some(s(30))
        );
        assert_eq!(ServerAlive::new(3, None).request_timeout(), None);
        // libssh can't be given more than about 35 minutes on Windows.
        assert_eq!(
            ServerAlive::new(3, Some(s(3600))).request_timeout(),
            Some(MAX_REQUEST_TIMEOUT)
        );
        assert!(MAX_REQUEST_TIMEOUT.as_micros() <= i32::MAX as u128);
    }

    #[test]
    fn a_request_is_unanswered_only_when_it_timed_out_with_nothing_received() {
        let s = Duration::from_secs;
        let alive = ServerAlive::new(1, Some(s(1)));
        assert!(alive.request_unanswered(Some(40), Some(40), s(2)));
        // libssh's clock is coarser than Instant's and can call time a
        // little early.
        assert!(alive.request_unanswered(Some(40), Some(40), Duration::from_millis(1985)));
        // Something came: the server is there, just slow with this one.
        assert!(!alive.request_unanswered(Some(40), Some(90), s(2)));
        // Answered in time.
        assert!(!alive.request_unanswered(Some(40), Some(40), Duration::from_millis(500)));
        // No count, no verdict.
        assert!(!alive.request_unanswered(None, None, s(2)));
        // Without keepalives libssh isn't given a timeout either.
        assert!(!ServerAlive::new(1, None).request_unanswered(Some(40), Some(40), s(600)));
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
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{} is not set; see alive.rs", name))
    }

    /// Accepts one connection and passes it on to `upstream` until frozen,
    /// holding what the server sends back for `delay` milliseconds first.
    fn proxy(upstream: String) -> (SocketAddr, Arc<AtomicBool>, Arc<AtomicU64>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let frozen = Arc::new(AtomicBool::new(false));
        let delay = Arc::new(AtomicU64::new(0));
        let (pumps_frozen, pumps_delay) = (Arc::clone(&frozen), Arc::clone(&delay));
        std::thread::spawn(move || {
            let (client, _) = listener.accept().unwrap();
            let server = TcpStream::connect(upstream).unwrap();
            pump(
                client.try_clone().unwrap(),
                server.try_clone().unwrap(),
                Arc::clone(&pumps_frozen),
                Arc::new(AtomicU64::new(0)),
            );
            pump(server, client, pumps_frozen, pumps_delay);
        });
        (addr, frozen, delay)
    }

    fn pump(
        mut from: TcpStream,
        mut to: TcpStream,
        frozen: Arc<AtomicBool>,
        delay: Arc<AtomicU64>,
    ) {
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
                        let delay = delay.load(Ordering::SeqCst);
                        if delay > 0 {
                            std::thread::sleep(Duration::from_millis(delay));
                        }
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

    /// An authenticated session through a fresh proxy, with the given
    /// backend, ServerAliveInterval and ServerAliveCountMax.
    struct Live {
        session: Session,
        events: Receiver<SessionEvent>,
        frozen: Arc<AtomicBool>,
        delay: Arc<AtomicU64>,
        known_hosts: std::path::PathBuf,
    }

    impl Drop for Live {
        fn drop(&mut self) {
            std::fs::remove_file(&self.known_hosts).ok();
        }
    }

    fn connect(name: &str, backend: &str, interval: u64, count_max: u32) -> Live {
        // RUST_LOG=wezterm_ssh=debug shows which check gave up.
        let _ = env_logger::try_init();
        let target = env("WEZTERM_LIVE_SFTP");
        let (user, upstream) = target.split_once('@').expect("user@host:port");
        let (addr, frozen, delay) = proxy(upstream.to_string());
        let known_hosts = std::env::temp_dir().join(format!(
            "wezterm-live-alive-known-hosts-{}-{}",
            std::process::id(),
            name
        ));

        let mut config = Config::new().for_host("127.0.0.1");
        for (key, value) in [
            ("port", addr.port().to_string()),
            ("user", user.to_string()),
            ("identityfile", env("WEZTERM_LIVE_SFTP_KEY")),
            ("identitiesonly", "yes".to_string()),
            ("userknownhostsfile", known_hosts.display().to_string()),
            ("wezterm_ssh_backend", backend.to_string()),
            ("serveraliveinterval", interval.to_string()),
            ("serveralivecountmax", count_max.to_string()),
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
        });
        Live {
            session,
            events,
            frozen,
            delay,
            known_hosts,
        }
    }

    fn expect_given_up(event: Option<SessionEvent>, since: Instant, what: &str) {
        match event {
            Some(SessionEvent::Error(err)) => {
                assert!(err.contains("与服务器的连接已断开"), "{}", err);
                eprintln!(
                    "{}: given up {:?} after the proxy froze",
                    what,
                    since.elapsed()
                );
            }
            Some(_) => panic!("{}: an event other than the error", what),
            None => panic!("{}: still up 10s after the server stopped answering", what),
        }
    }

    #[cfg(feature = "libssh-rs")]
    #[test]
    #[ignore = "needs a live sshd; see alive.rs"]
    fn live_a_server_that_stops_answering_ends_the_session() {
        let live = connect("idle", "libssh", 1, 1);
        smol::block_on(async {
            // Six keepalives, each answered: the session stays.
            if let Some(SessionEvent::Error(err)) =
                next_event(&live.events, Duration::from_secs(6)).await
            {
                panic!("ended while the server was answering: {}", err);
            }

            live.frozen.store(true, Ordering::SeqCst);
            let frozen_at = Instant::now();
            let event = next_event(&live.events, Duration::from_secs(10)).await;
            expect_given_up(event, frozen_at, "idle");
        });
    }

    /// A request is waited on with no keepalives going out, so it has to be
    /// given up on by itself.
    #[cfg(feature = "libssh-rs")]
    #[test]
    #[ignore = "needs a live sshd; see alive.rs"]
    fn live_a_request_the_server_never_answers_ends_the_session() {
        request_never_answered("libssh");
    }

    #[cfg(feature = "ssh2")]
    #[test]
    #[ignore = "needs a live sshd; see alive.rs"]
    fn live_ssh2_a_request_the_server_never_answers_ends_the_session() {
        request_never_answered("ssh2");
    }

    fn request_never_answered(backend: &str) {
        let live = connect(&format!("request-{}", backend), backend, 1, 1);
        smol::block_on(async {
            live.frozen.store(true, Ordering::SeqCst);
            let frozen_at = Instant::now();
            // A pump in the middle of a read when it froze still passes that
            // on (what the server sends straight after authentication, say);
            // let that land first, so the request goes out to silence.
            smol::Timer::after(Duration::from_millis(300)).await;
            let exec = smol::future::or(
                async { Some(live.session.exec("true", None).await) },
                async {
                    smol::Timer::after(Duration::from_secs(15)).await;
                    None
                },
            );
            let (exec, event) =
                smol::future::zip(exec, next_event(&live.events, Duration::from_secs(10))).await;
            expect_given_up(event, frozen_at, &format!("request ({})", backend));
            assert!(
                matches!(exec, Some(Err(_))),
                "the request should have failed, not waited on"
            );
        });
    }

    /// A server that answers, only slowly, is waited for. libssh2 used to
    /// give up on a request as soon as its next keepalive came due, a second
    /// or two here, and leave the request half done.
    #[cfg(feature = "ssh2")]
    #[test]
    #[ignore = "needs a live sshd; see alive.rs"]
    fn live_ssh2_a_slow_answer_is_waited_for() {
        slow_answer_is_waited_for("ssh2");
    }

    #[cfg(feature = "libssh-rs")]
    #[test]
    #[ignore = "needs a live sshd; see alive.rs"]
    fn live_a_slow_answer_is_waited_for() {
        slow_answer_is_waited_for("libssh");
    }

    fn slow_answer_is_waited_for(backend: &str) {
        // Keepalives every 2s (libssh2's least), so requests get 8s.
        let live = connect(&format!("slow-{}", backend), backend, 2, 3);
        smol::block_on(async {
            live.delay.store(3000, Ordering::SeqCst);
            let exec = smol::future::or(
                async { Some(live.session.exec("true", None).await) },
                async {
                    smol::Timer::after(Duration::from_secs(30)).await;
                    None
                },
            )
            .await;
            match exec {
                Some(Ok(_)) => {}
                Some(Err(err)) => panic!("the request failed: {:#}", err),
                None => panic!("no answer in 30s"),
            }
            if let Some(SessionEvent::Error(err)) =
                next_event(&live.events, Duration::from_millis(500)).await
            {
                panic!("the session ended: {}", err);
            }
        });
    }
}
