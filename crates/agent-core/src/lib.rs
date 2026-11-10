use agent_models::{ModelProvider, Subquadratic};
use agent_protocol::{Message, Session, ToolActivity};
use agent_runtime::Runtime;
use agent_storage::Storage;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub struct RunConfig {
    pub key: String,
    pub model: String,
    pub max_steps: usize,
}
pub async fn run(
    mut session: Session,
    task: String,
    config: RunConfig,
    storage: Storage,
    runtime: Runtime,
    cancel: CancellationToken,
) -> Result<()> {
    let provider = Subquadratic::new(config.key.clone())?;
    if session.messages.is_empty() {
        session.title = task.chars().take(60).collect();
    }
    session.messages.push(Message {
        id: id(),
        role: "user".into(),
        content: task.clone(),
        tool: None,
    });
    session.input.push(json!({"role":"user","content":task}));
    storage.save(&session).await?;
    runtime
        .emit("agent.started", json!({"session_id":session.id}))
        .await;
    let result = loop_run(
        &mut session,
        &config,
        &storage,
        &runtime,
        &cancel,
        &provider,
    )
    .await;
    // Interrupted calls must not leave unresolved tool-call items in the next model request.
    let unresolved: Vec<String> = session
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
    for call in unresolved {
        session.input.push(json!({"type":"function_call_output","call_id":call,"output":"Interrupted before execution completed. Inspect workspace state before retrying."}));
    }
    for m in &mut session.messages {
        if let Some(t) = &mut m.tool
            && ["running", "approval"].contains(&t.status.as_str())
        {
            t.status = "failed".into();
            t.output = Some("Interrupted".into());
        }
    }
    storage.save(&session).await?;
    result
}
async fn loop_run(
    session: &mut Session,
    config: &RunConfig,
    storage: &Storage,
    runtime: &Runtime,
    cancel: &CancellationToken,
    provider: &dyn ModelProvider,
) -> Result<()> {
    let root = agent_tools::workspace(&session.project)?;
    let entries = agent_tools::entries(&root, "").await?;
    let instructions = agent_context::instructions(&session.project, &entries.to_string());
    for _ in 0..config.max_steps {
        ensure!(!cancel.is_cancelled(), "Interrupted");
        ensure!(
            agent_context::fits(&session.input),
            "Conversation reached its context budget. Start a new conversation."
        );
        let message_id = id();
        let (tx, mut rx) = mpsc::channel::<String>(128);
        let sid = session.id.clone();
        let mid = message_id.clone();
        let rt = runtime.clone();
        let forward = tokio::spawn(async move {
            let mut text = String::new();
            while let Some(delta) = rx.recv().await {
                text.push_str(&delta);
                rt.emit(
                    "agent.message.delta",
                    json!({"session_id":sid,"message_id":mid,"delta":delta}),
                )
                .await;
            }
            text
        });
        let output = provider
            .respond(
                &config.model,
                &instructions,
                &session.input,
                agent_tools::definitions(),
                cancel,
                tx,
            )
            .await;
        let text = forward.await?;
        if !text.is_empty() {
            session.messages.push(Message {
                id: message_id.clone(),
                role: "assistant".into(),
                content: text,
                tool: None,
            });
        }
        runtime
            .emit(
                "agent.message.completed",
                json!({"session_id":session.id,"message_id":message_id}),
            )
            .await;
        let output = match output {
            Ok(o) => o,
            Err(e) => {
                storage.save(session).await?;
                return Err(e);
            }
        };
        let calls: Vec<_> = output
            .iter()
            .filter(|v| v["type"] == "function_call")
            .cloned()
            .collect();
        session.input.extend(output);
        storage.save(session).await?;
        if calls.is_empty() {
            return Ok(());
        }
        for call in calls {
            ensure!(!cancel.is_cancelled(), "Interrupted");
            let name = call["name"].as_str().unwrap_or("");
            let call_id = call["call_id"].as_str().unwrap_or("");
            let args: Result<Value, _> =
                serde_json::from_str(call["arguments"].as_str().unwrap_or("{}"));
            let result = match args {
                Ok(args) => run_tool(session, name, args, storage, runtime, cancel, &root).await,
                Err(e) => Err(e.into()),
            };
            let output = match result {
                Ok(v) => v.to_string(),
                Err(e) => json!({"error":e.to_string()}).to_string(),
            };
            session
                .input
                .push(json!({"type":"function_call_output","call_id":call_id,"output":output}));
            storage.save(session).await?;
        }
    }
    anyhow::bail!(
        "Stopped after {} model turns. Send a follow-up to continue.",
        config.max_steps
    )
}
async fn run_tool(
    session: &mut Session,
    name: &str,
    args: Value,
    storage: &Storage,
    runtime: &Runtime,
    cancel: &CancellationToken,
    root: &std::path::Path,
) -> Result<Value> {
    agent_tools::validate(name, &args)?;
    let preview = if name == "apply_patch" {
        Some(agent_tools::preview(root, &args).await?)
    } else {
        None
    };
    let sensitive = matches!(name, "apply_patch" | "execute_command");
    let mut tool = ToolActivity {
        id: id(),
        name: name.into(),
        arguments: args.clone(),
        status: if sensitive { "approval" } else { "running" }.into(),
        output: None,
        preview,
    };
    let index = session.messages.len();
    session.messages.push(Message {
        id: tool.id.clone(),
        role: "tool".into(),
        content: String::new(),
        tool: Some(tool.clone()),
    });
    // Register before publishing the request so a fast UI response cannot race the waiter.
    let approval = if sensitive {
        Some(runtime.register_approval(&session.id, &tool.id).await)
    } else {
        None
    };
    storage.save(session).await?;
    runtime
        .emit(
            if sensitive {
                "tool.approval.required"
            } else {
                "tool.started"
            },
            json!({"session_id":session.id,"tool":tool}),
        )
        .await;
    if let Some(rx) = approval {
        let approved = tokio::select! {_=cancel.cancelled()=>false,r=rx=>r.unwrap_or(false)};
        runtime.clear_approval(&tool.id).await;
        if !approved {
            tool.status = "rejected".into();
            tool.output = Some("User declined or interrupted this action".into());
            session.messages[index].tool = Some(tool.clone());
            runtime
                .emit(
                    "tool.completed",
                    json!({"session_id":session.id,"tool":tool}),
                )
                .await;
            return Ok(
                json!({"rejected":true,"message":"User declined this action. Do not retry it via another tool."}),
            );
        }
        tool.status = "running".into();
        runtime
            .emit("tool.started", json!({"session_id":session.id,"tool":tool}))
            .await;
    }
    let (tx, mut rx) = mpsc::channel::<String>(128);
    let rt = runtime.clone();
    let sid = session.id.clone();
    let tid = tool.id.clone();
    let forward = tokio::spawn(async move {
        while let Some(delta) = rx.recv().await {
            rt.emit(
                "tool.output.delta",
                json!({"session_id":sid,"tool_id":tid,"delta":delta}),
            )
            .await;
        }
    });
    ensure!(!cancel.is_cancelled(), "Interrupted");
    let result = if let Some(p) = &tool.preview {
        drop(tx);
        agent_tools::apply(root, p).await
    } else {
        agent_tools::execute(root, name, &args, cancel, tx).await
    };
    forward.await?;
    tool.status = if result
        .as_ref()
        .is_ok_and(|value| name != "execute_command" || value["exit_code"] == 0)
    {
        "completed"
    } else {
        "failed"
    }
    .into();
    tool.output = Some(match &result {
        Ok(v) => v.to_string(),
        Err(e) => e.to_string(),
    });
    session.messages[index].tool = Some(tool.clone());
    storage.save(session).await?;
    runtime
        .emit(
            "tool.completed",
            json!({"session_id":session.id,"tool":tool}),
        )
        .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RecoveringModel(AtomicUsize);
    #[async_trait::async_trait]
    impl ModelProvider for RecoveringModel {
        async fn respond(
            &self,
            _model: &str,
            _instructions: &str,
            input: &[Value],
            _tools: Vec<Value>,
            _cancel: &CancellationToken,
            deltas: mpsc::Sender<String>,
        ) -> Result<Vec<Value>> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(vec![
                    json!({"type":"function_call","call_id":"bad","name":"read_file","arguments":"{\"path\":42}"}),
                ]);
            }
            assert!(
                input.last().unwrap()["output"]
                    .as_str()
                    .unwrap()
                    .contains("error")
            );
            deltas.send("Recovered".into()).await?;
            Ok(vec![
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Recovered"}]}),
            ])
        }
    }
    async fn fixture() -> (
        std::path::PathBuf,
        Storage,
        Session,
        Runtime,
        mpsc::Receiver<Value>,
    ) {
        let root = std::env::temp_dir().join(format!("substrate-core-{}", id()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let storage = Storage::open(&root.join("data.db")).await.unwrap();
        let session = storage.create(root.to_string_lossy().into()).await.unwrap();
        let (tx, rx) = mpsc::channel(128);
        (root, storage, session, Runtime::new(tx), rx)
    }
    #[tokio::test]
    async fn recovers_from_invalid_tool_arguments() {
        let (root, storage, mut session, runtime, _rx) = fixture().await;
        let config = RunConfig {
            key: String::new(),
            model: "subq-preview".into(),
            max_steps: 3,
        };
        let model = RecoveringModel(AtomicUsize::new(0));
        loop_run(
            &mut session,
            &config,
            &storage,
            &runtime,
            &CancellationToken::new(),
            &model,
        )
        .await
        .unwrap();
        assert_eq!(model.0.load(Ordering::SeqCst), 2);
        assert_eq!(session.messages.last().unwrap().content, "Recovered");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn no_write_until_approval_and_decline_leaves_file_absent() {
        let (root, storage, mut session, runtime, mut rx) = fixture().await;
        let rt = runtime.clone();
        let path = root.join("new.txt");
        let reviewer = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if event["method"] == "tool.approval.required" {
                    assert!(!path.exists());
                    let p = &event["params"];
                    rt.resolve(
                        p["session_id"].as_str().unwrap(),
                        p["tool"]["id"].as_str().unwrap(),
                        false,
                    )
                    .await
                    .unwrap();
                    break;
                }
            }
        });
        let result = run_tool(
            &mut session,
            "apply_patch",
            json!({"path":"new.txt","old_text":"","new_text":"text"}),
            &storage,
            &runtime,
            &CancellationToken::new(),
            &root,
        )
        .await
        .unwrap();
        reviewer.await.unwrap();
        assert_eq!(result["rejected"], true);
        assert!(!root.join("new.txt").exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_prevents_model_invocation() {
        let (root, storage, mut session, runtime, _rx) = fixture().await;
        let token = CancellationToken::new();
        token.cancel();
        let model = RecoveringModel(AtomicUsize::new(0));
        let config = RunConfig {
            key: String::new(),
            model: "subq-preview".into(),
            max_steps: 3,
        };
        assert!(
            loop_run(&mut session, &config, &storage, &runtime, &token, &model)
                .await
                .is_err()
        );
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
