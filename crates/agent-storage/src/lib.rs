use agent_protocol::Session;
use anyhow::Result;
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
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
        Ok(())
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
