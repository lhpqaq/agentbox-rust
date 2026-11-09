//! Durable metadata and bounded replay journal. No process checkpoints.
use crate::{
    config,
    error::{AbError, Result},
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
pub struct Store {
    db: Connection,
    pub event_limit: usize,
    pub output_limit: usize,
    pub command_limit: usize,
    pub command_bytes: usize,
}
fn parse(s: String) -> Result<Value> {
    Ok(serde_json::from_str(&s)?)
}
fn sid(v: &Value) -> &str {
    v["session_id"].as_str().unwrap_or("")
}
fn event(tx: &Transaction<'_>, session: &str, kind: &str, data: &Value) -> Result<i64> {
    let text = serde_json::to_string(data)?;
    tx.execute(
        "INSERT INTO events(session_id,kind,data,created_at,bytes) VALUES(?1,?2,?3,?4,?5)",
        params![session, kind, text, config::now(), text.len() as i64],
    )?;
    Ok(tx.last_insert_rowid())
}
fn save_session(tx: &Transaction<'_>, session: &Value) -> Result<()> {
    tx.execute("INSERT INTO sessions(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![session["id"].as_str(),session.to_string()])?;
    Ok(())
}
fn save_command(tx: &Transaction<'_>, c: &Value) -> Result<()> {
    tx.execute("INSERT INTO commands(id,session_id,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![c["id"].as_str(),sid(c),c.to_string()])?;
    if let Some(key) = c["request_id"].as_str() {
        let mut summary = c.clone();
        summary.as_object_mut().unwrap().remove("command");
        let hash = format!(
            "{:x}",
            Sha256::digest(c["command"].as_str().unwrap_or("").as_bytes())
        );
        tx.execute("INSERT INTO requests(session_id,request_id,command_id,hash,data) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session_id,request_id) DO UPDATE SET data=excluded.data",params![sid(c),key,c["id"].as_str(),hash,summary.to_string()])?;
    }
    Ok(())
}
impl Store {
    pub fn open(home: &Path) -> Result<Self> {
        let p = home.join("state.db");
        drop(config::private_file(&p, true)?);
        // Reject SQLite sidecar symlinks before opening. Private home prevents other users racing these checks.
        for ext in ["state.db-wal", "state.db-shm"] {
            if home.join(ext).symlink_metadata().is_ok() {
                drop(config::private_file(&home.join(ext), false)?);
            }
        }
        let db = Connection::open(p)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,data TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS commands(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS commands_session ON commands(session_id);
        CREATE TABLE IF NOT EXISTS requests(session_id TEXT NOT NULL,request_id TEXT NOT NULL,command_id TEXT NOT NULL,hash TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(session_id,request_id));
        CREATE INDEX IF NOT EXISTS requests_command ON requests(command_id);
        CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL,created_at REAL NOT NULL,bytes INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS events_session ON events(session_id,seq);")?;
        Ok(Self {
            db,
            event_limit: 20000,
            output_limit: 8 * 1024 * 1024,
            command_limit: 500,
            command_bytes: 4 * 1024 * 1024,
        })
    }
    pub fn sessions(&self) -> Result<Vec<Value>> {
        let mut stmt = self
            .db
            .prepare("SELECT data FROM sessions ORDER BY rowid DESC")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|x| parse(x?))
            .collect();
        rows
    }
    pub fn session(&self, id: &str) -> Result<Value> {
        let text = self
            .db
            .query_row("SELECT data FROM sessions WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        text.map(parse)
            .transpose()?
            .ok_or_else(|| AbError::new(404, "Session not found"))
    }
    pub fn update_session(&mut self, mut value: Value, kind: &str) -> Result<Value> {
        value["updated_at"] = json!(config::now());
        let id = value["id"].as_str().unwrap().to_owned();
        let limits = self.limits();
        let tx = self.db.transaction()?;
        save_session(&tx, &value)?;
        event(&tx, &id, kind, &value)?;
        prune_conn(&tx, &id, limits)?;
        tx.commit()?;
        Ok(value)
    }
    pub fn output(
        &mut self,
        id: &str,
        text: String,
        command: Option<String>,
        prompt: bool,
    ) -> Result<()> {
        let limits = self.limits();
        let tx = self.db.transaction()?;
        event(
            &tx,
            id,
            "output",
            &json!({"text":text,"command_id":command,"prompt":prompt}),
        )?;
        prune_conn(&tx, id, limits)?;
        tx.commit()?;
        Ok(())
    }
    pub fn start_command(&mut self, mut command: Value) -> Result<Value> {
        let id = sid(&command).to_owned();
        let mut session = self.session(&id)?;
        session["status"] = json!("running");
        session["updated_at"] = json!(config::now());
        session["command_count"] = json!(session["command_count"].as_u64().unwrap_or(0) + 1);
        let limits = self.limits();
        let tx = self.db.transaction()?;
        command["start_seq"] = json!(event(&tx, &id, "command_started", &command)?);
        save_command(&tx, &command)?;
        save_session(&tx, &session)?;
        event(&tx, &id, "session_updated", &session)?;
        prune_conn(&tx, &id, limits)?;
        tx.commit()?;
        Ok(command)
    }
    pub fn finish_command(
        &mut self,
        id: &str,
        command_id: &str,
        status: &str,
        exit: Option<i32>,
        cwd: Option<&str>,
    ) -> Result<()> {
        let mut command = match self.raw_command(id, command_id)? {
            Some(c) => c,
            None => return Ok(()),
        };
        if command["status"] != "running" {
            return Ok(());
        }
        let now = config::now();
        command["status"] = json!(status);
        command["exit_code"] = json!(exit);
        command["finished_at"] = json!(now);
        command["duration_ms"] =
            json!(((now - command["started_at"].as_f64().unwrap_or(now)) * 1000.0).max(0.0));
        if let Some(cwd) = cwd {
            command["cwd"] = json!(cwd);
        }
        let mut session = self.session(id)?;
        if session["status"] != "closed" {
            session["status"] = json!("idle");
        }
        if let Some(cwd) = cwd {
            session["cwd"] = json!(cwd);
        }
        session["updated_at"] = json!(now);
        session["exit_code"] = json!(exit);
        let limits = self.limits();
        let tx = self.db.transaction()?;
        command["last_seq"] = json!(event(&tx, id, "command_finished", &command)?);
        save_command(&tx, &command)?;
        save_session(&tx, &session)?;
        event(&tx, id, "session_updated", &session)?;
        prune_conn(&tx, id, limits)?;
        tx.commit()?;
        Ok(())
    }
    pub fn close_session(&mut self, id: &str, code: Option<i32>, reason: &str) -> Result<()> {
        let mut session = self.session(id)?;
        let mut pending = self.commands(id)?;
        // Key summaries can outlive normal command history. Reconcile those too.
        let mut stmt = self
            .db
            .prepare("SELECT data FROM requests WHERE session_id=?1")?;
        let keys: Vec<String> = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        for key in keys {
            let c = parse(key)?;
            if c["status"] == "running" && !pending.iter().any(|v| v["id"] == c["id"]) {
                pending.push(c);
            }
        }
        let limits = self.limits();
        let tx = self.db.transaction()?;
        for mut c in pending.into_iter().filter(|c| c["status"] == "running") {
            c["status"] = json!(if code.is_some() { "exited" } else { "lost" });
            c["exit_code"] = json!(code);
            c["finished_at"] = json!(config::now());
            c["duration_ms"] = Value::Null;
            c["last_seq"] = json!(event(&tx, id, "command_finished", &c)?);
            tx.execute(
                "UPDATE commands SET data=?1 WHERE id=?2",
                params![c.to_string(), c["id"].as_str()],
            )?;
            c.as_object_mut().unwrap().remove("command");
            tx.execute(
                "UPDATE requests SET data=?1 WHERE session_id=?2 AND command_id=?3",
                params![c.to_string(), id, c["id"].as_str()],
            )?;
        }
        session["status"] = json!("closed");
        session["exit_code"] = json!(code);
        session["closed_reason"] = json!(reason);
        session["updated_at"] = json!(config::now());
        save_session(&tx, &session)?;
        event(&tx, id, "session_closed", &session)?;
        prune_conn(&tx, id, limits)?;
        tx.commit()?;
        Ok(())
    }
    pub fn recover(&mut self) -> Result<()> {
        for s in self.sessions()? {
            let id = s["id"].as_str().unwrap();
            let running = self.commands(id)?.iter().any(|c| c["status"] == "running");
            let keys_running:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM requests WHERE session_id=?1 AND json_extract(data,'$.status')='running')",[id],|r|r.get(0))?;
            if s["status"] != "closed" || running || keys_running {
                self.close_session(
                    id,
                    None,
                    "Broker restarted; live shell state was not recovered",
                )?;
            }
        }
        Ok(())
    }
    fn raw_command(&self, id: &str, cid: &str) -> Result<Option<Value>> {
        let c = self
            .db
            .query_row(
                "SELECT data FROM commands WHERE session_id=?1 AND id=?2",
                params![id, cid],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        let c = match c {
            Some(c) => Some(c),
            None => self
                .db
                .query_row(
                    "SELECT data FROM requests WHERE session_id=?1 AND command_id=?2",
                    params![id, cid],
                    |r| r.get::<_, String>(0),
                )
                .optional()?,
        };
        c.map(parse).transpose()
    }
    fn with_retention(&self, id: &str, mut c: Value) -> Result<Value> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM commands WHERE id=?1 AND session_id=?2)",
            params![c["id"].as_str(), id],
            |r| r.get(0),
        )?;
        let start = c["start_seq"].as_i64().unwrap_or(0);
        let retained: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND seq=?2)",
            params![id, start],
            |r| r.get(0),
        )?;
        c["history_retained"] = json!(exists);
        c["output_may_be_truncated"] = json!(!retained);
        Ok(c)
    }
    pub fn command(&self, id: &str, cid: &str) -> Result<Value> {
        let c = self
            .raw_command(id, cid)?
            .ok_or_else(|| AbError::new(404, "Command not found"))?;
        self.with_retention(id, c)
    }
    pub fn commands(&self, id: &str) -> Result<Vec<Value>> {
        let mut stmt = self
            .db
            .prepare("SELECT data FROM commands WHERE session_id=?1 ORDER BY rowid DESC")?;
        let rows: Vec<String> = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        rows.into_iter()
            .map(|s| self.with_retention(id, parse(s)?))
            .collect()
    }
    pub fn prior_request(&self, id: &str, key: &str, text: &str) -> Result<Option<Value>> {
        let record = self
            .db
            .query_row(
                "SELECT hash,data FROM requests WHERE session_id=?1 AND request_id=?2",
                params![id, key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        match record {
            None => Ok(None),
            Some((hash, data)) => {
                if hash != format!("{:x}", Sha256::digest(text.as_bytes())) {
                    return Err(AbError::new(
                        409,
                        "request_id was already used for a different command",
                    ));
                }
                let mut c = self.with_retention(id, parse(data)?)?;
                c["command"] = json!(text);
                c["replayed"] = json!(true);
                Ok(Some(c))
            }
        }
    }
    pub fn request_count(&self, id: &str) -> Result<i64> {
        Ok(self.db.query_row(
            "SELECT COUNT(*) FROM requests WHERE session_id=?1",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn cursor(&self, id: &str) -> Result<Value> {
        let (first, last): (i64, i64) = self.db.query_row(
            "SELECT COALESCE(MIN(seq),0),COALESCE(MAX(seq),0) FROM events WHERE session_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(json!({"first_seq":first,"last_seq":last,"replay_through":last}))
    }
    pub fn events(&self, id: &str, after: i64, limit: usize) -> Result<Vec<Value>> {
        let mut stmt=self.db.prepare("SELECT seq,kind,data,created_at FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
        let rows = stmt.query_map(params![id, after, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (seq, kind, data, time) = r?;
            Ok(json!({"seq":seq,"session_id":id,"type":kind,"data":parse(data)?,"created_at":time}))
        })
        .collect()
    }
    fn limits(&self) -> (usize, usize, usize, usize) {
        (
            self.event_limit,
            self.output_limit,
            self.command_limit,
            self.command_bytes,
        )
    }
    pub fn prune(&mut self, id: &str) -> Result<()> {
        prune_conn(&self.db, id, self.limits())
    }
}
fn prune_conn(db: &Connection, id: &str, limits: (usize, usize, usize, usize)) -> Result<()> {
    let (event_limit, output_limit, command_limit, command_bytes) = limits;
    // Keep a contiguous event suffix; clipping is exposed by missing start_seq.
    let (count, bytes): (i64, i64) = db.query_row(
        "SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM events WHERE session_id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count > event_limit as i64 || bytes > output_limit as i64 {
        let mut stmt =
            db.prepare("SELECT seq,bytes FROM events WHERE session_id=?1 ORDER BY seq DESC")?;
        let rows: Vec<(i64, i64)> = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        let mut size = 0i64;
        let mut first = 0i64;
        for (kept, (seq, n)) in rows.into_iter().enumerate() {
            if kept > 0 && (kept >= event_limit || size + n > output_limit as i64) {
                break;
            }
            size += n;
            first = seq;
        }
        db.execute(
            "DELETE FROM events WHERE session_id=?1 AND seq<?2",
            params![id, first],
        )?;
    }
    let mut stmt=db.prepare("SELECT id,length(CAST(data AS BLOB)),json_extract(data,'$.status') FROM commands WHERE session_id=?1 ORDER BY rowid DESC")?;
    let rows: Vec<(String, i64, String)> = stmt
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (cid, size, status) in rows {
        if status == "running" {
            continue;
        }
        count += 1;
        bytes += size as usize;
        if count > command_limit || bytes > command_bytes {
            db.execute(
                "DELETE FROM commands WHERE id=?1 AND session_id=?2",
                params![cid, id],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let home = config::init_home(&d.path().join("state")).unwrap();
        let mut s = Store::open(&home).unwrap();
        s.update_session(
            json!({"id":"s","status":"idle","cwd":"/tmp","command_count":0}),
            "session_created",
        )
        .unwrap();
        (d, s)
    }
    fn command(id: &str, key: &str) -> Value {
        json!({"id":id,"session_id":"s","command":"printf hi","status":"running","started_at":config::now(),"request_id":key})
    }
    #[test]
    fn durable_key_conflict_and_finish() {
        let (_d, mut s) = setup();
        let c = s.start_command(command("a", "key")).unwrap();
        assert!(c["start_seq"].as_i64().unwrap() > 0);
        s.finish_command("s", "a", "completed", Some(0), Some("/tmp"))
            .unwrap();
        assert_eq!(
            s.prior_request("s", "key", "printf hi").unwrap().unwrap()["status"],
            "completed"
        );
        assert_eq!(s.prior_request("s", "key", "bad").unwrap_err().code, 409);
    }
    #[test]
    fn pruning_keeps_key() {
        let (_d, mut s) = setup();
        s.command_limit = 1;
        s.event_limit = 3;
        s.start_command(command("a", "key")).unwrap();
        s.finish_command("s", "a", "completed", Some(0), None)
            .unwrap();
        s.start_command(command("b", "second")).unwrap();
        s.finish_command("s", "b", "completed", Some(0), None)
            .unwrap();
        let c = s.prior_request("s", "key", "printf hi").unwrap().unwrap();
        assert_eq!(c["history_retained"], false);
        assert_eq!(c["output_may_be_truncated"], true);
    }
    #[test]
    fn recovery_is_lost_not_execution() {
        let (_d, mut s) = setup();
        s.start_command(command("a", "key")).unwrap();
        s.recover().unwrap();
        assert_eq!(s.session("s").unwrap()["status"], "closed");
        assert_eq!(s.command("s", "a").unwrap()["status"], "lost");
        assert_eq!(
            s.prior_request("s", "key", "printf hi").unwrap().unwrap()["status"],
            "lost"
        );
    }
    #[test]
    fn completion_event_and_record_match() {
        let (_d, mut s) = setup();
        s.start_command(command("a", "key")).unwrap();
        s.finish_command("s", "a", "completed", Some(3), None)
            .unwrap();
        let c = s.command("s", "a").unwrap();
        let e = s
            .events("s", c["last_seq"].as_i64().unwrap() - 1, 1)
            .unwrap();
        assert_eq!(e[0]["type"], "command_finished");
        assert_eq!(e[0]["data"]["exit_code"], 3);
    }
    #[test]
    fn completion_rolls_back_on_event_failure() {
        let (_d, mut s) = setup();
        s.start_command(command("a", "key")).unwrap();
        s.db.execute_batch("CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(FAIL,'injected'); END;").unwrap();
        assert!(s
            .finish_command("s", "a", "completed", Some(0), None)
            .is_err());
        assert_eq!(s.command("s", "a").unwrap()["status"], "running");
        assert_eq!(
            s.prior_request("s", "key", "printf hi").unwrap().unwrap()["status"],
            "running"
        );
    }
    #[test]
    fn start_rolls_back_if_pruning_fails() {
        let (_d, mut s) = setup();
        s.event_limit = 1;
        s.db.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON events BEGIN SELECT RAISE(FAIL,'injected'); END;").unwrap();
        assert!(s.start_command(command("a", "key")).is_err());
        assert_eq!(s.command("s", "a").unwrap_err().code, 404);
        assert!(s.prior_request("s", "key", "printf hi").unwrap().is_none());
        assert_eq!(s.session("s").unwrap()["status"], "idle");
    }
    #[test]
    fn command_byte_retention_keeps_tombstone() {
        let (_d, mut s) = setup();
        s.command_bytes = 1;
        s.start_command(command("a", "key")).unwrap();
        s.finish_command("s", "a", "finished", Some(0), None)
            .unwrap();
        assert!(s.commands("s").unwrap().is_empty());
        assert_eq!(
            s.prior_request("s", "key", "printf hi").unwrap().unwrap()["history_retained"],
            false
        );
    }
    #[test]
    fn known_shell_exit_is_preserved() {
        let (_d, mut s) = setup();
        s.start_command(command("a", "key")).unwrap();
        s.close_session("s", Some(7), "Shell exited").unwrap();
        let c = s.command("s", "a").unwrap();
        assert_eq!(c["status"], "exited");
        assert_eq!(c["exit_code"], 7);
    }
}
