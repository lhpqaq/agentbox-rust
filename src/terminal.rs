//! Linux/macOS real-PTY, persistent Bash. The control pipe is instrumentation, not a
//! security boundary: trusted shell code can alter it. Raw terminal input permanently
//! disables structured submission for that terminal. Cleanup covers the Unix
//! session, not programs deliberately escaping with setsid/daemonization.
use anyhow::{anyhow, bail, Context, Result};
use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub struct TerminalSpec {
    pub home: PathBuf,
    pub id: String,
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
}
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    Ready {
        cwd: String,
    },
    Started {
        command_id: String,
    },
    Output {
        text: String,
        command_id: Option<String>,
        prompt: bool,
    },
    Finished {
        command_id: String,
        exit_code: i32,
        cwd: String,
    },
    Exited {
        exit_code: Option<i32>,
        reason: String,
    },
    State {
        status: String,
        cwd: Option<String>,
    },
}
type Sink = Arc<dyn Fn(TerminalEvent) + Send + Sync>;
type Reply = mpsc::Sender<Result<()>>;
enum Request {
    Run(String, String, Reply),
    Input(String, Reply),
    Interrupt(Reply),
    Resize(u16, u16, Reply),
    Close(Reply),
}
pub struct Terminal {
    tx: SyncSender<Request>,
    idle: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    pid: u32,
    worker: std::sync::Mutex<Option<JoinHandle<()>>>,
}
const LIMIT: usize = 1024 * 1024;
const CONTROL_FD: RawFd = 198;

impl Terminal {
    pub fn spawn(spec: TerminalSpec, sink: Sink) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(32);
        let idle = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let mut engine = Engine::spawn(spec, sink, idle.clone(), closed.clone())?;
        let pid = engine.child.id();
        let worker = thread::Builder::new()
            .name(format!("agentbox-pty-{pid}"))
            .spawn(move || engine.serve(rx, ready_tx))?;
        let terminal = Self {
            tx,
            idle,
            closed,
            pid,
            worker: std::sync::Mutex::new(Some(worker)),
        };
        match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(terminal),
            Ok(Err(e)) => {
                let _ = terminal.close();
                Err(e)
            }
            Err(_) => {
                let _ = terminal.close();
                bail!("Bash did not become ready within 8 seconds")
            }
        }
    }
    fn request(&self, make: impl FnOnce(Reply) -> Request) -> Result<()> {
        if self.is_closed() {
            bail!("Terminal is closed");
        }
        let (tx, rx) = mpsc::channel();
        self.tx
            .try_send(make(tx))
            .map_err(|e| anyhow!("Terminal request queue unavailable: {e}"))?;
        rx.recv().map_err(|_| anyhow!("Terminal worker stopped"))?
    }
    pub fn run(&self, id: &str, command: &str) -> Result<()> {
        if command.len() > 16 * LIMIT {
            bail!("Command exceeds 16 MiB limit");
        }
        self.request(|r| Request::Run(id.into(), command.into(), r))
    }
    pub fn input(&self, data: &str) -> Result<()> {
        if data.len() > LIMIT {
            bail!("Terminal input queue full");
        }
        self.request(|r| Request::Input(data.into(), r))
    }
    pub fn interrupt(&self) -> Result<()> {
        self.request(Request::Interrupt)
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.request(|r| Request::Resize(rows, cols, r))
    }
    pub fn close(&self) -> Result<()> {
        let mut result = Ok(());
        if !self.is_closed() {
            let (tx, rx) = mpsc::channel();
            if self.tx.send(Request::Close(tx)).is_ok() {
                result = rx
                    .recv()
                    .unwrap_or_else(|_| Err(anyhow!("Terminal worker stopped during cleanup")));
            }
        }
        if let Some(handle) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            if handle.join().is_err() {
                return Err(anyhow!("Terminal worker panicked"));
            }
        }
        result
    }
    pub fn is_idle(&self) -> bool {
        self.idle.load(Ordering::Acquire) && !self.is_closed()
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
struct Active {
    id: String,
    path: PathBuf,
    begun: bool,
    done: Option<Completion>,
    prompt: Option<(i32, String)>,
    interrupt: bool,
    early: Vec<u8>,
    retry: Option<(Instant, u8)>,
}
struct Completion {
    status: i32,
    cwd: String,
    at: Instant,
}
struct Engine {
    master: OwnedFd,
    control: OwnedFd,
    child: Child,
    rc: PathBuf,
    runtime: PathBuf,
    sink: Sink,
    idle: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    cwd: String,
    active: Option<Active>,
    tainted: bool,
    initialized: bool,
    control_bytes: Vec<u8>,
    pending: VecDeque<u8>,
    decoder: Decoder,
    framing: Framing,
    cleaned: bool,
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn private_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if let Err(e) = file.write_all(content) {
        let _ = fs::remove_file(path);
        return Err(e.into());
    }
    Ok(())
}
fn nonblock(fd: RawFd) -> Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}
impl Engine {
    fn spawn(
        spec: TerminalSpec,
        sink: Sink,
        idle: Arc<AtomicBool>,
        closed: Arc<AtomicBool>,
    ) -> Result<Self> {
        let runtime = spec.home.join("runtime");
        fs::create_dir_all(&runtime)?;
        let nonce = Uuid::new_v4().simple().to_string();
        let rc = runtime.join(format!("{nonce}.bashrc"));
        let script = format!(
            r#"HISTFILE=/dev/null
HISTSIZE=0
set +o history
set +o emacs
set +o vi
PS1='agentbox \w $ '
PS2='> '
__ab_start='\[\e]633;AgentBoxPrompt;{nonce}\a\]'
__ab_end='\[\e]633;AgentBoxEnd;{nonce}\a\]'
__ab_current_command=
__ab_prompt() {{
  local __ab_status=$?
  PS1=${{PS1//"$__ab_start"/}}
  PS1=${{PS1//"$__ab_end"/}}
  PS1="${{__ab_start}}${{PS1}}${{__ab_end}}"
  builtin printf 'P\0%s\0%s\0%s\0%s\0' "$__ab_current_command" "$__ab_status" "$PWD" "${{BASHPID:-$$}}" >&198
  __ab_current_command=
}}
PROMPT_COMMAND=__ab_prompt
"#
        );
        private_file(&rc, script.as_bytes())?;
        let result = (|| -> Result<Self> {
            let mut fds = [0; 2];
            if unsafe { crate::platform::pipe_cloexec(fds.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error().into());
            }
            let control = unsafe { OwnedFd::from_raw_fd(fds[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
            let (mut master, mut slave) = (-1, -1);
            let mut size = libc::winsize {
                ws_row: spec.rows.max(1),
                ws_col: spec.cols.max(1),
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    // Darwin requires a mutable pointer; Linux accepts const.
                    std::ptr::addr_of_mut!(size),
                )
            } < 0
            {
                return Err(io::Error::last_os_error().into());
            }
            let master = unsafe { OwnedFd::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
                if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error().into());
                }
            }
            nonblock(master.as_raw_fd())?;
            nonblock(control.as_raw_fd())?;
            let write_fd = write.as_raw_fd();
            let mut cmd =
                Command::new(std::env::var_os("AGENTBOX_BASH").unwrap_or_else(|| "bash".into()));
            cmd.args(["--noprofile", "--rcfile"])
                .arg(&rc)
                .arg("-i")
                .current_dir(&spec.cwd)
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave))
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .env("AGENTBOX_SESSION", &spec.id);
            for key in [
                "AGENTBOX_TOKEN",
                "BASH_ENV",
                "ENV",
                "PS1",
                "PS2",
                "PROMPT_COMMAND",
                "SHELLOPTS",
                "BASHOPTS",
            ] {
                cmd.env_remove(key);
            }
            unsafe {
                cmd.pre_exec(move || {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if write_fd != CONTROL_FD && libc::dup2(write_fd, CONTROL_FD) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::fcntl(CONTROL_FD, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = cmd.spawn().context("start interactive Bash")?;
            Ok(Self {
                master,
                control,
                child,
                rc: rc.clone(),
                runtime,
                sink,
                idle,
                closed,
                cwd: spec.cwd.to_string_lossy().into(),
                active: None,
                tainted: false,
                initialized: false,
                control_bytes: Vec::new(),
                pending: VecDeque::new(),
                decoder: Decoder::default(),
                framing: Framing::new(&nonce),
                cleaned: false,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(rc);
        }
        result
    }
    fn emit(&self, event: TerminalEvent) {
        (self.sink)(event);
    }
    fn foreground(&self) -> bool {
        unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) == self.child.id() as i32 }
    }
    fn serve(&mut self, rx: Receiver<Request>, ready: mpsc::Sender<Result<()>>) {
        let mut ready = Some(ready);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                self.read_output(false);
                self.read_control()?;
                self.settle_completion();
                self.flush()?;
                if self.initialized {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Ok(()));
                    }
                }
                if let Some(status) = self.child.try_wait()? {
                    let _ = self.cleanup(
                        status.code().or_else(|| {
                            use std::os::unix::process::ExitStatusExt;
                            status.signal().map(|v| -v)
                        }),
                        "Shell exited",
                    );
                    break;
                }
                self.retry_interrupt();
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(Request::Run(id, command, reply)) => {
                        let result = self.run(id, command);
                        let _ = reply.send(result);
                    }
                    Ok(Request::Input(data, reply)) => {
                        let result = self.input(data);
                        let _ = reply.send(result);
                    }
                    Ok(Request::Interrupt(reply)) => {
                        let result = self.interrupt();
                        let _ = reply.send(result);
                    }
                    Ok(Request::Resize(rows, cols, reply)) => {
                        let result = self.resize(rows, cols);
                        let _ = reply.send(result);
                    }
                    Ok(Request::Close(reply)) => {
                        let result = self.cleanup(None, "Closed by user");
                        let _ = reply.send(result);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = self.cleanup(None, "Owner disconnected");
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Ok::<(), anyhow::Error>(())
        }));
        let reason = match outcome {
            Ok(Ok(())) => "Shell stopped".into(),
            Ok(Err(e)) => format!("Terminal I/O failed: {e}"),
            Err(_) => "Terminal observer or worker panicked".into(),
        };
        if !self.cleaned {
            let _ = self.cleanup(None, &reason);
        }
        if let Some(tx) = ready.take() {
            let _ = tx.send(Err(anyhow!("{reason}")));
        }
    }
    fn run(&mut self, id: String, command: String) -> Result<()> {
        if !self.initialized || self.active.is_some() || self.tainted || !self.foreground() {
            bail!("Session is not idle; raw input requires a new session for structured commands");
        }
        if id.is_empty() || id.len() > 256 || id.contains('\0') || command.contains('\0') {
            bail!("Invalid command ID or NUL in command");
        }
        let path = self.runtime.join(format!("{}.sh", Uuid::new_v4().simple()));
        let path_text = path
            .to_str()
            .ok_or_else(|| anyhow!("Runtime path must be UTF-8"))?;
        // Restore echo inside Bash before user code; never race a program's read -s.
        let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut attrs) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let echo = attrs.c_lflag & libc::ECHO != 0;
        if echo && !Path::new("/bin/stty").is_file() {
            bail!("/bin/stty is required for safe terminal echo restoration");
        }
        let restore = if echo { "/bin/stty echo; " } else { "" };
        let line = format!("__ab_current_command={}; {restore}builtin printf 'S\\0%s\\0%s\\0%s\\0%s\\0' {} 0 \"$PWD\" \"${{BASHPID:-$$}}\" >&198; builtin source {}; builtin printf 'D\\0%s\\0%s\\0%s\\0%s\\0' {} \"$?\" \"$PWD\" \"${{BASHPID:-$$}}\" >&198\n",quote(&id),quote(&id),quote(path_text),quote(&id));
        if line.len() > 3500 || self.pending.len() + line.len() > LIMIT {
            bail!("Command wrapper exceeds PTY input bound");
        }
        private_file(&path, command.as_bytes())?;
        if echo {
            let mut quiet = attrs;
            quiet.c_lflag &= !libc::ECHO;
            if unsafe { libc::tcsetattr(self.master.as_raw_fd(), libc::TCSANOW, &quiet) } < 0 {
                let _ = fs::remove_file(path);
                return Err(io::Error::last_os_error().into());
            }
        }
        self.active = Some(Active {
            id,
            path,
            begun: false,
            done: None,
            prompt: None,
            interrupt: false,
            early: Vec::new(),
            retry: None,
        });
        self.idle.store(false, Ordering::Release);
        self.pending.extend(line.bytes());
        self.emit(TerminalEvent::State {
            status: "running".into(),
            cwd: Some(self.cwd.clone()),
        });
        Ok(())
    }
    fn input(&mut self, data: String) -> Result<()> {
        if self.pending.len() + self.active.as_ref().map_or(0, |a| a.early.len()) + data.len()
            > LIMIT
        {
            bail!("Terminal input queue full");
        }
        if data.is_empty() {
            return Ok(());
        }
        // Even input to a running program may leave unread bytes for Bash.
        self.tainted = true;
        self.idle.store(false, Ordering::Release);
        // Only before START does byte 3 mean a deferred interrupt request.
        // Once started, the foreground program owns terminal input semantics:
        // with ISIG disabled it must receive byte 3 rather than a SIGINT.
        if let Some(a) = &mut self.active {
            if !a.begun {
                if data == "\u{3}" {
                    a.interrupt = true;
                    a.early.clear();
                } else {
                    a.early.extend(data.bytes());
                }
                return Ok(());
            }
        }
        if self.active.is_none() {
            self.tainted = true;
            self.idle.store(false, Ordering::Release);
            self.emit(TerminalEvent::State {
                status: "interactive".into(),
                cwd: Some(self.cwd.clone()),
            });
        }
        self.pending.extend(data.bytes());
        Ok(())
    }
    fn interrupt(&mut self) -> Result<()> {
        if let Some(a) = &mut self.active {
            if !a.begun {
                a.interrupt = true;
                a.early.clear();
                return Ok(());
            }
            a.retry = Some((Instant::now() + Duration::from_millis(50), 2));
        }
        self.pending.clear();
        // TTY VINTR both signals and flushes the slave's queued input when enabled.
        let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut attrs) } == 0
            && attrs.c_lflag & libc::ISIG != 0
            && attrs.c_lflag & libc::NOFLSH == 0
            && attrs.c_cc[libc::VINTR] != 0
        {
            self.pending.push_back(attrs.c_cc[libc::VINTR]);
        } else {
            self.signal_foreground();
        }
        Ok(())
    }
    fn signal_foreground(&self) {
        unsafe {
            let pg = libc::tcgetpgrp(self.master.as_raw_fd());
            if pg > 0 && pg != libc::getpgrp() {
                libc::kill(-pg, libc::SIGINT);
            }
        }
    }
    fn retry_interrupt(&mut self) {
        let retry = self.active.as_mut().and_then(|a| {
            if let Some((at, n)) = a.retry {
                if Instant::now() >= at {
                    a.retry = if n > 1 {
                        Some((Instant::now() + Duration::from_millis(50), n - 1))
                    } else {
                        None
                    };
                    return Some(());
                }
            }
            None
        });
        if retry.is_some() {
            self.signal_foreground();
        }
    }
    fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        if rows == 0 || cols == 0 {
            bail!("Terminal dimensions must be positive");
        }
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        for _ in 0..16 {
            let (first, _) = self.pending.as_slices();
            if first.is_empty() {
                break;
            }
            let n =
                unsafe { libc::write(self.master.as_raw_fd(), first.as_ptr().cast(), first.len()) };
            if n > 0 {
                self.pending.drain(..n as usize);
            } else if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            } else {
                break;
            }
        }
        Ok(())
    }
    fn read_output(&mut self, final_read: bool) {
        for _ in 0..16 {
            let mut buf = [0u8; 16384];
            let n =
                unsafe { libc::read(self.master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            let text = self.decoder.push(&buf[..n as usize], false);
            self.output(&text, false);
        }
        if final_read {
            let text = self.decoder.push(&[], true);
            self.output(&text, true);
        }
    }
    fn output(&mut self, text: &str, final_read: bool) {
        for (text, prompt) in self.framing.push(text, final_read) {
            self.emit(TerminalEvent::Output {
                text,
                prompt,
                command_id: if prompt {
                    None
                } else {
                    self.active.as_ref().map(|a| a.id.clone())
                },
            });
        }
    }
    fn read_control(&mut self) -> Result<()> {
        for _ in 0..16 {
            let mut buf = [0u8; 4096];
            let n =
                unsafe { libc::read(self.control.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            self.control_bytes.extend_from_slice(&buf[..n as usize]);
            if self.control_bytes.len() > LIMIT {
                bail!("Shell control channel overflow");
            }
        }
        for _ in 0..1024 {
            let ends: Vec<usize> = self
                .control_bytes
                .iter()
                .enumerate()
                .filter_map(|(i, b)| (*b == 0).then_some(i))
                .take(5)
                .collect();
            if ends.len() < 5 {
                break;
            }
            let record: Vec<u8> = self.control_bytes.drain(..=ends[4]).collect();
            let fields: Vec<&[u8]> = record[..record.len() - 1].split(|b| *b == 0).collect();
            let parse = |v: &[u8]| {
                std::str::from_utf8(v)
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
            };
            if parse(fields[4]) != Some(self.child.id() as i32) {
                continue;
            }
            let status = match parse(fields[2]) {
                Some(s) => s,
                None => continue,
            };
            let id = String::from_utf8_lossy(fields[1]).into_owned();
            self.read_output(false);
            self.cwd = String::from_utf8_lossy(fields[3]).into_owned();
            match fields[0] {
                b"S" => {
                    if let Some(a) = &mut self.active {
                        if a.id == id && !a.begun {
                            a.begun = true;
                            let interrupt = a.interrupt;
                            let early = std::mem::take(&mut a.early);
                            self.emit(TerminalEvent::Started { command_id: id });
                            if interrupt {
                                self.interrupt()?;
                            } else {
                                self.pending.extend(early);
                            }
                        }
                    }
                }
                b"D" => {
                    if let Some(a) = &mut self.active {
                        if a.id == id && a.begun && a.done.is_none() {
                            a.done = Some(Completion {
                                status,
                                cwd: self.cwd.clone(),
                                at: Instant::now(),
                            });
                            a.retry = None;
                        }
                    }
                }
                b"P" => {
                    if !self.initialized && id.is_empty() {
                        self.initialized = true;
                        self.idle.store(true, Ordering::Release);
                        self.emit(TerminalEvent::Ready {
                            cwd: self.cwd.clone(),
                        });
                    }
                    if let Some(a) = &mut self.active {
                        if a.id == id && a.begun {
                            a.prompt = Some((status, self.cwd.clone()));
                            a.retry = None;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn settle_completion(&mut self) {
        // DONE proves evaluation returned, but does not prove Bash is ready for
        // another managed line. Give the tagged prompt a bounded observation
        // window; a removed/replaced hook must not strand the finished command.
        let eligible = self.active.as_ref().is_some_and(|a| {
            a.prompt.is_some()
                || a.done
                    .as_ref()
                    .is_some_and(|d| d.at.elapsed() >= Duration::from_millis(100))
        });
        if !eligible || !self.foreground() {
            return;
        }
        self.read_output(false);
        let a = self.active.take().unwrap();
        let tagged = a.prompt.is_some();
        let (status, cwd) = if let Some(done) = a.done {
            (done.status, done.cwd)
        } else {
            a.prompt.unwrap()
        };
        if !tagged {
            self.tainted = true;
        }
        let _ = fs::remove_file(a.path);
        self.cwd = cwd.clone();
        self.idle.store(!self.tainted, Ordering::Release);
        self.emit(TerminalEvent::Finished {
            command_id: a.id,
            exit_code: status,
            cwd: cwd.clone(),
        });
        if self.tainted {
            self.emit(TerminalEvent::State {
                status: "interactive".into(),
                cwd: Some(cwd),
            });
        }
    }
    fn cleanup(&mut self, mut code: Option<i32>, reason: &str) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        self.idle.store(false, Ordering::Release);
        self.closed.store(true, Ordering::Release);
        let leader = self.child.id() as i32;
        let mut failures = Vec::new();
        let mut blocked = HashSet::new();
        // A failed STOP is not permission to try another signal or route.
        match self.child.try_wait() {
            Ok(None) => match read_process(leader) {
                Ok(Some(process)) => {
                    if let Err(e) = signal_identity(leader, &process.identity, libc::SIGSTOP) {
                        failures.push(format!("PID {leader} SIGSTOP: {e}"));
                        blocked.insert((leader, process.identity));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    failures.push(format!("Read leader PID {leader} identity: {e}"));
                    blocked.insert((leader, (0, String::new())));
                }
            },
            Ok(Some(status)) => {
                code = code.or_else(|| exit_code(status));
            }
            Err(e) => failures.push(format!("Inspect leader PID {leader}: {e}")),
        }
        let tracked = kill_session(leader, &mut blocked, &mut failures);
        // Never block indefinitely if a process is unkillable or signaling was
        // denied. try_wait is the only proof that this direct child was reaped.
        let deadline = Instant::now() + Duration::from_millis(750);
        let mut reaped = false;
        loop {
            // Darwin can defer terminal teardown while slave output is pending.
            // Keep draining bounded PTY batches while waiting for actual reaping;
            // waiting first and draining only afterward can strand cleanup.
            self.read_output(false);
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    code = code.or_else(|| exit_code(status));
                    reaped = true;
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    failures.push(format!("Reap leader PID {leader}: {e}"));
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !reaped {
            failures.push(format!(
                "Leader PID {leader} was not reaped before the cleanup deadline; it may survive"
            ));
        }
        // An accepted signal is not proof of exit. Check every tracked identity
        // for a live survivor, allowing killed descendants a bounded grace period.
        let mut remaining = tracked;
        loop {
            remaining.retain(|(pid, identity)| match read_process(*pid) {
                Ok(Some(p)) => p.identity == *identity && p.state != 'Z' && p.state != 'X',
                Ok(None) => false,
                Err(_) => true,
            });
            if remaining.is_empty() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        for (pid, _) in remaining {
            failures.push(format!(
                "PID {pid} remains alive or its exit could not be verified"
            ));
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.read_output(true)));
        if let Err(e) = fs::remove_file(&self.rc) {
            if e.kind() != io::ErrorKind::NotFound {
                failures.push(format!("Remove shell startup file: {e}"));
            }
        }
        if let Some(a) = self.active.take() {
            if let Err(e) = fs::remove_file(a.path) {
                if e.kind() != io::ErrorKind::NotFound {
                    failures.push(format!("Remove command file: {e}"));
                }
            }
        }
        let reason = if failures.is_empty() {
            reason.to_string()
        } else {
            format!("{reason}; cleanup incomplete: {}", failures.join("; "))
        };
        if !failures.is_empty() {
            eprintln!("agentbox terminal: {reason}");
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.emit(TerminalEvent::Exited {
                exit_code: code,
                reason: reason.clone(),
            })
        }));
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("{reason}"))
        }
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cleanup(None, "Terminal dropped");
    }
}
fn exit_code(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.code().or_else(|| status.signal().map(|n| -n))
}
type Identity = (i32, String);
struct ProcessIdentity {
    identity: Identity,
    state: char,
}
#[cfg(target_os = "linux")]
fn read_process(pid: i32) -> io::Result<Option<ProcessIdentity>> {
    let raw = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH) => {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    let parse = || -> Option<ProcessIdentity> {
        let fields: Vec<&str> = raw.get(raw.rfind(')')? + 2..)?.split_whitespace().collect();
        Some(ProcessIdentity {
            identity: (fields.get(3)?.parse().ok()?, fields.get(19)?.to_string()),
            state: fields.first()?.chars().next()?,
        })
    };
    parse().map(Some).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Malformed /proc process identity",
        )
    })
}
#[cfg(target_os = "macos")]
fn read_process(pid: i32) -> io::Result<Option<ProcessIdentity>> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if n != size {
        let e = io::Error::last_os_error();
        if n == 0 && matches!(e.raw_os_error(), Some(libc::ESRCH) | Some(libc::ENOENT)) {
            return Ok(None);
        }
        return Err(if n == 0 {
            e
        } else {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Incomplete macOS process identity",
            )
        });
    }
    let session = unsafe { libc::getsid(pid) };
    if session < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(e);
    }
    Ok(Some(ProcessIdentity {
        identity: (
            session,
            format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        ),
        state: if info.pbi_status == 5 { 'Z' } else { 'R' },
    }))
}
fn same_identity(pid: i32, identity: &Identity) -> io::Result<bool> {
    Ok(read_process(pid)?.is_some_and(|p| p.identity == *identity))
}
fn signal_identity(pid: i32, identity: &Identity, signal: i32) -> io::Result<()> {
    if !same_identity(pid, identity)? {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
        if fd >= 0 {
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            if !same_identity(pid, identity)? {
                return Ok(());
            }
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } == 0
            {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ESRCH) => return Ok(()),
                Some(libc::ENOSYS) => {}
                _ => return Err(error),
            }
        } else {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ESRCH) => return Ok(()),
                Some(libc::ENOSYS) => {}
                _ => return Err(error),
            }
        }
    }
    // macOS uses a checked PID/start-time identity; Linux reaches this path only
    // when pidfd is unavailable.
    // Only missing kernel support permits the identity-checked compatibility
    // route. EPERM/EACCES and every other error return without another attempt.
    if !same_identity(pid, identity)? {
        return Ok(());
    }
    if unsafe { libc::kill(pid, signal) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}
fn kill_session(
    session: i32,
    blocked: &mut HashSet<(i32, Identity)>,
    failures: &mut Vec<String>,
) -> HashSet<(i32, Identity)> {
    let mut tracked = HashSet::new();
    let mut attempted = HashSet::new();
    for _ in 0..3 {
        let entries = match crate::platform::session_candidates(session) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("Enumerate session processes: {e}"));
                break;
            }
        };
        for entry in entries {
            let pid = entry;
            if let Some(key) = blocked.iter().find(|(blocked_pid, _)| *blocked_pid == pid) {
                tracked.insert(key.clone());
                continue;
            }
            let identity = match read_process(pid) {
                Ok(Some(p)) if p.identity.0 == session => p.identity,
                Ok(_) => continue,
                Err(e) => {
                    if attempted.insert((pid, (0, String::new()))) {
                        failures.push(format!(
                            "Read PID {pid} ownership; cleanup coverage uncertain: {e}"
                        ));
                    }
                    continue;
                }
            };
            let key = (pid, identity.clone());
            tracked.insert(key.clone());
            if blocked.contains(&key) || !attempted.insert(key.clone()) {
                continue;
            }
            if let Err(e) = signal_identity(pid, &identity, libc::SIGKILL) {
                failures.push(format!("PID {pid} SIGKILL: {e}"));
                blocked.insert(key);
            }
        }
    }
    tracked
}
#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    fn push(&mut self, bytes: &[u8], end: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut offset = 0;
        loop {
            match std::str::from_utf8(&self.pending[offset..]) {
                Ok(s) => {
                    text.push_str(s);
                    offset = self.pending.len();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[offset..offset + valid]).unwrap(),
                    );
                    offset += valid;
                    match e.error_len() {
                        Some(n) => {
                            text.push('\u{fffd}');
                            offset += n;
                        }
                        None => {
                            if end {
                                text.push('\u{fffd}');
                                offset = self.pending.len();
                            }
                            break;
                        }
                    }
                }
            }
        }
        self.pending.drain(..offset);
        text
    }
}
struct Framing {
    start: String,
    end: String,
    pending: String,
    prompt: bool,
}
impl Framing {
    fn new(nonce: &str) -> Self {
        Self {
            start: format!("\x1b]633;AgentBoxPrompt;{nonce}\x07"),
            end: format!("\x1b]633;AgentBoxEnd;{nonce}\x07"),
            pending: String::new(),
            prompt: false,
        }
    }
    fn push(&mut self, text: &str, final_read: bool) -> Vec<(String, bool)> {
        self.pending.push_str(text);
        let mut pieces = Vec::new();
        loop {
            let marker = if self.prompt { &self.end } else { &self.start };
            if let Some(i) = self.pending.find(marker) {
                if i > 0 {
                    pieces.push((self.pending[..i].into(), self.prompt));
                }
                self.pending.drain(..i + marker.len());
                self.prompt = !self.prompt;
                continue;
            }
            let keep = if final_read {
                0
            } else {
                (1..marker.len().min(self.pending.len() + 1))
                    .rev()
                    .find(|&n| self.pending.as_bytes().ends_with(&marker.as_bytes()[..n]))
                    .unwrap_or(0)
            };
            let n = self.pending.len() - keep;
            if n > 0 {
                pieces.push((self.pending[..n].into(), self.prompt));
                self.pending.drain(..n);
            }
            break;
        }
        pieces
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_boundaries() {
        let mut d = Decoder::default();
        assert_eq!(d.push(&[0xe4, 0xb8], false), "");
        assert_eq!(d.push(&[0xad, 0xff], false), "中�");
        assert_eq!(d.push(&[0xe2], true), "�");
    }
    #[test]
    fn split_prompt_framing() {
        let mut f = Framing::new("x");
        let s = format!("hello{}prompt{}tail", f.start, f.end);
        let mut parts = Vec::new();
        for ch in s.chars() {
            parts.extend(f.push(&ch.to_string(), false));
        }
        parts.extend(f.push("", true));
        assert_eq!(
            parts
                .iter()
                .filter(|(_, p)| *p)
                .map(|(s, _)| s.as_str())
                .collect::<String>(),
            "prompt"
        );
        assert_eq!(
            parts
                .iter()
                .filter(|(_, p)| !*p)
                .map(|(s, _)| s.as_str())
                .collect::<String>(),
            "hellotail"
        );
    }
    #[test]
    fn quote_shell() {
        assert_eq!(quote("a'b"), "'a'\\''b'");
    }
    fn terminal() -> (tempfile::TempDir, Terminal, Receiver<TerminalEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let terminal = Terminal::spawn(
            TerminalSpec {
                home: dir.path().into(),
                id: "test".into(),
                cwd: dir.path().into(),
                rows: 24,
                cols: 80,
            },
            Arc::new(move |e| {
                let _ = tx.send(e);
            }),
        )
        .unwrap();
        (dir, terminal, rx)
    }
    fn finish(rx: &Receiver<TerminalEvent>, id: &str) -> (i32, String) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = String::new();
        loop {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("command timed out");
            match event {
                TerminalEvent::Output {
                    text,
                    command_id: Some(command_id),
                    prompt: false,
                } if command_id == id => output.push_str(&text),
                TerminalEvent::Finished {
                    command_id,
                    exit_code,
                    ..
                } if command_id == id => return (exit_code, output),
                TerminalEvent::Exited { reason, .. } => panic!("terminal exited: {reason}"),
                _ => {}
            }
        }
    }
    #[test]
    fn real_pty_persists_shell_state_and_long_source() {
        let (_dir, t, rx) = terminal();
        t.run(
            "one",
            "export TEST_AB_VALUE='persistent'; mkdir child; cd child",
        )
        .unwrap();
        assert_eq!(finish(&rx, "one").0, 0);
        t.run("two", "printf 'VALUE:%s\\n' \"$TEST_AB_VALUE\"; pwd")
            .unwrap();
        let (code, text) = finish(&rx, "two");
        assert_eq!(code, 0);
        assert!(text.contains("VALUE:persistent"));
        assert!(text.contains("/child"));
        let command = format!("printf '%s' '{}'", "x".repeat(12000));
        t.run("long", &command).unwrap();
        let (code, text) = finish(&rx, "long");
        assert_eq!(code, 0);
        assert_eq!(text.matches('x').count(), 12000);
        t.close().unwrap();
        assert!(t.is_closed());
    }
    #[test]
    fn real_pty_early_input_and_conservative_raw_input() {
        let (_dir, t, rx) = terminal();
        t.run("read", "read -r value; printf 'VALUE:%s\\n' \"$value\"")
            .unwrap();
        t.input("hello\n").unwrap();
        let (code, text) = finish(&rx, "read");
        assert_eq!(code, 0);
        assert!(text.contains("VALUE:hello"));
        t.input("echo raw\n").unwrap();
        assert!(!t.is_idle());
        assert!(t.run("unsafe", "echo must-not-run").is_err());
        t.close().unwrap();
    }
    #[test]
    fn real_pty_early_interrupt_does_not_strand_identity() {
        let (_dir, t, rx) = terminal();
        t.run("sleep", "sleep 30").unwrap();
        t.interrupt().unwrap();
        let (code, _) = finish(&rx, "sleep");
        assert_ne!(code, 0);
        t.run("next", "true").unwrap();
        assert_eq!(finish(&rx, "next").0, 0);
        t.close().unwrap();
    }

    #[test]
    fn real_pty_missing_prompt_finishes_without_reopening_admission() {
        let (_dir, t, rx) = terminal();
        t.run(
            "no-prompt",
            "PROMPT_COMMAND=; printf 'done-without-hook\\n'; false",
        )
        .unwrap();
        let (code, text) = finish(&rx, "no-prompt");
        assert_eq!(code, 1);
        assert!(text.contains("done-without-hook"));
        assert!(!t.is_idle());
        assert!(t.run("unsafe", "true").is_err());
        // A delayed or forged terminal prompt must not finish the command twice.
        t.input("printf 'raw\\n'\n").unwrap();
        let deadline = Instant::now() + Duration::from_millis(200);
        while let Ok(event) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            assert!(!matches!(event, TerminalEvent::Finished { .. }));
            if Instant::now() >= deadline {
                break;
            }
        }
        t.close().unwrap();
    }
    #[test]
    fn real_pty_emacs_mode_reports_completion() {
        let (_dir, t, rx) = terminal();
        t.run("emacs", "set -o emacs; printf 'mode-set\\n'")
            .unwrap();
        let (code, text) = finish(&rx, "emacs");
        assert_eq!(code, 0);
        assert!(text.contains("mode-set"));
        // Some Bash builds suppress the hook after editing mode changes. Either
        // a verified prompt allows reuse or the conservative fallback blocks it.
        if t.is_idle() {
            t.run("after-emacs", "printf 'next-command\\n'").unwrap();
            assert_eq!(finish(&rx, "after-emacs").0, 0);
        } else {
            assert!(t.run("unsafe", "true").is_err());
        }
        t.close().unwrap();
    }

    #[test]
    fn real_pty_started_raw_program_receives_literal_ctrl_c() {
        let (_dir, t, rx) = terminal();
        t.run("raw-byte",r#"saved=$(/bin/stty -g); /bin/stty -echo -icanon -isig min 1 time 0; printf 'RAW_READY\n'; byte=$(dd bs=1 count=1 2>/dev/null); /bin/stty "$saved"; printf 'BYTE:%d\n' "'$byte""#).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut started = false;
        let mut output = String::new();
        while !started || !output.contains("RAW_READY") {
            match rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("raw program did not become ready")
            {
                TerminalEvent::Started { command_id } if command_id == "raw-byte" => started = true,
                TerminalEvent::Output {
                    text,
                    command_id: Some(id),
                    prompt: false,
                } if id == "raw-byte" => output.push_str(&text),
                TerminalEvent::Finished { .. } => {
                    panic!("raw program finished before receiving input")
                }
                TerminalEvent::Exited { reason, .. } => panic!("raw terminal exited: {reason}"),
                _ => {}
            }
        }
        t.input("\u{3}").unwrap();
        let (code, text) = finish(&rx, "raw-byte");
        assert_eq!(code, 0, "{text:?}");
        assert!(text.contains("BYTE:3"), "{text:?}");
        assert!(!t.is_idle());
        assert!(t.run("unsafe", "true").is_err());
        t.close().unwrap();
    }

    #[test]
    fn signal_refuses_changed_process_identity() {
        let pid = std::process::id() as i32;
        let process = read_process(pid).unwrap().unwrap();
        let wrong = (
            process.identity.0,
            format!("{}-different", process.identity.1),
        );
        // SIGKILL must never be sent when a numeric PID no longer matches.
        signal_identity(pid, &wrong, libc::SIGKILL).unwrap();
    }
    #[test]
    fn real_pty_close_while_output_is_pending_reaps_every_child() {
        for _ in 0..16 {
            let (_dir, t, rx) = terminal();
            t.input("while :; do printf 'pending-output\\n'; done\n")
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let event = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap();
                if matches!(event, TerminalEvent::Output { text, .. } if text.contains("pending-output\r\n"))
                {
                    break;
                }
            }
            let pid = t.pid() as i32;
            t.close().unwrap();
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }
    #[test]
    fn real_pty_close_reaps_shell_and_stops_background_session_job() {
        let (dir, t, rx) = terminal();
        t.run(
            "background",
            "sleep 30 & printf '%s' \"$!\" > child.pid; true",
        )
        .unwrap();
        assert_eq!(finish(&rx, "background").0, 0);
        let pid = fs::read_to_string(dir.path().join("child.pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let identity = read_process(pid).unwrap().unwrap().identity;
        let start = Instant::now();
        t.close().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        if let Some(process) = read_process(pid).unwrap() {
            assert!(process.identity != identity || process.state == 'Z' || process.state == 'X');
        }
        assert!(read_process(t.pid() as i32).unwrap().is_none());
    }
}
