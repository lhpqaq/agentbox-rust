mod attach;

use agentbox::{
    config,
    rpc::{self, RpcClient},
};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    time::Duration,
};
#[derive(Parser)]
#[command(
    name = "agentbox",
    version,
    about = "Local persistent Bash PTY workspace (Rust)"
)]
struct Cli {
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[arg(long, global = true, default_value_t = 8765)]
    port: u16,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Serve the bundled web UI; an independent broker keeps shells alive.
    Serve,
    /// Start only the detached broker, for CLI/MCP use.
    Start,
    #[command(hide = true)]
    Broker,
    /// Print the local bearer token. Treat output as a password.
    Token,
    Status,
    Sessions,
    /// Share a native terminal; omit SESSION_ID to create a new shell. Ctrl-] detaches.
    Attach {
        session_id: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Show only new output instead of replaying retained terminal output.
        #[arg(long)]
        no_replay: bool,
        /// Observe an existing session without sending input or resizing its PTY.
        #[arg(long, requires = "session_id")]
        read_only: bool,
    },
    Create {
        #[arg(default_value = "terminal")]
        name: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long, default_value_t = 30)]
        rows: u16,
        #[arg(long, default_value_t = 100)]
        cols: u16,
    },
    Run {
        session_id: String,
        command: String,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
    Wait {
        session_id: String,
        command_id: String,
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
    Input {
        session_id: String,
        data: Option<String>,
        #[arg(long)]
        newline: bool,
    },
    Interrupt {
        session_id: String,
    },
    Close {
        session_id: String,
    },
    Rename {
        session_id: String,
        name: String,
    },
    Resize {
        session_id: String,
        #[arg(long)]
        rows: u16,
        #[arg(long)]
        cols: u16,
    },
    History {
        session_id: String,
    },
    Logs {
        session_id: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long)]
        follow: bool,
    },
    /// Stop HTTP, preserving shells unless --shutdown-sessions is supplied.
    Stop {
        #[arg(long)]
        shutdown_sessions: bool,
        /// Stop the broker and shells without contacting an HTTP server.
        #[arg(long, requires = "shutdown_sessions")]
        broker_only: bool,
    },
    /// Newline-delimited JSON-RPC MCP server on stdin/stdout.
    Mcp,
}
fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}
fn validate_timeout(t: f64) -> Result<Duration> {
    if !t.is_finite() || !(0.0..=3600.0).contains(&t) {
        bail!("timeout must be finite and between 0 and 3600 seconds");
    }
    Ok(Duration::from_secs_f64(t))
}
fn wait(client: &RpcClient, sid: &str, cid: &str, timeout: f64) -> Result<Value> {
    validate_timeout(timeout)?;
    let observed = agentbox::mcp::wait(
        client,
        &json!({"session_id":sid,"command_id":cid,"timeout":timeout,"poll_interval":0.05,"after":0,"max_output_bytes":1024*1024}),
    )?;
    let mut result = observed["command"].clone();
    if !result.is_object() {
        result = json!({"id":cid,"session_id":sid});
    }
    for key in [
        "output",
        "output_bytes",
        "output_truncated",
        "output_may_be_truncated",
        "history_retained",
        "output_retention_verified",
        "error",
        "may_be_running",
    ] {
        if let Some(v) = observed.get(key) {
            result[key] = v.clone();
        }
    }
    result["timed_out"] = json!(observed["error"].is_string());
    result["cursor"] = observed["after"].clone();
    Ok(result)
}
fn check_result(result: &Value) -> Result<()> {
    if result["status"] != "finished" || result["exit_code"].as_i64() != Some(0) {
        bail!(
            "Command ended with status {} and exit code {}; inspect returned output",
            result["status"],
            result["exit_code"]
        );
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("agentbox: {e:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args = Cli::parse();
    let home = args.home.unwrap_or_else(config::default_home);
    match args.command {
        Commands::Serve => {
            rpc::ensure_broker(&home)?;
            eprintln!(
                "AgentBox Rust: http://127.0.0.1:{} (token: agentbox token --home YOUR_HOME)",
                args.port
            );
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(agentbox::http::serve(home, args.port))?;
        }
        Commands::Attach {
            session_id,
            cwd,
            no_replay,
            read_only,
        } => attach::run(home, session_id, cwd, no_replay, read_only)?,
        Commands::Start => print(&rpc::ensure_broker(&home)?)?,
        Commands::Broker => agentbox::broker::serve(home)?,
        Commands::Token => {
            let home = config::init_home(&home)?;
            println!("{}", config::read_token(&home)?);
        }
        Commands::Mcp => agentbox::mcp::run(home)?,
        mut command => {
            let client = RpcClient::new(&home)?;
            // Resolve once before any mutation or polling. A concurrent rename
            // must never redirect subsequent input or observation to another shell.
            let selector = match &mut command {
                Commands::Run { session_id, .. }
                | Commands::Wait { session_id, .. }
                | Commands::Input { session_id, .. }
                | Commands::Interrupt { session_id }
                | Commands::Close { session_id }
                | Commands::Rename { session_id, .. }
                | Commands::Resize { session_id, .. }
                | Commands::History { session_id }
                | Commands::Logs { session_id, .. } => Some(session_id),
                _ => None,
            };
            if let Some(id) = selector {
                *id = agentbox::selection::resolve(&client, id)?;
            }
            match command {
                Commands::Status => print(&client.call("status", json!({}))?)?,
                Commands::Sessions => print(&client.call("sessions", json!({}))?)?,
                Commands::Create {
                    name,
                    cwd,
                    rows,
                    cols,
                } => {
                    let c = client.call(
                        "create",
                        json!({"name":name,"cwd":cwd,"rows":rows,"cols":cols}),
                    )?;
                    println!("{}", c["id"].as_str().context("Missing session ID")?);
                }
                Commands::Run {
                    session_id,
                    command,
                    request_id,
                    no_wait,
                    timeout,
                } => {
                    validate_timeout(timeout)?;
                    let c = client.call(
                        "run",
                        json!({"session_id":session_id,"command":command,"request_id":request_id}),
                    )?;
                    if no_wait {
                        print(&c)?;
                    } else {
                        let result = wait(
                            &client,
                            &session_id,
                            c["id"].as_str().context("Missing command ID")?,
                            timeout,
                        )?;
                        print(&result)?;
                        if result["timed_out"] == true {
                            bail!("Wait timed out; command may still be running. Wait again; do not resubmit automatically.");
                        }
                        check_result(&result)?;
                    }
                }
                Commands::Wait {
                    session_id,
                    command_id,
                    timeout,
                } => {
                    let result = wait(&client, &session_id, &command_id, timeout)?;
                    print(&result)?;
                    if result["timed_out"] == true {
                        bail!("Wait timed out; command may still be running");
                    }
                    check_result(&result)?;
                }
                Commands::Input {
                    session_id,
                    data,
                    newline,
                } => {
                    let mut data = match data {
                        Some(s) => s,
                        None => {
                            let mut s = String::new();
                            io::stdin().take(65537).read_to_string(&mut s)?;
                            s
                        }
                    };
                    if newline {
                        data.push('\n');
                    }
                    if data.len() > 65536 || data.contains('\0') {
                        bail!("Input must be <=64 KiB without NUL");
                    }
                    print(&client.call("input", json!({"session_id":session_id,"data":data}))?)?;
                }
                Commands::Interrupt { session_id } => {
                    print(&client.call("interrupt", json!({"session_id":session_id}))?)?
                }
                Commands::Close { session_id } => {
                    print(&client.call("close", json!({"session_id":session_id}))?)?
                }
                Commands::Rename { session_id, name } => {
                    print(&client.call("rename", json!({"session_id":session_id,"name":name}))?)?
                }
                Commands::Resize {
                    session_id,
                    rows,
                    cols,
                } => print(&client.call(
                    "resize",
                    json!({"session_id":session_id,"rows":rows,"cols":cols}),
                )?)?,
                Commands::History { session_id } => {
                    print(&client.call("history", json!({"session_id":session_id}))?)?
                }
                Commands::Logs {
                    session_id,
                    mut after,
                    follow,
                } => loop {
                    let events = client.call(
                        "events",
                        json!({"session_id":session_id,"after":after,"limit":1000}),
                    )?;
                    for e in events.as_array().context("Invalid events")? {
                        after = after.max(e["seq"].as_i64().unwrap_or(after));
                        println!("{e}");
                    }
                    if !follow {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                },
                Commands::Stop {
                    shutdown_sessions,
                    broker_only,
                } => {
                    if shutdown_sessions {
                        print(&client.call("shutdown", json!({}))?)?;
                    }
                    if broker_only {
                        return Ok(());
                    }
                    match stop_http(args.port, &client.token) {
                        Ok(()) => eprintln!("HTTP server stopped"),
                        Err(e) if shutdown_sessions => {
                            eprintln!("Broker shutdown requested; HTTP stop not confirmed: {e}")
                        }
                        Err(e) => return Err(e),
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}
fn stop_http(port: u16, token: &str) -> Result<()> {
    let mut s = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(s,"POST /api/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}")?;
    let mut first = String::new();
    std::io::BufReader::new(s.take(8192)).read_line(&mut first)?;
    if first.split_whitespace().nth(1) != Some("200") {
        bail!("HTTP shutdown was not confirmed");
    }
    Ok(())
}
