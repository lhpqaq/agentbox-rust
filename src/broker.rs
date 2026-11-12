use crate::{
    config,
    error::{AbError, Result},
    store::Store,
    terminal::{Terminal, TerminalEvent, TerminalSpec},
    VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const MAX_FRAME: usize = 400 * 1024;
static SIGNAL_STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    SIGNAL_STOP.store(true, Ordering::SeqCst);
}
pub struct Core {
    home: PathBuf,
    store: Arc<Mutex<Store>>,
    terminals: HashMap<String, Terminal>,
    started: Instant,
    pub stopping: Arc<AtomicBool>,
    fatal: Arc<AtomicBool>,
}
impl Core {
    pub fn new(home: &Path) -> Result<Self> {
        let mut store = Store::open(home)?;
        store.recover()?;
        for entry in fs::read_dir(home.join("runtime"))? {
            let entry = entry?;
            let path = entry.path();
            if matches!(
                path.extension().and_then(|x| x.to_str()),
                Some("sh" | "bashrc")
            ) && entry.file_type()?.is_file()
            {
                fs::remove_file(path)?;
            }
        }
        Ok(Self {
            home: home.to_owned(),
            store: Arc::new(Mutex::new(store)),
            terminals: HashMap::new(),
            started: Instant::now(),
            stopping: Arc::new(AtomicBool::new(false)),
            fatal: Arc::new(AtomicBool::new(false)),
        })
    }
    fn db(&self) -> Result<std::sync::MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| AbError::new(500, "Journal lock poisoned"))
    }
    fn terminal(&self, id: &str) -> Result<&Terminal> {
        self.terminals
            .get(id)
            .filter(|t| !t.is_closed())
            .ok_or_else(|| AbError::new(409, "Session is closed"))
    }
    pub fn dispatch(&mut self, method: &str, p: Value) -> Result<Value> {
        if !p.is_object() {
            return Err(AbError::new(422, "Parameters must be an object"));
        }
        if (self.stopping.load(Ordering::SeqCst) || self.fatal.load(Ordering::SeqCst))
            && method != "status"
        {
            return Err(AbError::new(503, "Broker is shutting down"));
        }
        match method {
            "status" => {
                return Ok(
                    json!({"version":VERSION,"capabilities":["native_shell_v1","client_environment_v1"],"broker_pid":std::process::id(),"uptime":self.started.elapsed().as_secs_f64(),"session_count":self.terminals.values().filter(|t|!t.is_closed()).count(),"persistence":"Live sessions survive HTTP restart, not broker or machine restart","journal_healthy":!self.fatal.load(Ordering::SeqCst)}),
                )
            }
            "sessions" => return Ok(json!(self.db()?.sessions()?)),
            "create" => return self.create(p),
            "shutdown" => {
                self.stopping.store(true, Ordering::SeqCst);
                return Ok(json!({"ok":true}));
            }
            _ => {}
        }
        let id = text(&p, "session_id")?;
        let session = self.db()?.session(id)?;
        match method {
            "detail" | "sync" => {
                let db = self.db()?;
                let cursor = db.cursor(id)?;
                let mut out = if method == "detail" {
                    session
                } else {
                    json!({"session":session})
                };
                for (k, v) in cursor.as_object().unwrap() {
                    out[k] = v.clone();
                }
                out["commands"] = json!(db.commands(id)?);
                if method == "detail" {
                    out["events"] = json!(db.events(id, 0, 1000)?);
                }
                Ok(out)
            }
            "read" => {
                let after = number(&p, "after", 0, 0, i64::MAX as u64)? as i64;
                let cap = number(&p, "max_bytes", 65536, 1, 1024 * 1024)? as usize;
                // Hold the journal lock across metadata, output and retention:
                // one bounded response, with no command-history payload or race.
                let db = self.db()?;
                let cursor = db.cursor(id)?;
                let mut snapshot = cursor.clone();
                snapshot["session"] = db.session(id)?;
                let events = json!(db.events(id, after, 64)?);
                crate::transcript::from_snapshot(&snapshot, &events, &cursor, after, cap)
            }
            "cursor" => self.db()?.cursor(id),
            "history" => Ok(json!(self.db()?.commands(id)?)),
            "command" => self.db()?.command(id, text(&p, "command_id")?),
            "events" => {
                let after = number(&p, "after", 0, 0, i64::MAX as u64)?;
                let limit = number(&p, "limit", 1000, 1, 1000)?;
                Ok(json!(self.db()?.events(
                    id,
                    after as i64,
                    limit as usize
                )?))
            }
            "rename" => {
                let mut s = session;
                s["name"] = json!(name(&p)?);
                self.db()?.update_session(s, "session_updated")
            }
            "close" => {
                if let Some(t) = self.terminals.remove(id) {
                    t.close().map_err(terminal_error)?;
                }
                self.db()?.session(id)
            }
            "run" => self.run_command(id, &p),
            "input" => {
                let data = bounded_text(&p, "data", 65536, false)?;
                if p.get("source").is_some_and(|v| v != "user") {
                    return Err(AbError::new(422,"Automatic terminal replies are disabled; only explicit user input is accepted"));
                }
                self.terminal(id)?.input(data).map_err(terminal_error)?;
                Ok(json!({"ok":true}))
            }
            "interrupt" => {
                self.terminal(id)?.interrupt().map_err(terminal_error)?;
                Ok(json!({"ok":true}))
            }
            "resize" => {
                let (rows, cols) = dimensions(&p)?;
                self.terminal(id)?
                    .resize(rows, cols)
                    .map_err(terminal_error)?;
                let mut s = self.db()?.session(id)?;
                s["rows"] = json!(rows);
                s["cols"] = json!(cols);
                self.db()?.update_session(s, "session_updated")
            }
            _ => Err(AbError::new(404, "Unknown broker method")),
        }
    }
    fn create(&mut self, p: Value) -> Result<Value> {
        self.terminals.retain(|_, t| !t.is_closed());
        if self.terminals.len() >= 24 {
            return Err(AbError::new(409, "Maximum 24 live sessions reached"));
        }
        if self.db()?.sessions()?.len() >= 256 {
            return Err(AbError::new(
                409,
                "Maximum 256 retained sessions in this home; use a new private home for new work",
            ));
        }
        let name = name(&p)?;
        let (rows, cols) = dimensions(&p)?;
        let cwd = match p.get("cwd") {
            None | Some(Value::Null) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?),
            Some(Value::String(s)) if !s.is_empty() => PathBuf::from(s),
            _ => return Err(AbError::new(422, "cwd must be a nonempty directory path")),
        };
        let cwd = fs::canonicalize(cwd)
            .map_err(|_| AbError::new(422, "Working directory does not exist"))?;
        if !cwd.is_dir() {
            return Err(AbError::new(422, "Working directory is not a directory"));
        }
        let launch = crate::shell::Launch::parse(&p, &cwd)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = config::now();
        let s = json!({"id":id,"name":name,"cwd":cwd.to_string_lossy(),"status":"starting","pid":null,"created_at":now,"updated_at":now,"exit_code":null,"rows":rows,"cols":cols,"command_count":0,"shell":launch.metadata()});
        self.db()?.update_session(s, "session_created")?;
        let store = self.store.clone();
        let fatal = self.fatal.clone();
        let session_id = id.clone();
        let sink = Arc::new(move |event: TerminalEvent| {
            let result = (|| -> Result<()> {
                let mut db = store
                    .lock()
                    .map_err(|_| AbError::new(500, "Journal lock poisoned"))?;
                match event {
                    TerminalEvent::Output {
                        text,
                        command_id,
                        prompt,
                    } => db.output(&session_id, text, command_id, prompt)?,
                    TerminalEvent::Ready { cwd } => {
                        let mut s = db.session(&session_id)?;
                        s["status"] = json!("idle");
                        s["cwd"] = json!(cwd);
                        db.update_session(s, "session_updated")?;
                    }
                    TerminalEvent::Started { .. } => {}
                    TerminalEvent::Finished {
                        command_id,
                        exit_code,
                        cwd,
                    } => db.finish_command(
                        &session_id,
                        &command_id,
                        if exit_code == 130 {
                            "interrupted"
                        } else {
                            "finished"
                        },
                        Some(exit_code),
                        Some(&cwd),
                    )?,
                    TerminalEvent::Exited { exit_code, reason } => {
                        db.close_session(&session_id, exit_code, &reason)?
                    }
                    TerminalEvent::State { status, cwd } => {
                        let mut s = db.session(&session_id)?;
                        if s["status"] != "closed" {
                            s["status"] = json!(status);
                            if let Some(cwd) = cwd {
                                s["cwd"] = json!(cwd);
                            }
                            db.update_session(s, "session_updated")?;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(e) = result {
                fatal.store(true, Ordering::SeqCst);
                eprintln!("Journal failure: {e}; broker will stop sessions");
            }
        });
        match Terminal::spawn(
            TerminalSpec {
                home: self.home.clone(),
                id: id.clone(),
                cwd,
                rows,
                cols,
                launch,
            },
            sink,
        ) {
            Ok(t) => {
                let mut s = self.db()?.session(&id)?;
                s["pid"] = json!(t.pid());
                self.db()?.update_session(s, "session_updated")?;
                self.terminals.insert(id.clone(), t);
                self.db()?.session(&id)
            }
            Err(e) => {
                let _ = self.db()?.close_session(&id, None, "Shell startup failed");
                Err(AbError::new(500, format!("Shell startup failed: {e}")))
            }
        }
    }
    fn run_command(&mut self, id: &str, p: &Value) -> Result<Value> {
        let command = bounded_text(p, "command", 262144, true)?;
        let key=match p.get("request_id"){None|Some(Value::Null)=>None,Some(Value::String(s)) if valid_key(s)=>Some(s.as_str()),_=>return Err(AbError::new(422,"request_id must be 1-128 ASCII letters, digits, dots, colons, underscores or hyphens"))};
        if let Some(key) = key {
            if let Some(c) = self.db()?.prior_request(id, key, command)? {
                return Ok(c);
            }
            if self.db()?.request_count(id)? >= 10000 {
                return Err(AbError::new(
                    409,
                    "Session reached 10000 durable request keys",
                ));
            }
        }
        if !self.terminal(id)?.is_idle() {
            return Err(AbError::new(409,"Session is busy or has untracked raw input. Use explicit input/interrupt or create a new session"));
        }
        let c = json!({"id":uuid::Uuid::new_v4().simple().to_string(),"session_id":id,"command":command,"status":"running","started_at":config::now(),"finished_at":null,"duration_ms":null,"exit_code":null,"cwd":self.db()?.session(id)?["cwd"],"request_id":key});
        let c = self.db()?.start_command(c)?;
        let submitted = match self.terminal(id) {
            Ok(t) => t
                .run(c["id"].as_str().unwrap(), command)
                .map_err(terminal_error),
            Err(e) => Err(e),
        };
        if let Err(e) = submitted {
            if let Err(write_error) =
                self.db()?
                    .finish_command(id, c["id"].as_str().unwrap(), "failed", None, None)
            {
                self.fatal.store(true, Ordering::SeqCst);
                return Err(write_error.uncertain());
            }
            return Err(AbError::new(
                500,
                format!("Command admission failed: {e}; inspect command history before retrying"),
            )
            .uncertain());
        }
        Ok(c)
    }
    pub fn close_all(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        for (_, t) in self.terminals.drain() {
            let _ = t.close();
        }
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        self.close_all();
    }
}
fn terminal_error(e: anyhow::Error) -> AbError {
    AbError::new(409, e.to_string())
}
fn name(p: &Value) -> Result<String> {
    let s = p
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("terminal")
        .trim();
    if s.is_empty() || s.chars().count() > 80 {
        return Err(AbError::new(422, "Name must contain 1-80 characters"));
    }
    Ok(s.to_owned())
}
fn text<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AbError::new(422, format!("{key} must be a string")))
}
fn bounded_text<'a>(p: &'a Value, key: &str, max: usize, nonempty: bool) -> Result<&'a str> {
    let s = text(p, key)?;
    if s.len() > max || s.contains('\0') || (nonempty && s.trim().is_empty()) {
        return Err(AbError::new(
            422,
            format!("Invalid {key}: text limit {max} bytes, NUL forbidden"),
        ));
    }
    Ok(s)
}
fn number(p: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let n = match p.get(key) {
        None => default,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| AbError::new(422, format!("{key} must be an integer")))?,
    };
    if !(min..=max).contains(&n) {
        return Err(AbError::new(422, format!("Invalid {key} range")));
    }
    Ok(n)
}
fn dimensions(p: &Value) -> Result<(u16, u16)> {
    Ok((
        number(p, "rows", 30, 2, 300)? as u16,
        number(p, "cols", 100, 10, 500)? as u16,
    ))
}
fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

pub fn serve(home: PathBuf) -> Result<()> {
    let home = config::init_home(&home)?;
    let token = config::read_token(&home)?;
    let lock = config::private_file(&home.join("broker.lock"), true)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(AbError::new(
            409,
            "Another broker owns this state directory",
        ));
    }
    let socket = home.join("broker.sock");
    if let Ok(m) = fs::symlink_metadata(&socket) {
        if !m.file_type().is_socket() || m.uid() != unsafe { libc::geteuid() } {
            return Err(AbError::new(
                403,
                "Refusing to replace unsafe broker socket path",
            ));
        }
        fs::remove_file(&socket)?;
    }
    // Binding failure is returned verbatim; never fall back to TCP or another permission route.
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let core = Arc::new(Mutex::new(Core::new(&home)?));
    let connections = Arc::new(AtomicUsize::new(0));
    unsafe {
        libc::signal(libc::SIGTERM, stop_signal as *const () as usize);
        libc::signal(libc::SIGINT, stop_signal as *const () as usize);
    }
    while !SIGNAL_STOP.load(Ordering::SeqCst) {
        {
            let c = core
                .lock()
                .map_err(|_| AbError::new(500, "Broker lock poisoned"))?;
            if c.stopping.load(Ordering::SeqCst) || c.fatal.load(Ordering::SeqCst) {
                break;
            }
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // Darwin inherits O_NONBLOCK from the listener. The framed
                // handler uses blocking I/O with explicit total deadlines.
                stream.set_nonblocking(false)?;
                if connections.load(Ordering::SeqCst) >= 32 {
                    drop(stream);
                    continue;
                }
                connections.fetch_add(1, Ordering::SeqCst);
                let c = core.clone();
                let t = token.clone();
                let count = connections.clone();
                std::thread::spawn(move || {
                    handle(stream, &t, c);
                    count.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => {
                if let Ok(mut c) = core.lock() {
                    c.close_all();
                }
                return Err(e.into());
            }
        }
    }
    if let Ok(mut c) = core.lock() {
        c.close_all();
    }
    drop(listener);
    let _ = fs::remove_file(&socket);
    drop(lock);
    Ok(())
}
fn handle(mut stream: UnixStream, token: &str, core: Arc<Mutex<Core>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let result = (|| -> Result<Value> {
        let mut reader = BufReader::new(&mut stream);
        let mut data = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| AbError::new(408, "Request frame deadline exceeded"))?;
            reader.get_ref().set_read_timeout(Some(remaining))?;
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                return Err(AbError::new(422, "Incomplete request"));
            }
            let n = chunk
                .iter()
                .position(|b| *b == b'\n')
                .map_or(chunk.len(), |i| i + 1);
            if data.len() + n > MAX_FRAME {
                return Err(AbError::new(413, "Broker request too large"));
            }
            let done = chunk[n - 1] == b'\n';
            data.extend_from_slice(&chunk[..n]);
            reader.consume(n);
            if done {
                break;
            }
        }
        let request: Value = crate::mcp::parse_json(&data)?;
        if !request.is_object()
            || !config::constant_time_eq(request["token"].as_str().unwrap_or(""), token)
        {
            return Err(AbError::new(401, "Unauthorized"));
        }
        let method = request["method"]
            .as_str()
            .ok_or_else(|| AbError::new(422, "Missing method"))?;
        core.lock()
            .map_err(|_| AbError::new(500, "Broker lock poisoned"))?
            .dispatch(method, request.get("params").cloned().unwrap_or(json!({})))
    })();
    let response = match result {
        Ok(result) => json!({"ok":true,"result":result}),
        Err(e) => {
            json!({"ok":false,"error":e.message,"code":e.code,"may_have_executed":e.uncertain})
        }
    };
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut left = bytes.as_slice();
        while !left.is_empty() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if stream.set_write_timeout(Some(remaining)).is_err() {
                break;
            }
            match stream.write(left) {
                Ok(0) | Err(_) => break,
                Ok(n) => left = &left[n..],
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_values() {
        assert!(valid_key("build:1-a_b.c"));
        assert!(!valid_key("x y"));
        assert!(!valid_key(""));
        assert!(dimensions(&json!({"rows":true})).is_err());
        assert!(bounded_text(&json!({"command":" \n"}), "command", 262144, true).is_err());
    }
    #[test]
    fn transcript_snapshot_excludes_command_history_and_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let home = config::init_home(&temp.path().join("s")).unwrap();
        let mut core = Core::new(&home).unwrap();
        {
            let mut db = core.db().unwrap();
            db.update_session(
                json!({"id":"s","status":"closed","command_count":0}),
                "session_created",
            )
            .unwrap();
            db.start_command(json!({"id":"c","session_id":"s","command":"history-only-secret","status":"running"})).unwrap();
            db.output("s", "hello".into(), Some("c".into()), false)
                .unwrap();
        }
        let page = core
            .dispatch("read", json!({"session_id":"s","max_bytes":5}))
            .unwrap();
        assert_eq!(page["output"], "hello");
        assert_eq!(page["output_may_be_truncated"], false);
        assert!(!page.to_string().contains("history-only-secret"));
        let small = core
            .dispatch("read", json!({"session_id":"s","max_bytes":1}))
            .unwrap();
        assert_eq!(small["output"], "");
        assert_eq!(small["next_event_bytes"], 5);
        assert_eq!(
            core.dispatch("read", json!({"session_id":"s","max_bytes":0}))
                .unwrap_err()
                .code,
            422
        );
    }
    #[test]
    fn unknown_session_is_not_created() {
        let d = tempfile::tempdir().unwrap();
        let h = config::init_home(&d.path().join("s")).unwrap();
        let mut c = Core::new(&h).unwrap();
        assert_eq!(
            c.dispatch("run", json!({"session_id":"missing","command":"true"}))
                .unwrap_err()
                .code,
            404
        );
        assert_eq!(c.dispatch("sessions", json!({})).unwrap(), json!([]));
    }
}
