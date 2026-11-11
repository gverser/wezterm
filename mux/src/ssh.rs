use crate::connui::ConnectionUI;
use crate::domain::{alloc_domain_id, Domain, DomainId, DomainState, WriterWrapper};
use crate::localpane::LocalPane;
use crate::pane::{alloc_pane_id, Pane, PaneId};
use crate::Mux;
use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use config::{Shell, SshBackend, SshDomain};
use filedescriptor::{poll, pollfd, socketpair, AsRawSocketDescriptor, FileDescriptor, POLLIN};
use portable_pty::cmdbuilder::CommandBuilder;
use portable_pty::{ChildKiller, ExitStatus, MasterPty, PtySize};
use smol::channel::{bounded, Receiver as AsyncReceiver};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termwiz::cell::{unicode_column_width, AttributeChange, Intensity};
use termwiz::input::{InputEvent, InputParser};
use termwiz::lineedit::*;
use termwiz::render::terminfo::TerminfoRenderer;
use termwiz::surface::{Change, LineAttribute};
use termwiz::terminal::{ScreenSize, Terminal, TerminalWaker};
use wezterm_ssh::{
    ConfigMap, HostVerificationFailed, Session, SessionEvent, SshChildProcess, SshPty,
};
use wezterm_term::TerminalSize;

#[derive(Default)]
struct PasswordPromptHost {
    history: BasicHistory,
    echo: bool,
}
impl LineEditorHost for PasswordPromptHost {
    fn history(&mut self) -> &mut dyn History {
        &mut self.history
    }

    fn highlight_line(&self, line: &str, cursor_position: usize) -> (Vec<OutputElement>, usize) {
        if self.echo {
            (vec![OutputElement::Text(line.to_string())], cursor_position)
        } else {
            // Rewrite the input so that we can obscure the password
            // characters when output to the terminal widget
            let placeholder = "🔑";
            let grapheme_count = unicode_column_width(line, None);
            let mut output = vec![];
            for _ in 0..grapheme_count {
                output.push(OutputElement::Text(placeholder.to_string()));
            }
            (
                output,
                unicode_column_width(placeholder, None) * cursor_position,
            )
        }
    }
}

/// Lets the embedder supply and keep a password for a [`RemoteSshDomain`],
/// so that a saved host can be connected to without typing it again.
///
/// Methods are called from the connection thread, not the GUI thread.
pub trait SshPasswordHelper: Send + Sync {
    /// The password to offer automatically, or None if there is none or it
    /// has already been rejected in this process.
    fn saved_password(&self) -> Option<String>;
    /// Whether any password is on file at all. Decides whether a password the
    /// user typed by hand is offered for saving or for updating.
    fn has_saved_password(&self) -> bool;
    /// The password from `saved_password` did not get the user in.
    fn saved_password_rejected(&self);
    /// The user typed this password, logged in with it, and agreed to keep it.
    fn save_password(&self, password: String);
}

/// Does this prompt ask for a password (as opposed to a one time code, a
/// key passphrase or a visible question)?
pub fn is_password_prompt(prompt: &wezterm_ssh::AuthenticationPrompt) -> bool {
    !prompt.echo && {
        let text = prompt.prompt.to_lowercase();
        text.contains("password") || text.contains("密码")
    }
}

/// What to ask the user once they are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOffer {
    Save(String),
    Update(String),
}

/// The decisions around filling in a saved password, kept apart from the
/// terminal UI so that they can be tested.
///
/// The saved password is tried exactly once. If another password prompt
/// follows it, or the connection fails before authenticating, it was wrong:
/// we never retry it automatically, because repeating a wrong password is how
/// accounts get locked.
pub(crate) struct PasswordAutofill {
    saved: Option<String>,
    has_saved: bool,
    auto_used: bool,
    typed: Option<String>,
    rejected: bool,
}

impl PasswordAutofill {
    pub fn new(saved: Option<String>, has_saved: bool) -> Self {
        Self {
            saved,
            has_saved,
            auto_used: false,
            typed: None,
            rejected: false,
        }
    }

    pub fn auto_answer(&mut self, prompt: &wezterm_ssh::AuthenticationPrompt) -> Option<String> {
        if !is_password_prompt(prompt) {
            return None;
        }
        if self.auto_used {
            self.rejected = true;
            return None;
        }
        let saved = self.saved.clone()?;
        self.auto_used = true;
        Some(saved)
    }

    pub fn record_typed(&mut self, prompt: &wezterm_ssh::AuthenticationPrompt, answer: &str) {
        if is_password_prompt(prompt) {
            self.typed = Some(answer.to_string());
        }
    }

    pub fn note_error(&mut self) {
        if self.auto_used {
            self.rejected = true;
        }
    }

    pub fn rejected(&self) -> bool {
        self.rejected
    }

    pub fn offer_after_success(&self) -> Option<SaveOffer> {
        let typed = self.typed.clone()?;
        Some(if self.has_saved {
            SaveOffer::Update(typed)
        } else {
            SaveOffer::Save(typed)
        })
    }
}

pub fn ssh_connect_with_ui(
    ssh_config: wezterm_ssh::ConfigMap,
    ui: &mut ConnectionUI,
) -> anyhow::Result<Session> {
    let cloned_ui = ui.clone();
    cloned_ui.run_and_log_error(move || {
        let remote_address = ssh_config
            .get("hostname")
            .expect("ssh config to always set hostname");
        ui.output_str(&format!("正在通过 SSH 连接 {} …\n", remote_address));
        let (session, events) = Session::connect(ssh_config.clone())?;

        while let Ok(event) = smol::block_on(events.recv()) {
            match event {
                SessionEvent::Banner(banner) => {
                    if let Some(banner) = banner {
                        ui.output_str(&format!("{}\n", banner));
                    }
                }
                SessionEvent::HostVerify(verify) => {
                    ui.output_str(&format!("{}\n", verify.message));
                    let ok = if let Ok(line) = ui.input("输入 y 或 n> ") {
                        match line.as_ref() {
                            "y" | "Y" | "yes" | "YES" => true,
                            "n" | "N" | "no" | "NO" | _ => false,
                        }
                    } else {
                        false
                    };
                    smol::block_on(verify.answer(ok)).context("send verify response")?;
                }
                SessionEvent::Authenticate(auth) => {
                    if !auth.username.is_empty() {
                        ui.output_str(&format!("以 {} 登录\n", auth.username));
                    }
                    if !auth.instructions.is_empty() {
                        ui.output_str(&format!("{}\n", auth.instructions));
                    }
                    let mut answers = vec![];
                    for prompt in &auth.prompts {
                        let mut prompt_lines = prompt.prompt.split('\n').collect::<Vec<_>>();
                        let editor_prompt = prompt_lines.pop().unwrap();
                        for line in &prompt_lines {
                            ui.output_str(&format!("{}\n", line));
                        }
                        let res = if prompt.echo {
                            ui.input(editor_prompt)
                        } else {
                            ui.password(editor_prompt)
                        };
                        if let Ok(line) = res {
                            answers.push(line);
                        } else {
                            anyhow::bail!("已取消登录");
                        }
                    }
                    smol::block_on(auth.answer(answers))?;
                }
                SessionEvent::HostVerificationFailed(failed) => {
                    let message = format_host_verification_for_terminal(failed);
                    ui.output(message);
                    anyhow::bail!("主机密钥验证失败");
                }
                SessionEvent::Error(err) => {
                    anyhow::bail!("连接失败：{}", err);
                }
                SessionEvent::Authenticated => return Ok(session),
            }
        }
        bail!("登录失败");
    })
}

fn format_host_verification_for_terminal(failed: HostVerificationFailed) -> Vec<Change> {
    vec![
        AttributeChange::Intensity(Intensity::Bold).into(),
        LineAttribute::DoubleHeightTopHalfLine.into(),
        Change::Text("远程主机的身份变了\r\n".to_string()),
        LineAttribute::DoubleHeightBottomHalfLine.into(),
        Change::Text("远程主机的身份变了\r\n".to_string()),
        Change::Text("可能有人在搞鬼！\r\n".to_string()),
        AttributeChange::Intensity(Intensity::Normal).into(),
        Change::Text("\r\n通常有两种可能：\r\n".to_string()),
        Change::Text(" 1. 有人正在窃听（中间人攻击）\r\n".to_string()),
        Change::Text(" 2. 管理员更换了这台主机的密钥\r\n".to_string()),
        Change::Text("\r\n".to_string()),
        AttributeChange::Intensity(Intensity::Bold).into(),
        Change::Text("请先和系统管理员确认，再决定怎么做！\r\n".to_string()),
        AttributeChange::Intensity(Intensity::Normal).into(),
        Change::Text("\r\n".to_string()),
        match failed.file {
            Some(file) => Change::Text(format!(
                "主机是 {}，它现在的指纹是\r\n{}\r\n\
                如果管理员确认密钥确实换了，可以从\r\n\
                {} 里删掉这台主机的旧条目，再重新连接。\r\n",
                failed.remote_address,
                failed.key,
                file.display(),
            )),
            None => Change::Text(format!(
                "主机是 {}，它现在的指纹是\r\n{}\r\n",
                failed.remote_address, failed.key
            )),
        },
    ]
}

/// Represents a connection to remote host via ssh.
/// The domain is created with the ssh config prior to making the
/// connection.  The connection is established by the first spawn()
/// call.
/// In order to show the authentication dialog inline in that spawned
/// pane, we play some tricks with wrapped versions of the pty, child
/// and the reader and writer instances so that we can inject the
/// interactive setup.  The bulk of that is driven by `connect_ssh_session`.
pub struct RemoteSshDomain {
    session: Mutex<Option<SessionSlot>>,
    /// Bumped each time a new connection replaces the previous one.
    generation: AtomicU64,
    password_helper: Option<Arc<dyn SshPasswordHelper>>,
    dom: SshDomain,
    id: DomainId,
    name: String,
}

/// The current connection, and whether it has finished authenticating.
///
/// The session is stored before authentication so that a second pane can
/// queue a pty request on it, but callers outside this module only get to
/// see it once `authenticated` is set.
struct SessionSlot {
    session: Session,
    generation: u64,
    authenticated: Arc<AtomicBool>,
}

pub fn ssh_domain_to_ssh_config(ssh_dom: &SshDomain) -> anyhow::Result<ConfigMap> {
    let mut ssh_config = wezterm_ssh::Config::new();
    ssh_config.add_default_config_files();

    let (remote_host_name, port) = {
        let parts: Vec<&str> = ssh_dom.remote_address.split(':').collect();

        if parts.len() == 2 {
            (parts[0], Some(parts[1].parse::<u16>()?))
        } else {
            (ssh_dom.remote_address.as_str(), None)
        }
    };

    let mut ssh_config = ssh_config.for_host(&remote_host_name);
    ssh_config.insert(
        "wezterm_ssh_backend".to_string(),
        match ssh_dom
            .ssh_backend
            .unwrap_or_else(|| config::configuration().ssh_backend)
        {
            SshBackend::Ssh2 => "ssh2",
            SshBackend::LibSsh => "libssh",
        }
        .to_string(),
    );
    for (k, v) in &ssh_dom.ssh_option {
        ssh_config.insert(k.to_string(), v.to_string());
    }

    if let Some(username) = &ssh_dom.username {
        ssh_config.insert("user".to_string(), username.to_string());
    }
    if let Some(port) = port {
        ssh_config.insert("port".to_string(), port.to_string());
    }
    if ssh_dom.no_agent_auth {
        ssh_config.insert("identitiesonly".to_string(), "yes".to_string());
    }
    if let Some("true") = ssh_config.get("wezterm_ssh_verbose").map(|s| s.as_str()) {
        log::info!("Using ssh config: {ssh_config:#?}");
    }
    Ok(ssh_config)
}

impl RemoteSshDomain {
    pub fn with_ssh_domain(dom: &SshDomain) -> anyhow::Result<Self> {
        Self::with_ssh_domain_and_helper(dom, None)
    }

    /// As [`Self::with_ssh_domain`], with a helper that can fill in and keep
    /// the password.
    pub fn with_ssh_domain_and_helper(
        dom: &SshDomain,
        password_helper: Option<Arc<dyn SshPasswordHelper>>,
    ) -> anyhow::Result<Self> {
        let id = alloc_domain_id();
        Ok(Self {
            id,
            name: dom.name.clone(),
            session: Mutex::new(None),
            generation: AtomicU64::new(0),
            password_helper,
            dom: dom.clone(),
        })
    }

    /// The live connection, once it has authenticated, along with a counter
    /// that changes whenever the connection is replaced by a new one.
    ///
    /// Channels opened on it (exec, sftp) share the connection with the
    /// panes. Note that a `Session` only keeps the connection alive while it
    /// has channels open; sftp alone does not count.
    /// The counter `authenticated_session` would return, without cloning the
    /// session: dropping a clone wakes the session thread, which is not
    /// something to do on every paint just to find nothing has changed.
    pub fn authenticated_generation(&self) -> Option<u64> {
        let slot = self.session.lock().unwrap();
        let slot = slot.as_ref()?;
        if slot.authenticated.load(Ordering::SeqCst) {
            Some(slot.generation)
        } else {
            None
        }
    }

    pub fn authenticated_session(&self) -> Option<(u64, Session)> {
        let slot = self.session.lock().unwrap();
        let slot = slot.as_ref()?;
        if slot.authenticated.load(Ordering::SeqCst) {
            Some((slot.generation, slot.session.clone()))
        } else {
            None
        }
    }

    pub fn ssh_config(&self) -> anyhow::Result<ConfigMap> {
        ssh_domain_to_ssh_config(&self.dom)
    }

    fn build_command(
        &self,
        pane_id: PaneId,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
    ) -> anyhow::Result<(Option<String>, HashMap<String, String>)> {
        let config = config::configuration();
        let cmd = match command {
            Some(mut cmd) => {
                config.apply_cmd_defaults(&mut cmd, self.dom.default_prog.as_ref(), None);
                cmd
            }
            None => config.build_prog(None, self.dom.default_prog.as_ref(), None)?,
        };
        let mut env: HashMap<String, String> = cmd
            .iter_extra_env_as_str()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        // FIXME: this isn't useful without a way to talk to the remote mux.
        // One option is to forward the mux via unix domain, another is to
        // embed the mux protocol in an escape sequence and just use the
        // existing terminal connection
        env.insert("WEZTERM_REMOTE_PANE".to_string(), pane_id.to_string());

        fn build_env_command(
            dir: Option<String>,
            cmd: &CommandBuilder,
            env: &HashMap<String, String>,
        ) -> anyhow::Result<String> {
            // "Soft" chdir: if it doesn't exist then it doesn't matter
            let cd_cmd = if let Some(dir) = dir {
                format!("cd {};", shell_words::quote(&dir))
            } else if let Some(dir) = cmd.get_cwd() {
                let dir = dir.to_str().context("converting cwd to string")?;
                format!("cd {};", shell_words::quote(&dir))
            } else {
                String::new()
            };

            let mut env_cmd = vec!["env".to_string()];

            for (k, v) in env {
                env_cmd.push(format!("{}={}", k, v));
            }

            let cmd = if cmd.is_default_prog() {
                // We'd like to spawn a login shell, but since we are invoking env
                // we end up in a regular shell.
                // This guff tries to find a reasonably portable way to execute
                // the shell as a login shell.
                // Per: <https://unix.stackexchange.com/a/666850/123914>
                // the most portable way is to use perl, but in case perl is not
                // installed, zsh, bash and ksh all support `exec -a`.
                // Other shells may support `exec -a` but there isn't a simple
                // way to test for them, so we assume that if we have one of those
                // three that we can use it, otherwise we fall back to just running
                // the shell directly.
                let login_shell = "command -v perl > /dev/null && \
                  exec perl -e 'use File::Basename; $shell = basename($ENV{SHELL}); exec {$ENV{SHELL}} \"-$shell\"'; \
                  case \"$SHELL\" in */zsh|*/bash|*/ksh ) exec -a \"-$(basename $SHELL)\" $SHELL ;; esac ; \
                  exec $SHELL";

                format!("$SHELL -c {}", shell_words::quote(login_shell))
            } else {
                cmd.as_unix_command_line()?
            };

            Ok(cd_cmd + &shell_words::join(env_cmd) + " " + &cmd)
        }

        let command_line = match (cmd.is_default_prog(), self.dom.assume_shell, command_dir) {
            (_, Shell::Posix, dir) => Some(build_env_command(dir, &cmd, &env)?),
            (true, _, _) => None,
            (false, _, _) => Some(cmd.as_unix_command_line()?),
        };

        Ok((command_line, env))
    }

    async fn start_new_session(
        &self,
        command_line: Option<String>,
        env: HashMap<String, String>,
        size: TerminalSize,
    ) -> anyhow::Result<StartNewSessionResult> {
        let (session, events) = Session::connect(self.ssh_config().context("obtain ssh config")?)
            .context("connect to ssh server")?;
        let authenticated = Arc::new(AtomicBool::new(false));
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.session.lock().unwrap().replace(SessionSlot {
            session: session.clone(),
            generation,
            authenticated: Arc::clone(&authenticated),
        });
        let password_helper = self.password_helper.clone();

        // We get to establish the session!
        //
        // Since we want spawn to return the Pane in which
        // we'll carry out interactive auth, we generate
        // some shim/wrapper versions of the pty, child
        // and reader/writer.

        let (stdout_read, stdout_write) = socketpair()?;
        let (reader_tx, reader_rx) = channel();
        let (stdin_read, stdin_write) = socketpair()?;
        let (writer_tx, writer_rx) = channel();

        let pty_reader = PtyReader {
            reader: Box::new(stdout_read),
            rx: reader_rx,
        };

        let pty_writer = PtyWriter {
            writer: Box::new(stdin_write),
            rx: writer_rx,
        };
        let writer = Box::new(pty_writer);

        let (child_tx, child_rx) = channel();

        let child = Box::new(WrappedSshChild {
            status: None,
            rx: child_rx,
            exited: None,
            killer: WrappedSshChildKiller {
                inner: Arc::new(Mutex::new(KillerInner {
                    killer: None,
                    pending_kill: false,
                })),
            },
        });

        let (pty_tx, pty_rx) = channel();

        let size = Arc::new(Mutex::new(size));

        let pty = Box::new(WrappedSshPty {
            inner: RefCell::new(WrappedSshPtyInner::Connecting {
                size: Arc::clone(&size),
                reader: Some(pty_reader),
                connected: pty_rx,
            }),
        });

        // And with those created, we can now spawn a new thread
        // to perform the blocking (from its perspective) terminal
        // UI to carry out any authentication.
        let mut stdout_write = BufWriter::new(stdout_write);
        std::thread::spawn(move || {
            if let Err(err) = connect_ssh_session(
                session,
                events,
                stdin_read,
                writer_tx,
                &mut stdout_write,
                reader_tx,
                child_tx,
                pty_tx,
                size,
                command_line,
                env,
                password_helper,
                authenticated,
            ) {
                let _ = write!(
                    stdout_write,
                    "\r\n连接失败：{}\r\n",
                    wezterm_ssh::describe_error(&err)
                );
                log::error!("Failed to connect ssh: {:#}", err);
            }
            let _ = stdout_write.flush();
        });

        Ok(StartNewSessionResult { pty, child, writer })
    }
}

struct StartNewSessionResult {
    pty: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    writer: BoxedWriter,
}

/// Carry out the authentication process and create the initial pty.
fn connect_ssh_session(
    session: Session,
    events: smol::channel::Receiver<SessionEvent>,
    mut stdin_read: FileDescriptor,
    stdin_tx: Sender<BoxedWriter>,
    stdout_write: &mut BufWriter<FileDescriptor>,
    stdout_tx: Sender<BoxedReader>,
    child_tx: Sender<SshChildProcess>,
    pty_tx: Sender<SshPty>,
    size: Arc<Mutex<TerminalSize>>,
    command_line: Option<String>,
    env: HashMap<String, String>,
    password_helper: Option<Arc<dyn SshPasswordHelper>>,
    authenticated: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    struct StdoutShim<'a> {
        size: Arc<Mutex<TerminalSize>>,
        stdout: &'a mut BufWriter<FileDescriptor>,
    }

    impl<'a> Write for StdoutShim<'a> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.stdout.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.stdout.flush()
        }
    }

    impl<'a> termwiz::render::RenderTty for StdoutShim<'a> {
        fn get_size_in_cells(&mut self) -> termwiz::Result<(usize, usize)> {
            let size = *self.size.lock().unwrap();
            Ok((size.cols as _, size.rows as _))
        }
    }

    /// a termwiz Terminal for use with the line editor
    struct TerminalShim<'a> {
        stdout: &'a mut StdoutShim<'a>,
        stdin: &'a mut FileDescriptor,
        size: Arc<Mutex<TerminalSize>>,
        renderer: TerminfoRenderer,
        parser: InputParser,
        input_queue: VecDeque<InputEvent>,
    }

    impl<'a> termwiz::terminal::Terminal for TerminalShim<'a> {
        fn set_raw_mode(&mut self) -> termwiz::Result<()> {
            use termwiz::escape::csi::{DecPrivateMode, DecPrivateModeCode, Mode, CSI};

            macro_rules! decset {
                ($variant:ident) => {
                    write!(
                        self.stdout,
                        "{}",
                        CSI::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
                            DecPrivateModeCode::$variant
                        )))
                    )?;
                };
            }

            decset!(BracketedPaste);
            self.flush()?;

            Ok(())
        }

        fn flush(&mut self) -> termwiz::Result<()> {
            self.stdout.flush()?;
            Ok(())
        }

        fn set_cooked_mode(&mut self) -> termwiz::Result<()> {
            Ok(())
        }

        fn enter_alternate_screen(&mut self) -> termwiz::Result<()> {
            termwiz::bail!("TerminalShim has no alt screen");
        }

        fn exit_alternate_screen(&mut self) -> termwiz::Result<()> {
            termwiz::bail!("TerminalShim has no alt screen");
        }

        fn get_screen_size(&mut self) -> termwiz::Result<ScreenSize> {
            let size = *self.size.lock().unwrap();
            Ok(ScreenSize {
                cols: size.cols as _,
                rows: size.rows as _,
                xpixel: size.pixel_width as _,
                ypixel: size.pixel_height as _,
            })
        }

        fn set_screen_size(&mut self, _size: ScreenSize) -> termwiz::Result<()> {
            termwiz::bail!("TerminalShim cannot set screen size");
        }

        fn render(&mut self, changes: &[Change]) -> termwiz::Result<()> {
            self.renderer.render_to(changes, self.stdout)?;
            Ok(())
        }

        fn poll_input(&mut self, wait: Option<Duration>) -> termwiz::Result<Option<InputEvent>> {
            if let Some(event) = self.input_queue.pop_front() {
                return Ok(Some(event));
            }

            let deadline = wait.map(|d| Instant::now() + d);
            let starting_size = *self.size.lock().unwrap();

            self.stdin.set_non_blocking(true)?;

            loop {
                if let Some(deadline) = deadline.as_ref() {
                    if Instant::now() >= *deadline {
                        return Ok(None);
                    }
                }
                let mut pfd = [pollfd {
                    fd: self.stdin.as_socket_descriptor(),
                    events: POLLIN,
                    revents: 0,
                }];

                if let Ok(1) = poll(&mut pfd, Some(Duration::from_millis(200))) {
                    let mut buf = [0u8; 64];
                    let n = self.stdin.read(&mut buf)?;
                    let input_queue = &mut self.input_queue;
                    self.parser
                        .parse(&buf[0..n], |evt| input_queue.push_back(evt), n == buf.len());
                    return Ok(self.input_queue.pop_front());
                } else {
                    let size = *self.size.lock().unwrap();
                    if starting_size != size {
                        return Ok(Some(InputEvent::Resized {
                            cols: size.cols as usize,
                            rows: size.rows as usize,
                        }));
                    }
                }
            }
        }

        fn waker(&self) -> TerminalWaker {
            // TODO: TerminalWaker assumes that we're a SystemTerminal but that
            // isn't the case here.
            panic!("TerminalShim::waker called!?");
        }
    }

    let renderer = termwiz_funcs::new_wezterm_terminfo_renderer();
    let mut shim = TerminalShim {
        stdout: &mut StdoutShim {
            stdout: stdout_write,
            size: Arc::clone(&size),
        },
        size: Arc::clone(&size),
        renderer,
        stdin: &mut stdin_read,
        parser: InputParser::new(),
        input_queue: VecDeque::new(),
    };

    impl<'a> TerminalShim<'a> {
        fn output_line(&mut self, s: &str) -> termwiz::Result<()> {
            let mut s = s.replace("\n", "\r\n");
            s.push_str("\r\n");
            self.render(&[Change::Text(s)])
        }
    }

    let mut autofill = PasswordAutofill::new(
        password_helper.as_ref().and_then(|h| h.saved_password()),
        password_helper
            .as_ref()
            .map(|h| h.has_saved_password())
            .unwrap_or(false),
    );
    let mut reported_rejection = false;
    let mut report_rejection =
        |autofill: &PasswordAutofill, shim: &mut TerminalShim| -> termwiz::Result<()> {
            if autofill.rejected() && !reported_rejection {
                reported_rejection = true;
                if let Some(helper) = &password_helper {
                    helper.saved_password_rejected();
                }
                shim.output_line("已存的密码被服务器拒绝，下次连接将改为手动输入。")?;
            }
            Ok(())
        };

    // Process authentication related events
    while let Ok(event) = smol::block_on(events.recv()) {
        match event {
            SessionEvent::Banner(banner) => {
                if let Some(banner) = banner {
                    shim.output_line(&banner)?;
                }
            }
            SessionEvent::HostVerify(verify) => {
                shim.output_line(&verify.message)?;
                let mut editor = LineEditor::new(&mut shim);
                let mut host = PasswordPromptHost::default();
                host.echo = true;
                editor.set_prompt("输入 y 或 n> ");
                let ok = if let Some(line) = editor.read_line(&mut host)? {
                    match line.as_ref() {
                        "y" | "Y" | "yes" | "YES" => true,
                        "n" | "N" | "no" | "NO" | _ => false,
                    }
                } else {
                    false
                };
                smol::block_on(verify.answer(ok)).context("send verify response")?;
            }
            SessionEvent::Authenticate(auth) => {
                if !auth.username.is_empty() {
                    shim.output_line(&format!("以 {} 登录", auth.username))?;
                }
                if !auth.instructions.is_empty() {
                    shim.output_line(&auth.instructions)?;
                }
                let mut answers = vec![];
                for prompt in &auth.prompts {
                    if let Some(saved) = autofill.auto_answer(prompt) {
                        // Answer silently: the password is on file.
                        answers.push(saved);
                        continue;
                    }
                    report_rejection(&autofill, &mut shim)?;
                    let mut prompt_lines = prompt.prompt.split('\n').collect::<Vec<_>>();
                    let editor_prompt = prompt_lines.pop().unwrap();
                    for line in &prompt_lines {
                        shim.output_line(line)?;
                    }
                    let mut editor = LineEditor::new(&mut shim);
                    let mut host = PasswordPromptHost::default();
                    editor.set_prompt(editor_prompt);
                    host.echo = prompt.echo;
                    if let Some(line) = editor.read_line(&mut host)? {
                        autofill.record_typed(prompt, &line);
                        answers.push(line);
                    } else {
                        anyhow::bail!("已取消登录");
                    }
                }
                smol::block_on(auth.answer(answers))?;
            }
            SessionEvent::Error(err) => {
                shim.output_line(&format!("连接失败：{}", err))?;
                autofill.note_error();
                report_rejection(&autofill, &mut shim)?;
            }
            SessionEvent::HostVerificationFailed(failed) => {
                let message = format_host_verification_for_terminal(failed);
                shim.render(&message)?;
            }
            SessionEvent::Authenticated => {
                // Taken out first: a lock in the match's scrutinee would be
                // held through its arms, and the question asked in the Ok
                // arm below reads the size through `shim`, which would then
                // wait on this thread's own lock.
                let pty_size = crate::terminal_size_to_pty_size(*size.lock().unwrap())?;

                // Our session has been authenticated: we can now
                // set up the real pty for the pane
                match smol::block_on(session.request_pty(
                    &config::configuration().term,
                    pty_size,
                    command_line.as_ref().map(|s| s.as_str()),
                    Some(env),
                )) {
                    Err(err) => {
                        shim.output_line(&format!(
                            "无法启动远程命令：{}",
                            wezterm_ssh::describe_error(&err)
                        ))?;
                        break;
                    }
                    Ok((pty, child)) => {
                        // Only now may others use the session. The session
                        // thread stops once it has no channels and any clone
                        // of the Session has been dropped, so a clone taken
                        // and released before this pty existed -- by the
                        // file page, say, while the question below waits for
                        // an answer -- would take the connection down.
                        authenticated.store(true, Ordering::SeqCst);

                        // Asked with the shell already running behind it; its
                        // output waits in the channel until the pane is wired
                        // up below.
                        if let (Some(helper), Some(offer)) =
                            (&password_helper, autofill.offer_after_success())
                        {
                            let (question, password) = match offer {
                                SaveOffer::Save(pw) => ("保存密码，下次自动登录？(y/N) ", pw),
                                SaveOffer::Update(pw) => ("更新已存的密码？(y/N) ", pw),
                            };
                            let mut editor = LineEditor::new(&mut shim);
                            let mut host = PasswordPromptHost::default();
                            host.echo = true;
                            editor.set_prompt(question);
                            if let Some(line) = editor.read_line(&mut host)? {
                                if matches!(line.trim(), "y" | "Y" | "yes" | "YES") {
                                    helper.save_password(password);
                                }
                            }
                        }

                        drop(shim);

                        // Obtain the real stdin/stdout for the pty
                        let reader = pty.try_clone_reader()?;
                        let writer = pty.take_writer()?;

                        // And send them to the wrapped reader/writer
                        stdin_tx
                            .send(Box::new(writer))
                            .map_err(|e| anyhow!("{:#}", e))?;
                        stdout_tx
                            .send(Box::new(reader))
                            .map_err(|e| anyhow!("{:#}", e))?;

                        // Likewise, send the real pty and child to
                        // the wrappers
                        pty_tx.send(pty)?;
                        child_tx.send(child)?;

                        // Now when we return, our stdin_read and
                        // stdout_write will close and that will cause
                        // the PtyReader and PtyWriter to recv the
                        // the new reader/writer above and continue.
                        //
                        // The pty and child will be picked up when
                        // they are next polled or resized.

                        return Ok(());
                    }
                }
            }
        }
    }

    Ok(())
}

#[async_trait(?Send)]
impl Domain for RemoteSshDomain {
    async fn spawn_pane(
        &self,
        size: TerminalSize,
        command: Option<CommandBuilder>,
        command_dir: Option<String>,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let pane_id = alloc_pane_id();

        let (command_line, env) = self
            .build_command(pane_id, command, command_dir)
            .context("build_command")?;

        // This needs to be separate from the if let block below in order
        // for the lock to be released at the appropriate time
        let mut session: Option<Session> = self
            .session
            .lock()
            .unwrap()
            .as_ref()
            .map(|slot| slot.session.clone());

        let StartNewSessionResult { pty, child, writer } = if let Some(session) = session.take() {
            match session
                .request_pty(
                    &config::configuration().term,
                    crate::terminal_size_to_pty_size(size)
                        .context("compute pty size from terminal size")?,
                    command_line.as_ref().map(|s| s.as_str()),
                    Some(env.clone()),
                )
                .await
                .context("request ssh pty")
            {
                Ok((concrete_pty, concrete_child)) => {
                    let pty = Box::new(concrete_pty);
                    let child = Box::new(concrete_child);
                    let writer = Box::new(pty.take_writer().context("take writer from pty")?);

                    StartNewSessionResult { pty, child, writer }
                }
                Err(err) => {
                    if err
                        .root_cause()
                        .downcast_ref::<wezterm_ssh::DeadSession>()
                        .is_some()
                    {
                        // Session died (perhaps they closed the initial tab?)
                        // So we'll try making a new one
                        self.start_new_session(command_line, env, size).await?
                    } else {
                        log::error!("{err:#?}");
                        return Err(err);
                    }
                }
            }
        } else {
            self.start_new_session(command_line, env, size).await?
        };

        // Wrap up the pty etc. in a LocalPane.  That allows for
        // eg: tmux integration to be tunnelled via the remote
        // session without duplicating a lot of logic over here.

        let writer = WriterWrapper::new(writer);

        let terminal = wezterm_term::Terminal::new(
            size,
            std::sync::Arc::new(config::TermConfig::new()),
            "WezTerm",
            config::wezterm_version(),
            Box::new(writer.clone()),
        );

        let pane: Arc<dyn Pane> = Arc::new(LocalPane::new(
            pane_id,
            terminal,
            child,
            pty,
            Box::new(writer),
            self.id,
            "RemoteSshDomain".to_string(),
        ));
        let mux = Mux::get();
        mux.add_pane(&pane)?;

        Ok(pane)
    }

    fn domain_id(&self) -> DomainId {
        self.id
    }

    fn domain_name(&self) -> &str {
        &self.name
    }

    async fn attach(&self, _window_id: Option<crate::WindowId>) -> anyhow::Result<()> {
        Ok(())
    }

    fn detachable(&self) -> bool {
        false
    }

    fn detach(&self) -> anyhow::Result<()> {
        bail!("detach not implemented for RemoteSshDomain");
    }

    fn state(&self) -> DomainState {
        // Just pretend that we are always attached, as we don't
        // have a defined attach operation that is distinct from
        // a spawn.
        DomainState::Attached
    }
}

#[derive(Debug)]
struct KillerInner {
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
    /// If we haven't populated `killer` by the time someone has called
    /// `kill`, then we use this to remember to kill as soon as we recv
    /// the child process.
    pending_kill: bool,
}

#[derive(Debug, Clone)]
struct WrappedSshChildKiller {
    inner: Arc<Mutex<KillerInner>>,
}

#[derive(Debug)]
pub(crate) struct WrappedSshChild {
    status: Option<AsyncReceiver<ExitStatus>>,
    rx: Receiver<SshChildProcess>,
    exited: Option<ExitStatus>,
    killer: WrappedSshChildKiller,
}

impl WrappedSshChild {
    fn check_connected(&mut self) {
        if self.status.is_none() {
            match self.rx.try_recv() {
                Ok(c) => {
                    self.got_child(c);
                }
                Err(TryRecvError::Empty) => {}
                Err(err) => {
                    log::debug!("WrappedSshChild::check_connected err: {:#?}", err);
                    self.exited
                        .replace(ExitStatus::with_signal(wezterm_ssh::CONNECT_FAILED));
                }
            }
        }
    }

    fn got_child(&mut self, mut child: SshChildProcess) {
        {
            let mut killer = self.killer.inner.lock().unwrap();
            killer.killer.replace(child.clone_killer());
            if killer.pending_kill {
                let _ = child.kill().ok();
            }
        }

        let (tx, rx) = bounded(1);
        promise::spawn::spawn_into_main_thread(async move {
            if let Ok(status) = child.async_wait().await {
                tx.send(status).await.ok();
                let mux = Mux::get();
                mux.prune_dead_windows();
            }
        })
        .detach();
        self.status.replace(rx);
    }
}

impl portable_pty::Child for WrappedSshChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exited.as_ref() {
            return Ok(Some(status.clone()));
        }

        self.check_connected();
        if let Some(status) = self.exited.as_ref() {
            // The connection never came up.
            return Ok(Some(status.clone()));
        }

        if let Some(rx) = self.status.as_mut() {
            match rx.try_recv() {
                Ok(status) => {
                    self.exited.replace(status.clone());
                    Ok(Some(status))
                }
                Err(smol::channel::TryRecvError::Empty) => Ok(None),
                Err(err) => {
                    log::debug!("WrappedSshChild::try_wait err: {:#?}", err);
                    let status = ExitStatus::with_exit_code(1);
                    self.exited.replace(status.clone());
                    Ok(Some(status))
                }
            }
        } else {
            Ok(None)
        }
    }

    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        if let Some(status) = self.exited.as_ref() {
            return Ok(status.clone());
        }

        if self.status.is_none() {
            match smol::block_on(async { self.rx.recv() }) {
                Ok(c) => {
                    self.got_child(c);
                }
                Err(err) => {
                    log::debug!("WrappedSshChild err: {:#?}", err);
                    let status = ExitStatus::with_signal(wezterm_ssh::CONNECT_FAILED);
                    self.exited.replace(status.clone());
                    return Ok(status);
                }
            }
        }

        let rx = self.status.as_mut().unwrap();
        match smol::block_on(rx.recv()) {
            Ok(status) => {
                self.exited.replace(status.clone());
                Ok(status)
            }
            Err(err) => {
                log::error!("WrappedSshChild err: {:#?}", err);
                let status = ExitStatus::with_exit_code(1);
                self.exited.replace(status.clone());
                Ok(status)
            }
        }
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

impl ChildKiller for WrappedSshChild {
    fn kill(&mut self) -> std::io::Result<()> {
        let mut killer = self.killer.inner.lock().unwrap();
        if let Some(killer) = killer.killer.as_mut() {
            killer.kill()
        } else {
            killer.pending_kill = true;
            Ok(())
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.killer.clone())
    }
}

impl ChildKiller for WrappedSshChildKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        let mut killer = self.inner.lock().unwrap();
        if let Some(killer) = killer.killer.as_mut() {
            killer.kill()
        } else {
            killer.pending_kill = true;
            Ok(())
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

type BoxedReader = Box<dyn Read + Send + 'static>;
type BoxedWriter = Box<dyn Write + Send + 'static>;

pub(crate) struct WrappedSshPty {
    inner: RefCell<WrappedSshPtyInner>,
}

impl WrappedSshPty {
    pub fn is_connecting(&mut self) -> bool {
        self.inner.borrow_mut().is_connecting()
    }
}

enum WrappedSshPtyInner {
    Connecting {
        reader: Option<PtyReader>,
        connected: Receiver<SshPty>,
        size: Arc<Mutex<TerminalSize>>,
    },
    Connected {
        reader: Option<PtyReader>,
        pty: SshPty,
    },
}

struct PtyReader {
    reader: BoxedReader,
    rx: Receiver<BoxedReader>,
}

struct PtyWriter {
    writer: BoxedWriter,
    rx: Receiver<BoxedWriter>,
}

impl WrappedSshPtyInner {
    fn check_connected(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Connecting {
                reader,
                connected,
                size,
                ..
            } => {
                if let Ok(pty) = connected.try_recv() {
                    let res = pty.resize(crate::terminal_size_to_pty_size(*size.lock().unwrap())?);
                    *self = Self::Connected {
                        pty,
                        reader: reader.take(),
                    };
                    res
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    fn is_connecting(&mut self) -> bool {
        self.check_connected().ok();
        match self {
            Self::Connecting { .. } => true,
            Self::Connected { .. } => false,
        }
    }
}

impl portable_pty::MasterPty for WrappedSshPty {
    fn resize(&self, new_size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.borrow_mut();
        match &mut *inner {
            WrappedSshPtyInner::Connecting { ref mut size, .. } => {
                {
                    let mut size = size.lock().unwrap();
                    size.cols = new_size.cols as usize;
                    size.rows = new_size.rows as usize;
                    size.pixel_height = new_size.pixel_height as usize;
                    size.pixel_width = new_size.pixel_width as usize;
                }
                inner.check_connected()
            }
            WrappedSshPtyInner::Connected { pty, .. } => pty.resize(new_size),
        }
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        let mut inner = self.inner.borrow_mut();
        match &*inner {
            WrappedSshPtyInner::Connecting { size, .. } => {
                let size = crate::terminal_size_to_pty_size(*size.lock().unwrap())?;
                inner.check_connected()?;
                Ok(size)
            }
            WrappedSshPtyInner::Connected { pty, .. } => pty.get_size(),
        }
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send + 'static>> {
        let mut inner = self.inner.borrow_mut();
        inner.check_connected()?;
        match &mut *inner {
            WrappedSshPtyInner::Connected { ref mut reader, .. }
            | WrappedSshPtyInner::Connecting { ref mut reader, .. } => match reader.take() {
                Some(r) => Ok(Box::new(r)),
                None => anyhow::bail!("reader already taken"),
            },
        }
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send + 'static>> {
        anyhow::bail!("writer must be created during bootstrap");
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        let mut inner = self.inner.borrow_mut();
        let _ = inner.check_connected();
        None
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

impl std::io::Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Check for a new writer first: on Windows, the socket
        // will let us successfully write a byte to a disconnected
        // socket and we won't discover the issue until we write
        // the next byte.
        // <https://github.com/wezterm/wezterm/issues/771>
        if let Ok(writer) = self.rx.try_recv() {
            self.writer = writer;
        }
        self.writer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.writer.flush() {
            Ok(_) => Ok(()),
            res => match self.rx.recv() {
                Ok(writer) => {
                    self.writer = writer;
                    self.writer.flush()
                }
                _ => res,
            },
        }
    }
}

impl std::io::Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.reader.read(buf) {
            Ok(len) if len > 0 => Ok(len),
            res => match self.rx.recv() {
                Ok(reader) => {
                    self.reader = reader;
                    self.reader.read(buf)
                }
                _ => res,
            },
        }
    }
}

#[cfg(test)]
mod autofill_test {
    use super::*;
    use wezterm_ssh::AuthenticationPrompt;

    #[test]
    fn the_prompts_wezterm_asks_in_chinese_are_still_told_apart() {
        let p = |text: &str| AuthenticationPrompt {
            prompt: text.to_string(),
            echo: false,
        };
        assert!(is_password_prompt(&p("ty@127.0.0.1 的密码：")));
        assert!(is_password_prompt(&p("密码：")));
        // A key's passphrase is never filled in with the saved password.
        assert!(!is_password_prompt(&p(
            "解开 C:\\k\\id 的口令（ty@h）：\n> "
        )));
    }

    fn password() -> AuthenticationPrompt {
        AuthenticationPrompt {
            prompt: "Password: ".to_string(),
            echo: false,
        }
    }

    #[test]
    fn saved_password_is_offered_once() {
        let mut fill = PasswordAutofill::new(Some("secret".to_string()), true);
        assert_eq!(fill.auto_answer(&password()), Some("secret".to_string()));
        assert!(!fill.rejected());
        assert_eq!(fill.auto_answer(&password()), None);
        assert!(fill.rejected());
    }

    #[test]
    fn non_password_prompt_is_never_autofilled() {
        let mut fill = PasswordAutofill::new(Some("secret".to_string()), true);
        let code = AuthenticationPrompt {
            prompt: "Verification code: ".to_string(),
            echo: false,
        };
        assert_eq!(fill.auto_answer(&code), None);
        assert!(!fill.rejected());
    }

    #[test]
    fn echoed_prompt_is_not_a_password_prompt() {
        let prompt = AuthenticationPrompt {
            prompt: "Password hint (shown): ".to_string(),
            echo: true,
        };
        assert!(!is_password_prompt(&prompt));
    }

    #[test]
    fn chinese_password_prompt_is_recognized() {
        let prompt = AuthenticationPrompt {
            prompt: "密码：".to_string(),
            echo: false,
        };
        assert!(is_password_prompt(&prompt));
        let prompt = AuthenticationPrompt {
            prompt: "tester@host's PASSWORD:".to_string(),
            echo: false,
        };
        assert!(is_password_prompt(&prompt));
    }

    #[test]
    fn error_after_autofill_marks_rejected() {
        let mut fill = PasswordAutofill::new(Some("secret".to_string()), true);
        fill.auto_answer(&password());
        fill.note_error();
        assert!(fill.rejected());
    }

    #[test]
    fn error_without_autofill_is_not_a_rejection() {
        let mut fill = PasswordAutofill::new(None, false);
        fill.note_error();
        assert!(!fill.rejected());
    }

    #[test]
    fn typed_password_offers_save_when_none_saved() {
        let mut fill = PasswordAutofill::new(None, false);
        assert_eq!(fill.auto_answer(&password()), None);
        fill.record_typed(&password(), "pw");
        assert_eq!(
            fill.offer_after_success(),
            Some(SaveOffer::Save("pw".to_string()))
        );
    }

    #[test]
    fn typed_password_offers_update_after_rejection() {
        let mut fill = PasswordAutofill::new(Some("old".to_string()), true);
        fill.auto_answer(&password());
        assert_eq!(fill.auto_answer(&password()), None);
        fill.record_typed(&password(), "new");
        assert_eq!(
            fill.offer_after_success(),
            Some(SaveOffer::Update("new".to_string()))
        );
    }

    #[test]
    fn rejected_earlier_still_offers_update() {
        // The helper withholds a password it already knows is wrong, but one
        // is still on file, so the question is "update", not "save".
        let mut fill = PasswordAutofill::new(None, true);
        fill.record_typed(&password(), "new");
        assert_eq!(
            fill.offer_after_success(),
            Some(SaveOffer::Update("new".to_string()))
        );
    }

    #[test]
    fn autofill_success_offers_nothing() {
        let mut fill = PasswordAutofill::new(Some("secret".to_string()), true);
        fill.auto_answer(&password());
        assert_eq!(fill.offer_after_success(), None);
    }

    #[test]
    fn typed_non_password_is_not_offered() {
        let mut fill = PasswordAutofill::new(None, false);
        let code = AuthenticationPrompt {
            prompt: "Verification code: ".to_string(),
            echo: false,
        };
        fill.record_typed(&code, "123456");
        assert_eq!(fill.offer_after_success(), None);
    }

    #[test]
    fn no_saved_password_means_no_autofill() {
        let mut fill = PasswordAutofill::new(None, false);
        assert_eq!(fill.auto_answer(&password()), None);
        assert!(!fill.rejected());
    }
}

#[cfg(test)]
mod child_test {
    use super::*;
    use portable_pty::Child as _;

    fn child_whose_connection_ended() -> WrappedSshChild {
        // The connecting thread drops its sender when it gives up.
        let (tx, rx) = channel::<SshChildProcess>();
        drop(tx);
        WrappedSshChild {
            status: None,
            rx,
            exited: None,
            killer: WrappedSshChildKiller {
                inner: Arc::new(Mutex::new(KillerInner {
                    killer: None,
                    pending_kill: false,
                })),
            },
        }
    }

    #[test]
    fn a_pane_whose_connection_never_came_up_reports_connect_failed() {
        let mut child = child_whose_connection_ended();
        let status = child.try_wait().unwrap().expect("ended");
        assert_eq!(status.signal(), Some(wezterm_ssh::CONNECT_FAILED));

        let mut child = child_whose_connection_ended();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(wezterm_ssh::CONNECT_FAILED));
    }
}
