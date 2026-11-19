use agent_models::{DECISION_MODEL, ModelProvider, Subquadratic};
use agent_protocol::{
    ApprovalMode, CallTrace, ContextSelection, Message, MissedFile, ModelMetrics, RunTrace,
    Session, ToolActivity,
};
use agent_runtime::Runtime;
use agent_storage::Storage;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub struct RunConfig {
    pub key: String,
    pub model: String,
    pub max_steps: usize,
    pub context_budget_bytes: usize,
    pub use_snap: bool,
    pub approval_mode: ApprovalMode,
}
pub async fn capture_context(
    session: &Session,
    storage: &Storage,
    budget: usize,
    cancel: &CancellationToken,
) -> Result<agent_context::ContextPacket> {
    capture_context_inner(session, storage, budget, cancel, None).await
}
struct SelectionRun<'a> {
    provider: &'a dyn ModelProvider,
    runtime: &'a Runtime,
    run_id: &'a str,
    task: &'a str,
    cache: &'a mut Option<ContextSelection>,
    use_snap: bool,
    promoted: &'a [String],
}
async fn capture_context_inner(
    session: &Session,
    storage: &Storage,
    budget: usize,
    cancel: &CancellationToken,
    selector: Option<SelectionRun<'_>>,
) -> Result<agent_context::ContextPacket> {
    let root = agent_tools::workspace(&session.project)?;
    let previous = storage.context(&session.id, None).await?;
    let previous_id = previous.as_ref().map(|m| m.id.clone());
    let head = tokio::select! {
        _=cancel.cancelled()=>anyhow::bail!("Interrupted"),
        result=agent_tools::git(&root,&["rev-parse","HEAD"])=>result.ok().map(|s|s.trim().to_string())
    };
    let token = cancel.clone();
    let inventory =
        tokio::task::spawn_blocking(move || agent_context::inventory(&root, &token)).await??;
    let mut selection_trace = None;
    let selection = if let Some(selector) = selector {
        let task_hash = agent_context::hash(selector.task.as_bytes());
        let reusable = selector
            .cache
            .as_ref()
            .or_else(|| {
                previous
                    .as_ref()
                    .and_then(|s| s.selection.as_ref())
                    .filter(|s| s.strategy == "snap" || s.strategy == "snap_with_fallback")
            })
            .filter(|s| {
                (selector.use_snap || s.strategy == "local")
                    && s.task_hash == task_hash
                    && s.inventory_digest == inventory.digest
            });
        let mut selected = if let Some(selection) = reusable {
            selection.clone()
        } else {
            let local = agent_context::local_selection(&inventory, selector.task);
            if !selector.use_snap || local.ranks.is_empty() {
                local
            } else {
                let (state, questions) =
                    agent_context::snap_request(&inventory, selector.task, &local);
                let mut trace = CallTrace {
                    id: id(),
                    run_id: selector.run_id.into(),
                    session_id: session.id.clone(),
                    model: DECISION_MODEL.into(),
                    kind: "context_selection".into(),
                    started_at: agent_context::now_ms(),
                    status: "running".into(),
                    snapshot_id: String::new(),
                    context_revision: 0,
                    context_bytes: serde_json::to_vec(&state)?.len() as u64,
                    history_bytes: 0,
                    estimated_input_tokens: None,
                    tool_calls: 0,
                    metrics: ModelMetrics::default(),
                };
                storage.save_call(&trace).await?;
                selector
                    .runtime
                    .emit(
                        "usage.updated",
                        json!({"session_id":session.id,"call_id":trace.id}),
                    )
                    .await;
                let response = selector.provider.decide(state, questions, cancel).await;
                let selected = response
                    .output
                    .and_then(|answers| agent_context::snap_selection(&local, &answers));
                trace.metrics = response.metrics;
                trace.estimated_input_tokens = trace.metrics.request_bytes.map(|b| b.div_ceil(4));
                trace.status = if cancel.is_cancelled() {
                    "interrupted"
                } else if selected.is_ok() {
                    "completed"
                } else {
                    "failed"
                }
                .into();
                storage.save_call(&trace).await?;
                selector
                    .runtime
                    .emit(
                        "usage.updated",
                        json!({"session_id":session.id,"call_id":trace.id}),
                    )
                    .await;
                selection_trace = Some(trace);
                ensure!(!cancel.is_cancelled(), "Interrupted");
                selected.unwrap_or_else(|_| {
                    let mut fallback = local;
                    fallback.strategy = "fallback".into();
                    fallback.model = Some(DECISION_MODEL.into());
                    fallback.reason = Some(
                        "Snap unavailable or invalid response; using local task relevance".into(),
                    );
                    fallback
                })
            }
        };
        *selector.cache = Some(selected.clone());
        selected.promoted_paths = selector
            .promoted
            .iter()
            .rev()
            .filter(|path| selected.ranks.iter().any(|rank| &rank.path == *path))
            .take(16)
            .cloned()
            .collect();
        selected
    } else {
        previous
            .as_ref()
            .and_then(|s| s.selection.as_ref())
            .filter(|s| s.inventory_digest == inventory.digest)
            .cloned()
            .unwrap_or_else(|| agent_context::local_selection(&inventory, &selection_task(session)))
    };
    let session_id = session.id.clone();
    let token = cancel.clone();
    let packet = tokio::task::spawn_blocking(move || {
        agent_context::pack(
            inventory,
            &session_id,
            budget,
            previous.as_ref(),
            head,
            selection,
            &token,
        )
    })
    .await??;
    if previous_id.as_deref() != Some(&packet.manifest.id) {
        storage
            .save_context(&packet.manifest, &packet.contents)
            .await?;
    }
    if let Some(mut trace) = selection_trace {
        trace.snapshot_id = packet.manifest.id.clone();
        trace.context_revision = packet.manifest.revision;
        storage.save_call(&trace).await?;
    }
    Ok(packet)
}
fn selection_task(session: &Session) -> String {
    let latest = session.messages.iter().rposition(|m| m.role == "user");
    let mut task = latest
        .map(|i| {
            format!(
                "Current task: {}\n",
                agent_context::bounded(&session.messages[i].content, 8192)
            )
        })
        .unwrap_or_default();
    for message in session.messages[..latest.unwrap_or(0)]
        .iter()
        .rev()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .take(2)
    {
        task.push_str(&format!(
            "Prior {}: {}\n",
            message.role,
            agent_context::bounded(&message.content, 1536)
        ));
    }
    if task.is_empty() {
        task = session
            .input
            .iter()
            .rev()
            .find(|v| v["role"] == "user")
            .and_then(|v| v["content"].as_str())
            .unwrap_or_default()
            .into();
    }
    agent_context::bounded(&task, 12 * 1024).into()
}

fn context_key(path: &str) -> String {
    std::path::Path::new(path)
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(p) = c {
                Some(p.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}
fn read_versions(input: &[Value]) -> HashMap<String, String> {
    let mut calls = HashMap::new();
    let mut versions = HashMap::new();
    for item in input {
        if item["type"] == "function_call" && item["name"] == "read_file" {
            if let (Some(call), Some(arguments)) =
                (item["call_id"].as_str(), item["arguments"].as_str())
                && let Ok(args) = serde_json::from_str::<Value>(arguments)
                && let Some(path) = args["path"].as_str()
            {
                calls.insert(call.to_owned(), context_key(path));
            }
        } else if item["type"] == "function_call_output"
            && let Some(path) = item["call_id"].as_str().and_then(|id| calls.get(id))
            && let Some(output) = item["output"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
            && let Some(content) = output["content"].as_str()
        {
            versions.insert(path.clone(), agent_context::hash(content.as_bytes()));
        }
    }
    versions
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
    let mut run_trace = RunTrace {
        id: id(),
        session_id: session.id.clone(),
        model: config.model.clone(),
        started_at: agent_context::now_ms(),
        ended_at: None,
        status: "running".into(),
        tool_calls: 0,
        tool_ms: 0,
        approval_wait_ms: 0,
        context_reads: Default::default(),
    };
    storage.save_run(&run_trace).await?;
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
        &mut run_trace,
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
    run_trace.status = if cancel.is_cancelled() {
        "interrupted"
    } else if result.is_ok() {
        "completed"
    } else {
        "failed"
    }
    .into();
    run_trace.ended_at = Some(agent_context::now_ms());
    storage.save_run(&run_trace).await?;
    if !cancel.is_cancelled()
        && let Ok(packet) =
            capture_context(&session, &storage, config.context_budget_bytes, &cancel).await
    {
        runtime.emit("context.updated",json!({"session_id":session.id,"snapshot_id":packet.manifest.id,"revision":packet.manifest.revision})).await;
    }
    result
}
async fn inventory_digest(root: &std::path::Path, cancel: &CancellationToken) -> Result<String> {
    let root = root.to_path_buf();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        agent_context::inventory(&root, &cancel).map(|inventory| inventory.digest)
    })
    .await?
}

#[derive(Default)]
struct Verification {
    required: bool,
    passed_at: Option<String>,
    failure: Option<String>,
}
impl Verification {
    fn invalidate(&mut self) {
        self.required = true;
        self.passed_at = None;
    }

    fn pending(&self, digest: &str) -> bool {
        self.required && self.passed_at.as_deref() != Some(digest)
    }

    fn failed(&mut self, message: String) {
        self.passed_at = None;
        self.failure = Some(message);
    }
}

async fn loop_run(
    session: &mut Session,
    config: &RunConfig,
    storage: &Storage,
    runtime: &Runtime,
    cancel: &CancellationToken,
    provider: &dyn ModelProvider,
    run_trace: &mut RunTrace,
) -> Result<()> {
    let root = agent_tools::workspace(&session.project)?;
    let instructions = agent_context::instructions(&session.project, config.approval_mode);
    let check_new_npm_app = cfg!(unix) && agent_tools::npm_start_script(&root).await.is_none();
    let mut verification = Verification::default();
    let mut npm_verification = Verification::default();
    let mut initial_digest = None;
    let mut shell_ran = false;
    let mut premature_finishes = 0;
    let task = selection_task(session);
    let mut selection_cache = None;
    let mut promoted = Vec::new();
    for _ in 0..config.max_steps {
        ensure!(!cancel.is_cancelled(), "Interrupted");
        ensure!(
            agent_context::fits(&session.input),
            "Conversation reached its context budget. Start a new conversation."
        );
        let packet = capture_context_inner(
            session,
            storage,
            config.context_budget_bytes,
            cancel,
            Some(SelectionRun {
                provider,
                runtime,
                run_id: &run_trace.id,
                task: &task,
                cache: &mut selection_cache,
                use_snap: config.use_snap,
                promoted: &promoted,
            }),
        )
        .await?;
        let digest = &packet
            .manifest
            .selection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing workspace inventory for verification"))?
            .inventory_digest;
        let baseline = initial_digest.get_or_insert_with(|| digest.clone());
        verification.required |= shell_ran && digest != baseline;
        npm_verification.required =
            check_new_npm_app && agent_tools::npm_start_script(&root).await.is_some();
        let needs_npm = npm_verification.pending(digest);
        let needs_verification = needs_npm || verification.pending(digest);
        let buffer_text = needs_verification || verification.required || npm_verification.required;
        let mut instructions = instructions.clone();
        if needs_npm {
            instructions.push_str(
                "\nThe new npm app has no successful dependency and startup verification for its current workspace. Use verify_npm_app after installing dependencies.",
            );
        }
        if needs_verification {
            instructions.push_str(
                "\nVerification is incomplete. Inspect the changes and use run_check to exercise the fix and existing behavior the edit could break. Fix any failure and rerun the relevant checks against the latest code. If verification is blocked, use any permitted alternative or state the blocker. Do not claim completion.",
            );
            if premature_finishes > 0 {
                instructions.push_str("\nThe previous answer was withheld because verification is incomplete. Continue with a tool call if you can verify the changes.");
            }
        } else if buffer_text {
            instructions.push_str("\nA check passed on the current workspace. Review its output and the edit. Check that earlier failures are resolved and that the patch preserves behavior that worked before the fix. Look at the old and new code to enumerate supported variants of the affected input or behavior. When there are only a few materially different variants, run a focused check for each, including less common valid forms the old code handled. If the set is large, choose representative variants based on what the edit could break. Report which checks passed and any relevant coverage gaps.");
        }
        runtime.emit("context.updated",json!({"session_id":session.id,"snapshot_id":packet.manifest.id,"revision":packet.manifest.revision})).await;
        let mut versions = read_versions(&session.input);
        for file in &packet.manifest.files {
            versions.insert(file.path.clone(), file.hash.clone());
        }
        let mut model_input = vec![json!({"role":"user","content":packet.prompt()})];
        model_input.extend(session.input.iter().cloned());
        let mut trace = CallTrace {
            id: id(),
            run_id: run_trace.id.clone(),
            session_id: session.id.clone(),
            model: config.model.clone(),
            kind: "generation".into(),
            started_at: agent_context::now_ms(),
            status: "running".into(),
            snapshot_id: packet.manifest.id.clone(),
            context_revision: packet.manifest.revision,
            context_bytes: packet.manifest.prompt_bytes,
            history_bytes: serde_json::to_vec(&session.input)?.len() as u64,
            estimated_input_tokens: None,
            tool_calls: 0,
            metrics: ModelMetrics::default(),
        };
        storage.save_call(&trace).await?;
        runtime
            .emit(
                "usage.updated",
                json!({"session_id":session.id,"call_id":trace.id}),
            )
            .await;
        let message_id = id();
        let (tx, mut rx) = mpsc::channel::<String>(128);
        let sid = session.id.clone();
        let mid = message_id.clone();
        let rt = runtime.clone();
        let forward = tokio::spawn(async move {
            let mut text = String::new();
            while let Some(delta) = rx.recv().await {
                text.push_str(&delta);
                if !buffer_text {
                    rt.emit(
                        "agent.message.delta",
                        json!({"session_id":sid,"message_id":mid,"delta":delta}),
                    )
                    .await;
                }
            }
            text
        });
        let output = provider
            .respond(
                &config.model,
                &instructions,
                &model_input,
                agent_tools::definitions(),
                cancel,
                tx,
            )
            .await;
        let text = forward.await?;
        trace.metrics = output.metrics;
        trace.estimated_input_tokens = trace.metrics.request_bytes.map(|bytes| bytes.div_ceil(4));
        trace.status = if cancel.is_cancelled() {
            "interrupted"
        } else if output.output.is_ok() {
            "completed"
        } else {
            "failed"
        }
        .into();
        trace.tool_calls = output.output.as_ref().map_or(0, |items| {
            items
                .iter()
                .filter(|v| v["type"] == "function_call")
                .count() as u64
        });
        storage.save_call(&trace).await?;
        runtime
            .emit(
                "usage.updated",
                json!({"session_id":session.id,"call_id":trace.id}),
            )
            .await;
        if trace.tool_calls == 0 && output.output.is_ok() {
            ensure!(!cancel.is_cancelled(), "Interrupted");
            if needs_verification {
                premature_finishes += 1;
                let failure = if needs_npm {
                    &npm_verification.failure
                } else {
                    &verification.failure
                };
                if premature_finishes >= 2
                    || failure.as_deref() == Some("Check approval was declined.")
                {
                    anyhow::bail!(
                        "Verification incomplete. {}",
                        failure.as_deref().unwrap_or(if needs_npm {
                            "The new npm app still needs dependency and startup verification."
                        } else {
                            "The model stopped before a check passed on the current changes."
                        })
                    );
                }
                continue;
            }
            if buffer_text && inventory_digest(&root, cancel).await? != *digest {
                continue;
            }
        }
        premature_finishes = 0;
        if !text.is_empty() && !needs_verification {
            if buffer_text {
                runtime
                    .emit(
                        "agent.message.delta",
                        json!({"session_id":session.id,"message_id":message_id,"delta":text}),
                    )
                    .await;
            }
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
        let mut output = match output.output {
            Ok(o) => o,
            Err(e) => {
                storage.save(session).await?;
                return Err(e);
            }
        };
        if needs_verification {
            output.retain(|item| item["type"] != "message");
        }
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
            let is_check = matches!(name, "run_check" | "verify_npm_app");
            let before_check = if is_check {
                Some(inventory_digest(&root, cancel).await?)
            } else {
                None
            };
            run_trace.tool_calls += 1;
            let mut result = match args {
                Ok(args) => run_tool(
                    session,
                    name,
                    &args,
                    storage,
                    runtime,
                    cancel,
                    &mut ToolContext {
                        root: &root,
                        versions: &versions,
                        trace: run_trace,
                        approval_mode: config.approval_mode,
                    },
                )
                .await
                .inspect(|_| {
                    if name == "read_file"
                        && let Some(path) = args["path"].as_str()
                    {
                        let path = context_key(path);
                        let reads = &mut run_trace.context_reads;
                        reads.successful += 1;
                        if !packet.manifest.files.iter().any(|file| file.path == path) {
                            reads.missed += 1;
                            let eligible =
                                packet.manifest.selection.as_ref().is_some_and(|selection| {
                                    selection.ranks.iter().any(|rank| rank.path == path)
                                });
                            if reads.missed_paths.len() < 64
                                && !reads.missed_paths.iter().any(|miss| miss.path == path)
                            {
                                reads.missed_paths.push(MissedFile {
                                    path: path.clone(),
                                    snapshot_id: packet.manifest.id.clone(),
                                    eligible,
                                });
                            }
                            if eligible {
                                promoted.retain(|prior| prior != &path);
                                promoted.push(path);
                                if promoted.len() > 16 {
                                    promoted.remove(0);
                                }
                            }
                        }
                    }
                }),
                Err(e) => Err(e.into()),
            };
            let rejected = result.as_ref().is_ok_and(|value| value["rejected"] == true);
            shell_ran |= (agent_tools::is_shell_tool(name) || is_check) && !rejected;
            if name == "apply_patch" && result.as_ref().is_ok_and(|value| value["applied"] == true)
            {
                verification.invalidate();
                npm_verification.passed_at = None;
            }
            if let Some(before) = before_check {
                let passed = result.as_ref().is_ok_and(|value| {
                    if name == "verify_npm_app" {
                        value["verified"] == true
                    } else {
                        value["exit_code"] == 0
                    }
                });
                let state = if name == "verify_npm_app" {
                    &mut npm_verification
                } else {
                    &mut verification
                };
                if passed && inventory_digest(&root, cancel).await? == before {
                    state.passed_at = Some(before.clone());
                    state.failure = None;
                    if name == "verify_npm_app" && verification.failure.is_none() {
                        verification.passed_at = Some(before);
                    }
                } else {
                    let message = match &result {
                        _ if rejected => "Check approval was declined.".into(),
                        Ok(value) if passed => {
                            let message = "Workspace files changed during the check. Rerun it against the resulting code.";
                            result = Ok(json!({"result": value, "verification_error": message}));
                            message.into()
                        }
                        Err(error) => agent_context::bounded(&error.to_string(), 512).to_owned(),
                        Ok(value) if name == "run_check" => {
                            format!("The check failed (exit code {}).", value["exit_code"])
                        }
                        Ok(_) => "App launch verification failed.".into(),
                    };
                    state.failed(message);
                }
            }
            let output = match result {
                Ok(v) => v.to_string(),
                Err(e) => json!({"error":e.to_string()}).to_string(),
            };
            session
                .input
                .push(json!({"type":"function_call_output","call_id":call_id,"output":output}));
            storage.save(session).await?;
            storage.save_run(run_trace).await?;
        }
    }
    let verification_status = if verification.required || npm_verification.required {
        " The run ended before the model could confirm verification and report the result."
    } else {
        ""
    };
    anyhow::bail!(
        "Stopped after {} model turns.{verification_status}",
        config.max_steps,
    )
}
struct ToolContext<'a> {
    root: &'a std::path::Path,
    versions: &'a HashMap<String, String>,
    trace: &'a mut RunTrace,
    approval_mode: ApprovalMode,
}
async fn run_tool(
    session: &mut Session,
    name: &str,
    args: &Value,
    storage: &Storage,
    runtime: &Runtime,
    cancel: &CancellationToken,
    context: &mut ToolContext<'_>,
) -> Result<Value> {
    let root = context.root;
    agent_tools::validate(name, args)?;
    let preview = if name == "apply_patch" {
        let path = agent_tools::string(args, "path")?;
        let file = agent_tools::resolve(root, path)?;
        let expected = context.versions.get(&context_key(path));
        if file.exists() || expected.is_some() {
            let expected = expected.ok_or_else(|| {
                anyhow::anyhow!("Read this file before editing; it is not in the model's snapshot")
            })?;
            let current = agent_tools::read(root, path).await?;
            ensure!(
                agent_context::hash(current.as_bytes()) == *expected,
                "File changed since the model read it; refresh context or read it again before editing"
            );
        }
        let preview = agent_tools::preview(root, args).await?;
        if let Some(expected) = expected {
            ensure!(
                agent_context::hash(preview.before.as_bytes()) == *expected,
                "File changed while preparing the edit; retry after reading it again"
            );
        }
        Some(preview)
    } else {
        None
    };
    let launch_manifest = if name == "verify_npm_app" {
        Some(agent_tools::read(root, "package.json").await?)
    } else {
        None
    };
    let requires_approval = agent_tools::is_shell_tool(name)
        || name == "verify_npm_app"
        || (name == "apply_patch" && context.approval_mode == ApprovalMode::Ask);
    let mut tool = ToolActivity {
        id: id(),
        name: name.into(),
        arguments: if let Some(manifest) = &launch_manifest {
            let manifest: Value = serde_json::from_str(manifest)?;
            let start = agent_tools::string(&manifest["scripts"], "start")?;
            json!({"command":format!("npm ls --depth=0\nnpm outdated --depth=0\nnpm start\n  {start}")})
        } else {
            args.clone()
        },
        status: if requires_approval {
            "approval"
        } else {
            "running"
        }
        .into(),
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
    let approval = if requires_approval {
        Some(runtime.register_approval(&session.id, &tool.id).await)
    } else {
        None
    };
    storage.save(session).await?;
    runtime
        .emit(
            if requires_approval {
                "tool.approval.required"
            } else {
                "tool.started"
            },
            json!({"session_id":session.id,"tool":tool}),
        )
        .await;
    if let Some(rx) = approval {
        let waiting = Instant::now();
        let approved = tokio::select! {_=cancel.cancelled()=>false,r=rx=>r.unwrap_or(false)};
        context.trace.approval_wait_ms += waiting.elapsed().as_millis() as u64;
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
    let executing = Instant::now();
    let result = if let Some(p) = &tool.preview {
        drop(tx);
        agent_tools::apply(root, p).await
    } else if let Some(manifest) = launch_manifest {
        match agent_tools::read(root, "package.json").await {
            Ok(current) if current == manifest => {
                agent_tools::verify_npm_app(root, cancel, tx).await
            }
            _ => Err(anyhow::anyhow!(
                "package.json changed during approval. Review the new start script before launching."
            )),
        }
    } else {
        agent_tools::execute(root, name, args, cancel, tx).await
    };
    forward.await?;
    context.trace.tool_ms += executing.elapsed().as_millis() as u64;
    tool.status = if result
        .as_ref()
        .is_ok_and(|value| !agent_tools::is_shell_tool(name) || value["exit_code"] == 0)
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
    fn test_trace(session: &str) -> RunTrace {
        RunTrace {
            id: id(),
            session_id: session.into(),
            model: "test".into(),
            started_at: 0,
            ended_at: None,
            status: "running".into(),
            tool_calls: 0,
            tool_ms: 0,
            approval_wait_ms: 0,
            context_reads: Default::default(),
        }
    }
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
        ) -> agent_models::ModelResponse {
            let output=async {
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
            Ok(vec![json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Recovered"}]})])
            }.await;
            agent_models::ModelResponse {
                output,
                metrics: ModelMetrics::default(),
            }
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
            use_snap: true,
            approval_mode: ApprovalMode::Ask,
            context_budget_bytes: agent_context::DEFAULT_REPO_BUDGET,
        };
        let model = RecoveringModel(AtomicUsize::new(0));
        loop_run(
            &mut session,
            &config,
            &storage,
            &runtime,
            &CancellationToken::new(),
            &model,
            &mut test_trace("test"),
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
            &json!({"path":"new.txt","old_text":"","new_text":"text"}),
            &storage,
            &runtime,
            &CancellationToken::new(),
            &mut ToolContext {
                root: &root,
                versions: &HashMap::new(),
                trace: &mut test_trace("test"),
                approval_mode: ApprovalMode::Ask,
            },
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
            use_snap: true,
            approval_mode: ApprovalMode::Ask,
            context_budget_bytes: agent_context::DEFAULT_REPO_BUDGET,
        };
        assert!(
            loop_run(
                &mut session,
                &config,
                &storage,
                &runtime,
                &token,
                &model,
                &mut test_trace("test")
            )
            .await
            .is_err()
        );
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
