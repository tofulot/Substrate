use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
type PendingApprovals = HashMap<String, (String, oneshot::Sender<bool>)>;
pub struct Job {
    pub cancel: CancellationToken,
    pub approval_mode: watch::Sender<agent_protocol::ApprovalMode>,
}
#[derive(Clone)]
pub struct Runtime {
    pub events: mpsc::Sender<Value>,
    pub jobs: Arc<Mutex<HashMap<String, Job>>>,
    approvals: Arc<Mutex<PendingApprovals>>,
}
impl Runtime {
    pub fn new(events: mpsc::Sender<Value>) -> Self {
        Self {
            events,
            jobs: Default::default(),
            approvals: Default::default(),
        }
    }
    pub async fn emit(&self, method: &str, params: Value) {
        let _ = self
            .events
            .send(agent_protocol::notification(method, params))
            .await;
    }
    pub async fn register_approval(&self, session: &str, id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.approvals
            .lock()
            .await
            .insert(id.to_owned(), (session.to_owned(), tx));
        rx
    }
    pub async fn resolve(&self, session: &str, id: &str, approve: bool) -> anyhow::Result<()> {
        let mut pending = self.approvals.lock().await;
        anyhow::ensure!(
            pending.get(id).is_some_and(|(s, _)| s == session),
            "Approval not found in this session"
        );
        let (_, tx) = pending.remove(id).unwrap();
        let _ = tx.send(approve);
        Ok(())
    }
    pub async fn clear_approval(&self, id: &str) {
        self.approvals.lock().await.remove(id);
    }
}
