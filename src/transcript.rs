//! Bounded, non-mutating transcript pages for shared interactive terminals.
use crate::{
    error::{AbError, Result},
    rpc::RpcClient,
};
use serde_json::{json, Value};

pub fn read(client: &RpcClient, id: &str, after: i64, max_bytes: usize) -> Result<Value> {
    read_using(id, after, max_bytes, |method, params| {
        client.call(method, params)
    })
}

fn read_using(
    id: &str,
    after: i64,
    max_bytes: usize,
    mut call: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<Value> {
    if after < 0 || !(1..=1024 * 1024).contains(&max_bytes) {
        return Err(AbError::new(
            422,
            "after must be nonnegative and max-bytes must be 1..1048576",
        ));
    }
    match call(
        "read",
        json!({"session_id":id,"after":after,"max_bytes":max_bytes}),
    ) {
        Ok(result) => return Ok(result),
        Err(error)
            if error.from_broker
                && error.code == 404
                && error.message == "Unknown broker method" => {}
        Err(error) => return Err(error),
    }
    // Older brokers can stay running with their existing shells. Only the exact
    // unsupported-method response selects this read-only compatibility path.
    let snapshot = call("sync", json!({"session_id":id}))?;
    let events = call("events", json!({"session_id":id,"after":after,"limit":64}))?;
    let retained = call("cursor", json!({"session_id":id}))?;
    from_snapshot(&snapshot, &events, &retained, after, max_bytes)
}

pub(crate) fn from_snapshot(
    snapshot: &Value,
    events: &Value,
    retained: &Value,
    after: i64,
    max_bytes: usize,
) -> Result<Value> {
    let through = snapshot["last_seq"]
        .as_i64()
        .ok_or_else(|| AbError::new(502, "Missing snapshot cursor"))?;
    if after > through {
        return Err(AbError::new(
            422,
            "Cursor is ahead of this session; check the session ID and home",
        ));
    }
    let mut result = page(events, after, through, max_bytes)?;
    // A second retention observation detects pruning during the read. Global
    // sequence gaps do not prove loss, so report uncertainty rather than invent it.
    let first = retained["first_seq"]
        .as_i64()
        .ok_or_else(|| AbError::new(502, "Missing retention cursor"))?;
    let begins_at_creation = events
        .as_array()
        .and_then(|v| v.first())
        .is_some_and(|e| e["type"] == "session_created");
    result["output_may_be_truncated"] = json!(if after == 0 {
        !begins_at_creation || first > snapshot["first_seq"].as_i64().unwrap_or(0)
    } else {
        first > after.saturating_add(1)
    });
    result["session"] = snapshot["session"].clone();
    result["first_retained_seq"] = json!(first);
    result["snapshot_through"] = json!(through);
    Ok(result)
}

fn page(events: &Value, after: i64, through: i64, max_bytes: usize) -> Result<Value> {
    let events = events
        .as_array()
        .ok_or_else(|| AbError::new(502, "Invalid events response"))?;
    let mut cursor = after;
    let mut output = String::new();
    let mut next_event_bytes = 0;
    for event in events {
        let seq = event["seq"]
            .as_i64()
            .ok_or_else(|| AbError::new(502, "Invalid event sequence"))?;
        if seq > through {
            break;
        }
        if seq <= cursor {
            return Err(AbError::new(502, "Non-increasing event sequence"));
        }
        if event["type"] == "output" {
            let text = event["data"]["text"]
                .as_str()
                .ok_or_else(|| AbError::new(502, "Invalid output event"))?;
            if text.len() > max_bytes - output.len() {
                next_event_bytes = text.len();
                break;
            }
            output.push_str(text);
        }
        cursor = seq;
    }
    Ok(
        json!({"output":output,"output_bytes":output.len(),"after":cursor,
        "has_more":cursor < through,"next_event_bytes":next_event_bytes}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_broker_uses_one_read_and_legacy_broker_remains_compatible() {
        let mut calls = Vec::new();
        let result = read_using("s", 0, 100, |method, _| {
            calls.push(method.to_owned());
            Ok(json!({"output":"fast"}))
        })
        .unwrap();
        assert_eq!(result["output"], "fast");
        assert_eq!(calls, ["read"]);
        calls.clear();
        let result = read_using("s", 0, 100, |method, _| {
            calls.push(method.to_owned());
            match method {
                "read" => { let mut e = AbError::new(404, "Unknown broker method"); e.from_broker = true; Err(e) },
                "sync" => Ok(json!({"session":{"id":"s"},"first_seq":1,"last_seq":2})),
                "events" => Ok(json!([{"seq":1,"type":"session_created"},{"seq":2,"type":"output","data":{"text":"legacy"}}])),
                "cursor" => Ok(json!({"first_seq":1,"last_seq":2})),
                _ => panic!("unexpected method"),
            }
        }).unwrap();
        assert_eq!(calls, ["read", "sync", "events", "cursor"]);
        assert_eq!(result["output"], "legacy");
        assert_eq!(result["output_may_be_truncated"], false);
    }
    #[test]
    fn transport_or_session_errors_never_trigger_fallback() {
        for message in ["Session not found", "Connection interrupted"] {
            let mut calls = 0;
            let result = read_using("s", 0, 100, |_, _| {
                calls += 1;
                let mut error = AbError::new(404, message);
                error.from_broker = true;
                Err(error)
            });
            assert!(result.is_err());
            assert_eq!(calls, 1);
        }
    }
    #[test]
    fn bounded_pages_never_skip_or_split_unicode_output() {
        let events = json!([
            {"seq":1,"type":"session_created"},
            {"seq":3,"type":"output","data":{"text":"你好"}},
            {"seq":6,"type":"output","data":{"text":"world"}}
        ]);
        let first = page(&events, 0, 6, 7).unwrap();
        assert_eq!(first["output"], "你好");
        assert_eq!(first["after"], 3);
        assert_eq!(first["next_event_bytes"], 5);
        let second = page(&json!([events[2]]), 3, 6, 7).unwrap();
        assert_eq!(second["output"], "world");
        assert_eq!(second["has_more"], false);
        let small = page(&events, 0, 6, 1).unwrap();
        assert_eq!(small["after"], 1);
        assert_eq!(small["next_event_bytes"], 6);
    }
    #[test]
    fn snapshot_boundary_and_metadata_only_pages() {
        let p = page(&json!([{"seq":2,"type":"session_updated"},{"seq":9,"type":"output","data":{"text":"later"}}]), 0, 2, 1).unwrap();
        assert_eq!(p["output"], "");
        assert_eq!(p["after"], 2);
        assert_eq!(p["has_more"], false);
    }
}
