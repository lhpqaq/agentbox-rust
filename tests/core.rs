//! Real Bash + PTY + SQLite integration, without any network or Unix sockets.
use agentbox::{broker::Core, config};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
fn setup() -> (tempfile::TempDir, Core) {
    let d = tempfile::tempdir().unwrap();
    let h = config::init_home(&d.path().join("state")).unwrap();
    let c = Core::new(&h).unwrap();
    (d, c)
}
fn create(c: &mut Core) -> String {
    c.dispatch("create", json!({"name":"test","cwd":"/tmp"}))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}
fn run(c: &mut Core, s: &str, command: &str, key: &str) -> Value {
    c.dispatch(
        "run",
        json!({"session_id":s,"command":command,"request_id":key}),
    )
    .unwrap()
}
fn wait(c: &mut Core, s: &str, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = c
            .dispatch("command", json!({"session_id":s,"command_id":id}))
            .unwrap();
        if r["status"] != "running" {
            return r;
        }
        assert!(Instant::now() < deadline, "command did not finish: {r}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn output(c: &mut Core, s: &str, id: &str) -> String {
    let events = c
        .dispatch("events", json!({"session_id":s,"after":0,"limit":1000}))
        .unwrap();
    events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["type"] == "output" && e["data"]["command_id"] == id && e["data"]["prompt"] != true
        })
        .filter_map(|e| e["data"]["text"].as_str())
        .collect()
}
#[test]
fn state_is_persistent_and_sessions_are_independent() {
    let (_d, mut c) = setup();
    let a = create(&mut c);
    let b = create(&mut c);
    let r = run(
        &mut c,
        &a,
        "cd /; export AB_TEST=hello; ab_fn(){ printf 'function:%s' \"$AB_TEST\"; }",
        "a",
    );
    wait(&mut c, &a, r["id"].as_str().unwrap());
    let r = run(&mut c, &a, "pwd; ab_fn", "b");
    let id = r["id"].as_str().unwrap();
    assert_eq!(wait(&mut c, &a, id)["exit_code"], 0);
    let text = output(&mut c, &a, id);
    assert!(text.contains("function:hello"), "{text:?}");
    let r = run(&mut c, &b, "printf '%s' \"${AB_TEST-unset}\"", "a");
    let id = r["id"].as_str().unwrap();
    wait(&mut c, &b, id);
    assert!(output(&mut c, &b, id).contains("unset"));
}
#[test]
fn keyed_submission_runs_once_and_survives_reopen() {
    let (d, mut c) = setup();
    let s = create(&mut c);
    let r = run(
        &mut c,
        &s,
        "AB_N=$(( ${AB_N:-0} + 1 )); printf '%s' \"$AB_N\"",
        "once",
    );
    let id = r["id"].as_str().unwrap();
    wait(&mut c, &s, id);
    let replay = run(
        &mut c,
        &s,
        "AB_N=$(( ${AB_N:-0} + 1 )); printf '%s' \"$AB_N\"",
        "once",
    );
    assert_eq!(replay["id"], r["id"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        c.dispatch(
            "run",
            json!({"session_id":s,"command":"other","request_id":"once"})
        )
        .unwrap_err()
        .code,
        409
    );
    drop(c);
    let mut c = Core::new(&d.path().join("state")).unwrap();
    let replay = run(
        &mut c,
        &s,
        "AB_N=$(( ${AB_N:-0} + 1 )); printf '%s' \"$AB_N\"",
        "once",
    );
    assert_eq!(replay["status"], "finished");
    assert_eq!(replay["id"], r["id"]);
}
#[test]
fn interrupt_preserves_shell_and_raw_input_is_conservative() {
    let (_d, mut c) = setup();
    let s = create(&mut c);
    let r = run(&mut c, &s, "sleep 30", "sleep");
    c.dispatch("interrupt", json!({"session_id":s})).unwrap();
    wait(&mut c, &s, r["id"].as_str().unwrap());
    let r = run(&mut c, &s, "read -r x; printf 'read:%s' \"$x\"", "read");
    c.dispatch("input", json!({"session_id":s,"data":"hello\n"}))
        .unwrap();
    wait(&mut c, &s, r["id"].as_str().unwrap());
    assert!(output(&mut c, &s, r["id"].as_str().unwrap()).contains("read:hello"));
    assert_eq!(
        c.dispatch("run", json!({"session_id":s,"command":"true"}))
            .unwrap_err()
            .code,
        409
    );
}
#[test]
fn closing_active_session_records_exit() {
    let (_d, mut c) = setup();
    let s = create(&mut c);
    let r = run(&mut c, &s, "sleep 30", "key");
    c.dispatch("close", json!({"session_id":s})).unwrap();
    assert_eq!(
        c.dispatch("command", json!({"session_id":s,"command_id":r["id"]}))
            .unwrap()["status"],
        "exited"
    );
    assert_eq!(
        c.dispatch("detail", json!({"session_id":s})).unwrap()["status"],
        "closed"
    );
}
