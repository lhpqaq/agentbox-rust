//! Loopback HTTP facade. The detached broker outlives this server.
use crate::rpc::RpcClient;
use axum::{
    body::{to_bytes, Body},
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{convert::Infallible, path::PathBuf, time::Duration};
use tokio::sync::watch;
const MAX_BODY: usize = 300 * 1024;
#[derive(Clone)]
struct App {
    rpc: RpcClient,
    port: u16,
    stop: watch::Sender<bool>,
}
fn error(code: u16, message: impl Into<String>) -> Response {
    (
        StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({"detail":message.into()})),
    )
        .into_response()
}
/// Exact port validation prevents accepting a different local origin.
pub fn valid_authority(authority: &str, port: u16) -> bool {
    let Some((host, p)) = authority.rsplit_once(':') else {
        return false;
    };
    if p != port.to_string() {
        return false;
    }
    host == "localhost"
        || (if host.starts_with('[') && host.ends_with(']') {
            &host[1..host.len() - 1]
        } else {
            host
        })
        .parse::<std::net::IpAddr>()
        .map(|ip| {
            ip.is_loopback()
                && (!host.contains(':') || (host.starts_with('[') && host.ends_with(']')))
        })
        .unwrap_or(false)
}
async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut response =
        if h.get_all(header::HOST).iter().count() != 1 || !valid_authority(host, app.port) {
            error(403, "Invalid Host header")
        } else if h.get_all(header::ORIGIN).iter().count() > 1
            || h.get(header::ORIGIN)
                .map(|v| v.to_str().ok() != Some(format!("http://{host}").as_str()))
                .unwrap_or(false)
            || h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site")
        {
            error(403, "Cross-origin access denied")
        } else if req.uri().path().starts_with("/api/")
            && (h.get_all(header::AUTHORIZATION).iter().count() != 1
                || !h
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(|v| crate::config::constant_time_eq(v, &app.rpc.token))
                    .unwrap_or(false))
        {
            let mut r = error(401, "A valid local AgentBox token is required");
            r.headers_mut()
                .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
            r
        } else {
            next.run(req).await
        };
    for (key,value) in [("x-content-type-options","nosniff"),("x-frame-options","DENY"),("referrer-policy","no-referrer"),("cache-control","no-store"),("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")] {response.headers_mut().insert(axum::http::HeaderName::from_static(key),value.parse().unwrap());}
    response
}
// Responses are returned directly by the handlers, including on error.
#[allow(clippy::result_large_err)]
async fn call(app: &App, method: &str, params: Value) -> Result<Value, Response> {
    let rpc = app.rpc.clone();
    let method = method.to_owned();
    tokio::task::spawn_blocking(move || rpc.call(&method, params))
        .await
        .map_err(|_| error(500, "Broker task failed"))?
        .map_err(|e| error(e.code, e.message.replace(&app.rpc.token, "[redacted]")))
}
fn str_field(v: &Value, key: &str, min: usize, max: usize, required: bool) -> bool {
    match v.get(key) {
        None => !required,
        Some(s) => s
            .as_str()
            .map(|s| (min..=max).contains(&s.chars().count()))
            .unwrap_or(false),
    }
}
fn int_field(v: &Value, key: &str, min: u64, max: u64, required: bool) -> bool {
    match v.get(key) {
        None => !required,
        Some(s) => s
            .as_u64()
            .map(|s| (min..=max).contains(&s))
            .unwrap_or(false),
    }
}
pub(crate) fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}
fn validate(method: &str, v: &Value) -> bool {
    let keys: &[&str] = match method {
        "create" => &[
            "name",
            "cwd",
            "rows",
            "cols",
            "shell",
            "login",
            "no_rc",
            "environment",
        ],
        "rename" => &["name"],
        "run" => &["command", "request_id"],
        "input" => &["data", "source", "event_seq"],
        "resize" => &["rows", "cols"],
        "shutdown" => &["shutdown_sessions"],
        _ => &[],
    };
    if !v
        .as_object()
        .map(|o| o.keys().all(|k| keys.contains(&k.as_str())))
        .unwrap_or(false)
    {
        return false;
    }
    match method {
        "create" => {
            str_field(v, "name", 1, 80, false)
                && (v.get("cwd").map(|x| x.is_null()).unwrap_or(true)
                    || str_field(v, "cwd", 1, 4096, false))
                && int_field(v, "rows", 2, 300, false)
                && int_field(v, "cols", 10, 500, false)
                && str_field(v, "shell", 1, 4096, false)
                && ["login", "no_rc"]
                    .iter()
                    .all(|k| v.get(k).is_none_or(Value::is_boolean))
                && v.get("environment").is_none_or(|e| {
                    e.as_object()
                        .is_some_and(|o| o.len() <= 2048 && o.values().all(Value::is_string))
                })
        }
        "rename" => str_field(v, "name", 1, 80, true),
        "run" => {
            str_field(v, "command", 1, 262144, true)
                && v["command"]
                    .as_str()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                && (v
                    .get("request_id")
                    .map(|v| v.is_null() || v.as_str().map(valid_key).unwrap_or(false))
                    .unwrap_or(true))
        }
        "input" => {
            str_field(v, "data", 0, 65536, true)
                && v.get("source").map(|x| x == "user").unwrap_or(true)
                && v.get("event_seq")
                    .map(|x| {
                        x.is_null()
                            || x.as_u64()
                                .map(|n| n > 0 && n <= i64::MAX as u64)
                                .unwrap_or(false)
                    })
                    .unwrap_or(true)
        }
        "resize" => int_field(v, "rows", 2, 300, true) && int_field(v, "cols", 10, 500, true),
        "shutdown" => v
            .get("shutdown_sessions")
            .map(Value::is_boolean)
            .unwrap_or(true),
        _ => true,
    }
}
#[allow(clippy::result_large_err)]
async fn read_body(req: Request) -> Result<Value, Response> {
    let bytes = to_bytes(req.into_body(), MAX_BODY)
        .await
        .map_err(|_| error(413, "Request too large"))?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    crate::mcp::parse_json(&bytes).map_err(|_| error(422, "Invalid JSON body"))
}
async fn api(State(app): State<App>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let verb = req.method().as_str().to_owned();
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let (method, status) = match (verb.as_str(), parts.as_slice()) {
        ("GET", ["api", "status"]) => ("status", 200),
        ("GET", ["api", "sessions"]) => ("sessions", 200),
        ("POST", ["api", "sessions"]) => ("create", 201),
        ("GET", ["api", "sessions", _]) => ("detail", 200),
        ("PATCH", ["api", "sessions", _]) => ("rename", 200),
        ("DELETE", ["api", "sessions", _]) => ("close", 200),
        ("POST", ["api", "sessions", _, "commands"]) => ("run", 202),
        ("GET", ["api", "sessions", _, "commands", _]) => ("command", 200),
        ("GET", ["api", "sessions", _, "history"]) => ("history", 200),
        ("GET", ["api", "sessions", _, "log"]) => ("events", 200),
        ("POST", ["api", "sessions", _, "input"]) => ("input", 200),
        ("POST", ["api", "sessions", _, "interrupt"]) => ("interrupt", 200),
        ("POST", ["api", "sessions", _, "resize"]) => ("resize", 200),
        ("POST", ["api", "shutdown"]) => ("shutdown", 200),
        _ => return error(404, "Not found"),
    };
    let query = Query::<LogQuery>::try_from_uri(req.uri());
    let mut params = match read_body(req).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if !validate(method, &params) {
        return error(
            422,
            "Invalid or unsupported arguments (terminal device replies are disabled)",
        );
    }
    if parts.len() >= 3 && parts[1] == "sessions" {
        params["session_id"] = json!(parts[2]);
    }
    if method == "command" {
        params["command_id"] = json!(parts[4]);
    }
    if method == "events" {
        let q = match query {
            Ok(Query(q)) => q,
            Err(_) => return error(422, "Invalid query"),
        };
        if q.after > i64::MAX as u64 || q.limit == 0 || q.limit > 1000 {
            return error(422, "Invalid cursor or limit");
        }
        params["after"] = json!(q.after);
        params["limit"] = json!(q.limit);
    }
    if method == "shutdown" {
        let all = params["shutdown_sessions"].as_bool().unwrap_or(false);
        if all {
            if let Err(r) = call(&app, "shutdown", json!({})).await {
                return r;
            }
        }
        let _ = app.stop.send(true);
        return Json(json!({"ok":true,"sessions_preserved":!all})).into_response();
    }
    match call(&app, method, params).await {
        Ok(v) => (StatusCode::from_u16(status).unwrap(), Json(v)).into_response(),
        Err(r) => r,
    }
}
#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: u64,
}
fn default_limit() -> u64 {
    1000
}
async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
    headers: HeaderMap,
) -> Response {
    let mut after = q.after;
    if let Some(v) = headers.get("last-event-id") {
        match v.to_str().ok().and_then(|s| s.parse::<u64>().ok()) {
            Some(n) => after = after.max(n),
            None => return error(422, "Invalid Last-Event-ID"),
        }
    }
    if after > i64::MAX as u64 {
        return error(422, "Invalid event cursor");
    }
    let snapshot = match call(&app, "sync", json!({"session_id":id})).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let stop = app.stop.subscribe();
    let stream = async_stream::stream! {
    yield Ok::<Event,Infallible>(Event::default().data(json!({"type":"stream_sync","session_id":id,"data":{"replay_through":snapshot["last_seq"],"first_seq":snapshot["first_seq"],"session":snapshot["session"],"commands":snapshot["commands"]}}).to_string()));
    loop {if *stop.borrow(){break}match call(&app,"events",json!({"session_id":id,"after":after,"limit":200})).await{
    Ok(batch)=>{let Some(items)=batch.as_array() else{break};for item in items{let Some(seq)=item["seq"].as_u64() else{continue};if seq<=after{continue}after=seq;yield Ok(Event::default().id(seq.to_string()).data(item.to_string()));}if items.is_empty(){tokio::time::sleep(Duration::from_millis(250)).await;}},
    Err(_)=>{yield Ok(Event::default().event("error").data("{\"detail\":\"Broker unavailable\"}"));break}}
    }
    };
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    response
}
async fn static_file(req: Request) -> Response {
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        return error(405, "Method not allowed");
    }
    macro_rules! asset {
        ($name:literal,$mime:literal) => {
            ($mime, include_bytes!(concat!("../web/", $name)).as_slice())
        };
    }
    let (mime, data) = match req.uri().path() {
        "/" | "/index.html" => asset!("index.html", "text/html; charset=utf-8"),
        "/style.css" => asset!("style.css", "text/css"),
        "/app.js" => asset!("app.js", "text/javascript"),
        "/ansi-text.js" => asset!("ansi-text.js", "text/javascript"),
        "/terminal-input.js" => asset!("terminal-input.js", "text/javascript"),
        "/link-safety.js" => asset!("link-safety.js", "text/javascript"),
        "/replay-state.js" => asset!("replay-state.js", "text/javascript"),
        "/stream-limits.js" => asset!("stream-limits.js", "text/javascript"),
        "/submission-state.js" => asset!("submission-state.js", "text/javascript"),
        "/input-transport.js" => asset!("input-transport.js", "text/javascript"),
        "/vendor/xterm.js" => asset!("vendor/xterm.js", "text/javascript"),
        "/vendor/xterm.css" => asset!("vendor/xterm.css", "text/css"),
        _ => return error(404, "Not found"),
    };
    ([(header::CONTENT_TYPE, mime)], Body::from(data)).into_response()
}
pub fn router(rpc: RpcClient, port: u16, stop: watch::Sender<bool>) -> Router {
    let app = App { rpc, port, stop };
    let mut routes = Router::new()
        .route("/health", get(|| async { Json(json!({"ok":true})) }))
        .route("/api/sessions/:id/events", get(events));
    for path in [
        "/api/status",
        "/api/sessions",
        "/api/sessions/:id",
        "/api/sessions/:id/commands",
        "/api/sessions/:id/commands/:command_id",
        "/api/sessions/:id/history",
        "/api/sessions/:id/log",
        "/api/sessions/:id/input",
        "/api/sessions/:id/interrupt",
        "/api/sessions/:id/resize",
        "/api/shutdown",
    ] {
        routes = routes.route(path, axum::routing::any(api));
    }
    routes
        .fallback(static_file)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
pub async fn serve(home: PathBuf, port: u16) -> anyhow::Result<()> {
    anyhow::ensure!(port != 0, "Port must be nonzero");
    let rpc = RpcClient::new(&home)?;
    let (tx, mut rx) = watch::channel(false);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    eprintln!("AgentBox: http://127.0.0.1:{port} (local user privileges; never share your token)");
    axum::serve(listener, router(rpc, port, tx.clone()))
        .with_graceful_shutdown(async move {
            tokio::select! {_=tokio::signal::ctrl_c()=>{let _=tx.send(true);},_=rx.changed()=>{}}
        })
        .await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_is_strict() {
        for good in ["localhost:8765", "127.0.0.1:8765", "[::1]:8765"] {
            assert!(valid_authority(good, 8765));
        }
        for bad in [
            "localhost",
            "localhost:80",
            "evil:8765",
            "localhost.evil:8765",
            "127.0.0.1@evil:8765",
            "::1:8765",
            "localhost:08765",
        ] {
            assert!(!valid_authority(bad, 8765));
        }
    }
    #[test]
    fn rejects_unsafe_input() {
        assert!(!validate(
            "input",
            &json!({"data":"x","source":"terminal_reply"})
        ));
        assert!(!validate("run", &json!({"command":" ","extra":1})));
        assert!(!validate("resize", &json!({"rows":true,"cols":80})));
        assert!(validate(
            "run",
            &json!({"command":"echo hello","request_id":"safe:1"})
        ));
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use tower::ServiceExt;
    fn app() -> (tempfile::TempDir, Router, String) {
        let d = tempfile::tempdir().unwrap();
        let home = crate::config::init_home(&d.path().join("state")).unwrap();
        let rpc = RpcClient::new(&home).unwrap();
        let token = rpc.token.clone();
        let (tx, _) = watch::channel(false);
        (d, router(rpc, 8765, tx), token)
    }
    #[tokio::test]
    async fn static_and_auth_work_without_sockets() {
        let (_d, app, _token) = app();
        let request = |path: &str| {
            axum::http::Request::builder()
                .uri(path)
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap()
        };
        let r = app.clone().oneshot(request("/health")).await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        let r = app.clone().oneshot(request("/api/status")).await.unwrap();
        assert_eq!(r.status(), 401);
        let r = app.clone().oneshot(request("/index.html")).await.unwrap();
        assert_eq!(r.status(), 200);
        let r = app.oneshot(request("/../Cargo.toml")).await.unwrap();
        assert_eq!(r.status(), 404);
    }
    #[tokio::test]
    async fn origin_and_port_rejected() {
        let (_d, app, _) = app();
        for (host, origin) in [
            ("localhost:80", "http://localhost:80"),
            ("localhost:8765", "http://evil.example"),
            ("evil.example:8765", "http://evil.example:8765"),
        ] {
            let req = axum::http::Request::builder()
                .uri("/health")
                .header("host", host)
                .header("origin", origin)
                .body(Body::empty())
                .unwrap();
            assert_eq!(app.clone().oneshot(req).await.unwrap().status(), 403);
        }
    }
    #[tokio::test]
    async fn body_validation_precedes_broker_call() {
        let (_d, app, token) = app();
        for (path, body, status) in [
            ("/api/sessions", "{\"rows\":true}".to_owned(), 422),
            (
                "/api/sessions/s/input",
                "{\"data\":\"x\",\"source\":\"terminal_reply\"}".to_owned(),
                422,
            ),
            (
                "/api/sessions/s/commands",
                "{\"command\":\"x\",\"command\":\"y\"}".to_owned(),
                422,
            ),
            ("/api/sessions", "x".repeat(MAX_BODY + 1), 413),
        ] {
            let req = axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "localhost:8765")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status().as_u16(),
                status
            );
        }
    }
}
