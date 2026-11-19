use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_FRAME: usize = 4 * 1024 * 1024;
#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}
impl Request {
    pub fn parse(line: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(line.len() <= MAX_FRAME, "Frame exceeds 4 MiB");
        let request: Self = serde_json::from_str(line)?;
        anyhow::ensure!(request.jsonrpc == "2.0", "Expected JSON-RPC 2.0");
        anyhow::ensure!(
            request.id.is_string() || request.id.is_number(),
            "Invalid request id"
        );
        Ok(request)
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Ask,
    ApproveEdits,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    pub path: String,
    pub before: String,
    pub after: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolActivity {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<Preview>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolActivity>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub title: String,
    pub updated_at: i64,
    pub messages: Vec<Message>,
    #[serde(skip)]
    pub input: Vec<Value>,
}
pub fn success(id: Value, result: Value) -> Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn failure(id: Value, code: i32, message: &str) -> Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn notification(method: &str, params: Value) -> Value {
    serde_json::json!({"jsonrpc":"2.0","method":method,"params":params})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_envelope() {
        assert!(Request::parse(r#"{"jsonrpc":"1.0","id":1,"method":"test"}"#).is_err());
        assert!(Request::parse("{").is_err());
        assert!(Request::parse(&"x".repeat(MAX_FRAME + 1)).is_err());
    }
    #[test]
    fn preserves_id() {
        let r = Request::parse(r#"{"jsonrpc":"2.0","id":"a","method":"test"}"#).unwrap();
        assert_eq!(success(r.id, Value::Null)["id"], "a");
    }
}

/// Values are provider-reported. None means unavailable, never zero or an estimate.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelMetrics {
    pub request_bytes: Option<u64>,
    pub response_bytes: Option<u64>,
    pub duration_ms: Option<u64>,
    pub first_output_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallTrace {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub model: String,
    #[serde(default = "generation_kind")]
    pub kind: String,
    pub started_at: i64,
    pub status: String,
    pub snapshot_id: String,
    pub context_revision: u64,
    pub context_bytes: u64,
    pub history_bytes: u64,
    /// UTF-8 bytes / 4 heuristic; not a tokenizer count, capacity limit, or bill.
    pub estimated_input_tokens: Option<u64>,
    pub tool_calls: u64,
    pub metrics: ModelMetrics,
}
fn generation_kind() -> String {
    "generation".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunTrace {
    pub id: String,
    pub session_id: String,
    pub model: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: String,
    pub tool_calls: u64,
    pub tool_ms: u64,
    pub approval_wait_ms: u64,
    #[serde(default)]
    pub context_reads: ContextReads,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContextReads {
    pub successful: u64,
    pub missed: u64,
    pub missed_paths: Vec<MissedFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissedFile {
    pub path: String,
    pub snapshot_id: String,
    pub eligible: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileVersion {
    pub path: String,
    pub hash: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExcludedPath {
    pub path: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextChange {
    pub path: String,
    pub kind: String,
    pub previous_hash: Option<String>,
    pub hash: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub id: String,
    pub session_id: String,
    pub revision: u64,
    pub parent_id: Option<String>,
    pub digest: String,
    pub created_at: i64,
    pub git_head: Option<String>,
    pub budget_bytes: u64,
    pub source_bytes: u64,
    pub prompt_bytes: u64,
    pub scanned_files: u64,
    pub truncated: bool,
    pub files: Vec<FileVersion>,
    pub excluded: Vec<ExcludedPath>,
    pub changes: Vec<ContextChange>,
    #[serde(default)]
    pub selection: Option<ContextSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSelection {
    pub strategy: String,
    pub model: Option<String>,
    pub task_hash: String,
    pub inventory_digest: String,
    pub candidate_files: u64,
    pub snap_files: u64,
    pub reason: Option<String>,
    pub ranks: Vec<FileRank>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted_paths: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRank {
    pub path: String,
    pub hash: String,
    pub score: f64,
    pub confidence: Option<f64>,
    pub source: String,
}
