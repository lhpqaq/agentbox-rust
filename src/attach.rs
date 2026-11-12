//! Native shared-terminal client. Ctrl-] detaches without closing the shell.
use agentbox::rpc::RpcClient;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct LocalTerminal {
    original: libc::termios,
    handlers: Vec<(libc::c_int, libc::sighandler_t)>,
}
impl LocalTerminal {
    fn enter() -> Result<Self> {
        let mut original = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut original) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut guard = Self {
            original,
            handlers: Vec::new(),
        };
        STOP.store(false, Ordering::Relaxed);
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
            let previous = unsafe { libc::signal(signal, stop as *const () as libc::sighandler_t) };
            if previous == libc::SIG_ERR {
                return Err(io::Error::last_os_error().into());
            }
            guard.handlers.push((signal, previous));
        }
        let mut raw = guard.original;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(guard)
    }
}
impl Drop for LocalTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.original);
            // Do not leave the user's terminal in an application's display,
            // mouse-reporting or bracketed-paste modes after detach or error.
            let reset = b"\x1b[0m\x1b[?25h\x1b[?1049l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l";
            libc::write(1, reset.as_ptr().cast(), reset.len());
            for &(signal, previous) in self.handlers.iter().rev() {
                libc::signal(signal, previous);
            }
        }
    }
}

fn size() -> Option<(u16, u16)> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } < 0
        || size.ws_row == 0
        || size.ws_col == 0
    {
        return None;
    }
    Some((size.ws_row.clamp(2, 300), size.ws_col.clamp(10, 500)))
}
fn call(client: &RpcClient, method: &str, params: Value) -> Result<Value> {
    client
        .call_timeout(method, params, Duration::from_secs(3))
        .map_err(|e| {
            if e.uncertain {
                anyhow::anyhow!("{method}: {e}; input may have been delivered; it was not retried")
            } else {
                anyhow::anyhow!("{method}: {e}")
            }
        })
}

// Native terminals can automatically answer queries from shell output. Suppress
// those queries and string controls (including clipboard OSC 52) so replay or
// live output cannot manufacture input. Keep ordinary screen/color controls.
#[derive(Default)]
struct DisplayFilter {
    state: u8,
    sequence: Vec<u8>,
}
impl DisplayFilter {
    fn push(&mut self, text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        // UTF-8 encoded C1 controls are equivalent to their ESC forms. Normalize
        // complete Unicode scalars, never arbitrary UTF-8 continuation bytes.
        let normalized;
        let bytes = if text.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c)) {
            let mut buffer = Vec::with_capacity(text.len());
            for c in text.chars() {
                if ('\u{80}'..='\u{9f}').contains(&c) {
                    buffer.extend_from_slice(&[0x1b, c as u8 - 0x40]);
                } else {
                    let mut encoded = [0u8; 4];
                    buffer.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
                }
            }
            normalized = buffer;
            normalized.as_slice()
        } else {
            text.as_bytes()
        };
        for &b in bytes {
            match self.state {
                0 => {
                    if b == 0x1b {
                        self.state = 1;
                    } else {
                        out.push(b);
                    }
                }
                1 => match b {
                    b'[' => {
                        self.state = 2;
                        self.sequence.clear();
                    }
                    b']' => self.state = 3,
                    b'P' | b'_' | b'^' | b'X' => self.state = 4,
                    b'Z' => self.state = 0, // DECID requests a device response.
                    0x20..=0x2f => {
                        self.state = 6;
                        self.sequence.clear();
                        self.sequence.push(b);
                    }
                    _ => {
                        out.extend_from_slice(&[0x1b, b]);
                        self.state = 0;
                    }
                },
                2 => {
                    self.sequence.push(b);
                    if (0x40..=0x7e).contains(&b) {
                        // Only screen drawing, colors, scrolling and mode
                        // changes. Unknown CSI operations may solicit input.
                        let safe = b"@ABCDEFGH IJKLMPSTXZ`abdefghlmrs".contains(&b)
                            || (b == b'u' && self.sequence.len() == 1)
                            || (b == b'q' && self.sequence.ends_with(b" q"));
                        let safe_mode = if matches!(b, b'h' | b'l') {
                            let parameters = &self.sequence[..self.sequence.len() - 1];
                            let private = parameters.starts_with(b"?");
                            let parameters = if private {
                                &parameters[1..]
                            } else {
                                parameters
                            };
                            parameters.split(|v| *v == b';').all(|parameter| {
                                if parameter.is_empty() {
                                    return true;
                                }
                                parameter.iter().all(u8::is_ascii_digit)
                                    && std::str::from_utf8(parameter)
                                        .ok()
                                        .and_then(|s| s.parse::<u16>().ok())
                                        .is_some_and(|mode| !private || mode != 1004)
                            })
                        } else {
                            true
                        };
                        if safe && safe_mode {
                            out.extend_from_slice(b"\x1b[");
                            out.extend_from_slice(&self.sequence);
                        }
                        self.sequence.clear();
                        self.state = 0;
                    } else if self.sequence.len() > 256 {
                        self.sequence.clear();
                        self.state = 7;
                    }
                }
                3 | 4 => {
                    if b == 0x1b {
                        self.state += 10;
                    } else if b == 7 && self.state == 3 {
                        self.state = 0;
                    }
                }
                13 | 14 => {
                    if b == b'\\' {
                        self.state = 0;
                    } else if b != 0x1b {
                        self.state -= 10;
                    }
                }
                6 => {
                    self.sequence.push(b);
                    if (0x30..=0x7e).contains(&b) {
                        out.push(0x1b);
                        out.extend_from_slice(&self.sequence);
                        self.sequence.clear();
                        self.state = 0;
                    } else if self.sequence.len() > 16 {
                        self.sequence.clear();
                        self.state = 0;
                    }
                }
                7 => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = 0;
                    }
                }
                _ => unreachable!(),
            }
        }
        out
    }
}

// Preserve a UTF-8 character split over separate terminal reads. The broker's
// JSON input API accepts Unicode rather than arbitrary binary bytes.
#[derive(Default)]
struct Keyboard {
    pending: Vec<u8>,
}
impl Keyboard {
    fn push(&mut self, bytes: &[u8]) -> (String, bool) {
        let end = bytes.iter().position(|b| *b == 0x1d);
        self.pending
            .extend_from_slice(&bytes[..end.unwrap_or(bytes.len())]);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(e) if e.error_len().is_none() && end.is_none() => e.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let text = String::from_utf8_lossy(&self.pending[..valid]).replace('\0', "");
        self.pending.drain(..valid);
        (text, end.is_some())
    }
}

pub fn run(
    home: PathBuf,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    no_replay: bool,
    read_only: bool,
    shell_options: crate::cli_shell::ShellOptions,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("attach needs an interactive terminal on stdin and stdout");
    }
    if session_id.is_some() && (cwd.is_some() || shell_options.specified()) {
        bail!("--cwd and shell/environment options are only supported when creating a new session");
    }
    if read_only && session_id.is_none() {
        bail!("--read-only requires an existing session ID");
    }
    if !read_only {
        agentbox::rpc::ensure_broker(&home)?;
    }
    let client = RpcClient::new(&home)?;
    let session_id = match session_id {
        Some(id) => agentbox::selection::resolve(&client, &id)?,
        None => {
            let (rows, cols) = size().unwrap_or((30, 100));
            let cwd = cwd.unwrap_or(std::env::current_dir()?);
            let mut request = json!({"name":"shared terminal","cwd":cwd,"rows":rows,"cols":cols});
            shell_options.apply(&mut request, true, &cwd)?;
            let native = request.get("shell").is_some();
            let session = call(&client, "create", request)?;
            let id = session["id"]
                .as_str()
                .context("Missing session ID")?
                .to_owned();
            // Initialize only our fresh shell, at the top-level command line.
            // Bash 3.2 loses subsequent prompts if readline is enabled inside
            // the sourced script used by structured run. Direct input also
            // correctly marks this session as interactive from the outset.
            if !native {
                call(
                    &client,
                    "input",
                    json!({"session_id":id,"data":"set -o emacs\n","source":"user"}),
                )?;
            }
            id
        }
    };
    let snapshot = call(&client, "sync", json!({"session_id":session_id}))?;
    let through = snapshot["last_seq"]
        .as_i64()
        .context("Invalid replay cursor")?;
    let mut after = if no_replay { through } else { 0 };
    let mut closed = snapshot["session"]["status"] == "closed";
    let mut import_pending = snapshot["session"]["shell"]["state_import_requested"] == true
        && snapshot["session"]["shell"]["state_imported"] != true;
    eprintln!("AgentBox session: {session_id}\nCtrl-] detaches and keeps the shell running; exit closes the shell.\nBoth operators share keystrokes; take turns typing.");
    if snapshot["session"]["shell"]["state_import_requested"] == true
        && snapshot["session"]["shell"]["state_imported"] != true
        && !closed
    {
        eprintln!("Zsh state import is pending while startup files load; answer any startup prompts in this terminal.");
    }
    if read_only {
        eprintln!("READ ONLY: keyboard input and terminal resizing are disabled; Ctrl-] detaches.");
    }
    if !no_replay {
        eprintln!("Replaying retained terminal output (history is bounded).");
    }
    let _terminal = LocalTerminal::enter()?;
    let mut output = io::stdout().lock();
    let mut filter = DisplayFilter::default();
    let mut keyboard = Keyboard::default();
    let mut last_size = None;
    let mut next_resize = Instant::now();
    loop {
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        let events = call(
            &client,
            "events",
            json!({"session_id":session_id,"after":after,"limit":200}),
        )?;
        let events = events.as_array().context("Invalid event batch")?;
        for event in events {
            after = after.max(event["seq"].as_i64().context("Invalid event sequence")?);
            if event["type"] == "output" {
                output.write_all(&filter.push(event["data"]["text"].as_str().unwrap_or("")))?;
            } else if event["type"] == "session_closed" {
                closed = true;
            }
            if event["type"] == "session_updated"
                && event["data"]["shell"]["state_imported"] == true
            {
                import_pending = false;
            }
        }
        output.flush()?;
        if closed && events.len() < 200 {
            break;
        }
        if !read_only && !closed && Instant::now() >= next_resize {
            if let Some(dimensions) = size() {
                if Some(dimensions) != last_size {
                    call(
                        &client,
                        "resize",
                        json!({"session_id":session_id,"rows":dimensions.0,"cols":dimensions.1}),
                    )?;
                    last_size = Some(dimensions);
                }
            }
            next_resize = Instant::now() + Duration::from_millis(250);
        }
        // Finish initial replay before accepting keystrokes; native device
        // replies are suppressed by DisplayFilter rather than sent to Bash.
        if after < through && events.len() == 200 {
            continue;
        }
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, if events.len() == 200 { 0 } else { 40 }) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error().into());
        }
        if ready == 0 {
            continue;
        }
        if fd.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let mut bytes = [0u8; 4096];
            let count = io::stdin().read(&mut bytes)?;
            if count == 0 {
                break;
            }
            let (text, detach) = keyboard.push(&bytes[..count]);
            if !read_only && !text.is_empty() {
                call(
                    &client,
                    "input",
                    json!({"session_id":session_id,"data":text,"source":"user"}),
                )?;
            }
            if detach {
                break;
            }
        } else if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            bail!("Local terminal disconnected");
        }
    }
    // Restore local tty settings before printing status in cooked mode.
    drop(output);
    drop(_terminal);
    if closed && import_pending {
        bail!("Shell closed before Zsh state import completed; inspect this session's output for startup/import errors");
    }
    eprintln!(
        "\n{} session {session_id}",
        if closed { "Closed" } else { "Detached from" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_queries_and_string_controls_never_reach_local_terminal() {
        let mut f = DisplayFilter::default();
        assert_eq!(f.push("hello\x1b[6"), b"hello");
        assert_eq!(f.push("n\x1b[c\x1bZ\x1b[18t\x1b[?1$p"), b"");
        assert_eq!(f.push("\x1b]52;c;secret"), b"");
        assert_eq!(f.push("\x07\x1bP$qm\x1b\\ok"), b"ok");
        assert_eq!(
            f.push("\x1b[31mred\x1b[0m\x1b[2J"),
            b"\x1b[31mred\x1b[0m\x1b[2J"
        );
        f.push(&format!("\x1b[{}", "1".repeat(300)));
        assert_eq!(f.push("nend"), b"end");
    }
    #[test]
    fn c1_controls_and_zero_padded_focus_modes_are_filtered() {
        let mut filter = DisplayFilter::default();
        assert_eq!(
            filter.push("\u{9b}6n\u{9d}52;c;ignored\u{9c}丛中文"),
            "丛中文".as_bytes()
        );
        assert_eq!(filter.push("\u{90}$qm\u{9c}safe"), b"safe");
        assert_eq!(filter.push("\x1b[?001"), b"");
        assert_eq!(filter.push("004h\x1b[?25;01004h\x1b[?1004:0h"), b"");
        assert_eq!(
            filter.push("\x1b[?25h\u{9b}31mred\x1b[0m"),
            b"\x1b[?25h\x1b[31mred\x1b[0m"
        );
    }
    #[test]
    fn split_unicode_and_detach_preserve_exact_keyboard_input() {
        let mut k = Keyboard::default();
        assert_eq!(k.push(&[0xe4, 0xb8]), (String::new(), false));
        assert_eq!(k.push(&[0xad, b'\r']), ("中\r".into(), false));
        assert_eq!(k.push(b"x\0\x1dignored"), ("x".into(), true));
        assert!(k.pending.len() <= 3);
    }
}
