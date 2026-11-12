//! Resolve human selectors once, then pin subsequent operations to a session ID.
use crate::{
    error::{AbError, Result},
    rpc::RpcClient,
};
use serde_json::{json, Value};

pub fn resolve(client: &RpcClient, selector: &str) -> Result<String> {
    // Full IDs keep the existing direct-access path and broker validation.
    if selector.len() == 32 && selector.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(selector.to_owned());
    }
    select(&client.call("sessions", json!({}))?, selector)
}

fn select(sessions: &Value, selector: &str) -> Result<String> {
    if selector.is_empty() {
        return Err(AbError::new(422, "Session selector must not be empty"));
    }
    let records = sessions
        .as_array()
        .ok_or_else(|| AbError::new(502, "Invalid sessions response"))?;
    if let Some(id) = records
        .iter()
        .filter_map(|s| s["id"].as_str())
        .find(|id| *id == selector)
    {
        return Ok(id.to_owned());
    }
    let matches: Vec<&str> = records
        .iter()
        .filter(|s| {
            s["name"].as_str() == Some(selector)
                || (selector.len() >= 6
                    && s["id"].as_str().is_some_and(|id| id.starts_with(selector)))
        })
        .filter_map(|s| s["id"].as_str())
        .collect();
    match matches.as_slice() {
        [id] => Ok((*id).to_owned()),
        [] => Err(AbError::new(
            404,
            "No session matches this name or ID prefix (at least 6 characters)",
        )),
        _ => Err(AbError::new(
            409,
            format!(
                "Ambiguous session selector; use a full ID: {}",
                matches.join(", ")
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_names_and_unambiguous_prefixes() {
        let s = json!([{"id":"abcdef111","name":"build"},{"id":"fedcba222","name":"remote"}]);
        assert_eq!(select(&s, "build").unwrap(), "abcdef111");
        assert_eq!(select(&s, "fedcba").unwrap(), "fedcba222");
        assert_eq!(select(&s, "abc").unwrap_err().code, 404);
    }
    #[test]
    fn ambiguity_including_closed_sessions_never_picks_a_target() {
        let s = json!([{"id":"abcdef111","name":"build","status":"closed"},{"id":"abcdef222","name":"build","status":"idle"}]);
        assert_eq!(select(&s, "build").unwrap_err().code, 409);
        assert_eq!(select(&s, "abcdef").unwrap_err().code, 409);
        assert_eq!(select(&s, "abcdef111").unwrap(), "abcdef111");
    }
    #[test]
    fn name_and_prefix_collision_is_ambiguous() {
        let s = json!([{"id":"abcdef111","name":"one"},{"id":"fedcba222","name":"abcdef"}]);
        assert_eq!(select(&s, "abcdef").unwrap_err().code, 409);
    }
}
