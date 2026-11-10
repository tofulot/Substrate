use agent_protocol::{CallTrace, ContextSnapshot, RunTrace, Session};
use anyhow::Result;
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::collections::BTreeMap;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
impl Storage {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePool::connect_with(options).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, project TEXT NOT NULL, title TEXT NOT NULL, updated_at INTEGER NOT NULL, messages TEXT NOT NULL, input TEXT NOT NULL)").execute(&pool).await?;
        for statement in [
            "CREATE TABLE IF NOT EXISTS context_blobs (hash TEXT PRIMARY KEY, content TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS context_snapshots (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, revision INTEGER NOT NULL, manifest TEXT NOT NULL, UNIQUE(session_id,revision))",
            "CREATE TABLE IF NOT EXISTS session_context (session_id TEXT PRIMARY KEY, snapshot_id TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS run_traces (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, started_at INTEGER NOT NULL, status TEXT NOT NULL, data TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS call_traces (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, started_at INTEGER NOT NULL, status TEXT NOT NULL, data TEXT NOT NULL)",
            "CREATE INDEX IF NOT EXISTS call_traces_session ON call_traces(session_id,started_at)",
            "CREATE INDEX IF NOT EXISTS run_traces_session ON run_traces(session_id,started_at)",
        ] {
            sqlx::query(statement).execute(&pool).await?;
        }
        Ok(Self { pool })
    }
    /// Repair incomplete tool transactions after a process crash without replaying actions.
    pub async fn recover_interrupted(&self) -> Result<()> {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions")
            .fetch_all(&self.pool)
            .await?;
        for id in ids {
            let mut session = self.get(&id).await?;
            let mut changed = false;
            for message in &mut session.messages {
                if let Some(tool) = &mut message.tool
                    && ["running", "approval"].contains(&tool.status.as_str())
                {
                    tool.status = "failed".into();
                    tool.output = Some("Application stopped before this action completed. Inspect workspace state before retrying.".into());
                    changed = true;
                }
            }
            let pending: Vec<String> = session
                .input
                .iter()
                .filter(|v| v["type"] == "function_call")
                .filter_map(|v| v["call_id"].as_str())
                .filter(|call| {
                    !session
                        .input
                        .iter()
                        .any(|v| v["type"] == "function_call_output" && v["call_id"] == *call)
                })
                .map(str::to_owned)
                .collect();
            for call in pending {
                session.input.push(serde_json::json!({"type":"function_call_output","call_id":call,"output":"Application stopped before execution completed. Inspect workspace state before retrying."}));
                changed = true;
            }
            if changed {
                self.save(&session).await?;
            }
        }
        // A crashed stream cannot report a trustworthy duration or final usage.
        for table in ["call_traces", "run_traces"] {
            let rows: Vec<String> =
                sqlx::query_scalar(&format!("SELECT data FROM {table} WHERE status='running'"))
                    .fetch_all(&self.pool)
                    .await?;
            for row in rows {
                let mut value: serde_json::Value = serde_json::from_str(&row)?;
                value["status"] = serde_json::json!("interrupted");
                if table == "run_traces" {
                    // Recovery time is not the time the original process stopped.
                    value["ended_at"] = serde_json::Value::Null;
                }
                sqlx::query(&format!(
                    "UPDATE {table} SET status='interrupted',data=? WHERE id=?"
                ))
                .bind(value.to_string())
                .bind(value["id"].as_str())
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }
    pub async fn save_context(
        &self,
        manifest: &ContextSnapshot,
        contents: &BTreeMap<String, String>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (hash, content) in contents {
            sqlx::query("INSERT OR IGNORE INTO context_blobs (hash,content) VALUES (?,?)")
                .bind(hash)
                .bind(content)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("INSERT OR IGNORE INTO context_snapshots (id,session_id,revision,manifest) VALUES (?,?,?,?)")
            .bind(&manifest.id).bind(&manifest.session_id).bind(manifest.revision as i64).bind(serde_json::to_string(manifest)?).execute(&mut *tx).await?;
        sqlx::query("INSERT OR REPLACE INTO session_context (session_id,snapshot_id) VALUES (?,?)")
            .bind(&manifest.session_id)
            .bind(&manifest.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn context(
        &self,
        session_id: &str,
        snapshot: Option<&str>,
    ) -> Result<Option<ContextSnapshot>> {
        let row: Option<String> = if let Some(id) = snapshot {
            sqlx::query_scalar("SELECT manifest FROM context_snapshots WHERE session_id=? AND id=?")
                .bind(session_id)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
        } else {
            sqlx::query_scalar("SELECT manifest FROM context_snapshots WHERE id=(SELECT snapshot_id FROM session_context WHERE session_id=?) AND session_id=?").bind(session_id).bind(session_id).fetch_optional(&self.pool).await?
        };
        row.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub async fn context_history(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        let rows:Vec<String>=sqlx::query_scalar("SELECT manifest FROM context_snapshots WHERE session_id=? ORDER BY revision DESC LIMIT 30").bind(session_id).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|s| {
                let mut v: serde_json::Value = serde_json::from_str(&s)?;
                v.as_object_mut().unwrap().remove("files");
                v.as_object_mut().unwrap().remove("excluded");
                v.as_object_mut().unwrap().remove("changes");
                Ok(v)
            })
            .collect()
    }
    pub async fn context_file(
        &self,
        session_id: &str,
        snapshot_id: &str,
        path: &str,
    ) -> Result<String> {
        let snapshot = self
            .context(session_id, Some(snapshot_id))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Snapshot not found in this session"))?;
        let file = snapshot
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| anyhow::anyhow!("File is not included in this snapshot"))?;
        Ok(
            sqlx::query_scalar("SELECT content FROM context_blobs WHERE hash=?")
                .bind(&file.hash)
                .fetch_one(&self.pool)
                .await?,
        )
    }
    pub async fn save_call(&self, trace: &CallTrace) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO call_traces (id,session_id,started_at,status,data) VALUES (?,?,?,?,?)")
            .bind(&trace.id).bind(&trace.session_id).bind(trace.started_at).bind(&trace.status).bind(serde_json::to_string(trace)?).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn save_run(&self, trace: &RunTrace) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO run_traces (id,session_id,started_at,status,data) VALUES (?,?,?,?,?)")
            .bind(&trace.id).bind(&trace.session_id).bind(trace.started_at).bind(&trace.status).bind(serde_json::to_string(trace)?).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn usage(&self, session_id: &str) -> Result<serde_json::Value> {
        let calls:Vec<String>=sqlx::query_scalar("SELECT data FROM call_traces WHERE session_id=? ORDER BY started_at DESC,rowid DESC LIMIT 200").bind(session_id).fetch_all(&self.pool).await?;
        let runs:Vec<String>=sqlx::query_scalar("SELECT data FROM run_traces WHERE session_id=? ORDER BY started_at DESC,rowid DESC LIMIT 30").bind(session_id).fetch_all(&self.pool).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM call_traces WHERE session_id=?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await?;
        let parse = |rows: Vec<String>| -> Result<Vec<serde_json::Value>> {
            rows.into_iter()
                .map(|s| serde_json::from_str(&s).map_err(Into::into))
                .collect()
        };
        Ok(serde_json::json!({"calls":parse(calls)?,"runs":parse(runs)?,"total_calls":count}))
    }
    pub async fn create(&self, project: String) -> Result<Session> {
        let session = Session {
            id: uuid::Uuid::new_v4().to_string(),
            project,
            title: "New conversation".into(),
            updated_at: now(),
            messages: vec![],
            input: vec![],
        };
        self.save(&session).await?;
        Ok(session)
    }
    pub async fn save(&self, s: &Session) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO sessions (id,project,title,updated_at,messages,input) VALUES (?,?,?,?,?,?)")
            .bind(&s.id).bind(&s.project).bind(&s.title).bind(now()).bind(serde_json::to_string(&s.messages)?).bind(serde_json::to_string(&s.input)?).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn get(&self, id: &str) -> Result<Session> {
        let row = sqlx::query("SELECT * FROM sessions WHERE id=?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Session {
            id: row.get("id"),
            project: row.get("project"),
            title: row.get("title"),
            updated_at: row.get("updated_at"),
            messages: serde_json::from_str(row.get("messages"))?,
            input: serde_json::from_str(row.get("input"))?,
        })
    }
    pub async fn list(&self) -> Result<Vec<Session>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM sessions ORDER BY updated_at DESC, rowid DESC LIMIT 100",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = vec![];
        for id in ids {
            let mut s = self.get(&id).await?;
            s.messages.clear();
            s.input.clear();
            out.push(s);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{Message, ToolActivity};
    use serde_json::json;
    #[tokio::test]
    async fn crash_recovery_closes_orphan_calls_without_replaying_them() {
        let root = std::env::temp_dir().join(format!("substrate-storage-{}", uuid::Uuid::new_v4()));
        let storage = Storage::open(&root.join("data.db")).await.unwrap();
        let mut session = storage.create(root.to_string_lossy().into()).await.unwrap();
        session.input.push(json!({"type":"function_call","call_id":"orphan","name":"execute_command","arguments":"{}"}));
        session.messages.push(Message {
            id: "tool".into(),
            role: "tool".into(),
            content: String::new(),
            tool: Some(ToolActivity {
                id: "tool".into(),
                name: "execute_command".into(),
                arguments: json!({}),
                status: "approval".into(),
                output: None,
                preview: None,
            }),
        });
        storage.save(&session).await.unwrap();
        storage.recover_interrupted().await.unwrap();
        let restored = storage.get(&session.id).await.unwrap();
        assert_eq!(restored.input[1]["type"], "function_call_output");
        assert_eq!(restored.input[1]["call_id"], "orphan");
        assert_eq!(restored.messages[0].tool.as_ref().unwrap().status, "failed");
        storage.recover_interrupted().await.unwrap();
        assert_eq!(storage.get(&session.id).await.unwrap().input.len(), 2);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

#[cfg(test)]
mod context_trace_tests {
    use super::*;
    use agent_protocol::{ContextSnapshot, FileVersion, ModelMetrics};
    fn snapshot(session: &str, revision: u64) -> ContextSnapshot {
        ContextSnapshot {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session.into(),
            revision,
            parent_id: None,
            digest: format!("digest-{revision}"),
            created_at: 0,
            git_head: None,
            budget_bytes: 1024,
            source_bytes: 5,
            prompt_bytes: 100,
            scanned_files: 1,
            truncated: false,
            files: vec![FileVersion {
                path: "file.rs".into(),
                hash: "hash".into(),
                bytes: 5,
            }],
            excluded: vec![],
            changes: vec![],
        }
    }
    #[tokio::test]
    async fn deduplicates_blobs_and_enforces_session_snapshot_ownership() {
        let root = std::env::temp_dir().join(format!("substrate-storage-{}", uuid::Uuid::new_v4()));
        let storage = Storage::open(&root.join("db")).await.unwrap();
        let a = storage.create("/repo".into()).await.unwrap();
        let b = storage.create("/other".into()).await.unwrap();
        let first = snapshot(&a.id, 1);
        let second = snapshot(&a.id, 2);
        let contents = BTreeMap::from([("hash".into(), "hello".into())]);
        storage.save_context(&first, &contents).await.unwrap();
        storage.save_context(&second, &contents).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM context_blobs")
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            storage
                .context_file(&a.id, &first.id, "file.rs")
                .await
                .unwrap(),
            "hello"
        );
        assert!(
            storage
                .context_file(&b.id, &first.id, "file.rs")
                .await
                .is_err()
        );
        assert_eq!(
            storage.context(&a.id, None).await.unwrap().unwrap().id,
            second.id
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn crash_recovery_marks_inflight_calls_without_fabricating_usage() {
        let root = std::env::temp_dir().join(format!("substrate-trace-{}", uuid::Uuid::new_v4()));
        let storage = Storage::open(&root.join("db")).await.unwrap();
        let trace = CallTrace {
            id: "call".into(),
            run_id: "run".into(),
            session_id: "session".into(),
            model: "subq-preview".into(),
            started_at: 0,
            status: "running".into(),
            snapshot_id: "snapshot".into(),
            context_revision: 1,
            context_bytes: 100,
            history_bytes: 10,
            estimated_input_tokens: None,
            tool_calls: 0,
            metrics: ModelMetrics::default(),
        };
        storage.save_call(&trace).await.unwrap();
        storage
            .save_run(&RunTrace {
                id: "run".into(),
                session_id: "session".into(),
                model: "subq-preview".into(),
                started_at: 0,
                ended_at: None,
                status: "running".into(),
                tool_calls: 0,
                tool_ms: 0,
                approval_wait_ms: 0,
            })
            .await
            .unwrap();
        storage.recover_interrupted().await.unwrap();
        let report = storage.usage("session").await.unwrap();
        assert_eq!(report["calls"][0]["status"], "interrupted");
        assert_eq!(report["runs"][0]["status"], "interrupted");
        assert!(report["runs"][0]["ended_at"].is_null());
        assert!(report["calls"][0]["metrics"]["duration_ms"].is_null());
        assert!(report["calls"][0]["metrics"]["usage"].is_null());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
