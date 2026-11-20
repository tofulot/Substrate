use agent_protocol::{Request, failure, success};
use agent_runtime::{Job, Runtime};
use agent_storage::Storage;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc, watch},
};
use tokio_util::sync::CancellationToken;

const DEFAULT_MAX_STEPS: u64 = 64;
const MAX_MAX_STEPS: u64 = 128;

struct Config {
    key: String,
    model: String,
    max_steps: usize,
    context_budget_bytes: usize,
}
#[derive(Clone)]
struct Server {
    storage: Storage,
    runtime: Runtime,
    config: Arc<Mutex<Config>>,
    config_path: PathBuf,
}
fn str_arg<'a>(p: &'a Value, k: &str) -> Result<&'a str> {
    agent_tools::string(p, k)
}
impl Server {
    async fn root(&self, p: &Value) -> Result<PathBuf> {
        let s = self.storage.get(str_arg(p, "session_id")?).await?;
        agent_tools::workspace(&s.project)
    }
    async fn dispatch(&self, r: &Request) -> Result<Value> {
        let p = &r.params;
        match r.method.as_str() {
            "ping" => Ok(json!({"version":env!("CARGO_PKG_VERSION"),"provider":"subquadratic"})),
            "config.get" => {
                let c = self.config.lock().await;
                Ok(
                    json!({"model":c.model,"has_key":!c.key.is_empty(),"max_steps":c.max_steps,"context_budget_bytes":c.context_budget_bytes}),
                )
            }
            "config.credentials" => {
                let key = str_arg(p, "key")?;
                ensure!(key.len() < 8192, "Invalid API key");
                self.config.lock().await.key = key.into();
                Ok(json!(true))
            }
            "config.update" => {
                let model = agent_models::current_model(str_arg(p, "model")?);
                ensure!(
                    model.starts_with("subq-") && !model.contains("snap") && model.len() < 120,
                    "Select a SubQ generative model; Snap uses a separate Decisions API"
                );
                let mut c = self.config.lock().await;
                let max_steps = p["max_steps"]
                    .as_u64()
                    .unwrap_or(c.max_steps as u64)
                    .clamp(1, MAX_MAX_STEPS) as usize;
                let budget = p["context_budget_bytes"]
                    .as_u64()
                    .unwrap_or(c.context_budget_bytes as u64) as usize;
                ensure!(
                    (32 * 1024..=4 * 1024 * 1024).contains(&budget),
                    "Repository context budget must be 32 KiB–4 MiB"
                );
                c.context_budget_bytes = budget;
                c.model = model.into();
                c.max_steps = max_steps;
                tokio::fs::write(
                    &self.config_path,
                    serde_json::to_vec(
                        &json!({"model":model,"max_steps":max_steps,"context_budget_bytes":budget}),
                    )?,
                )
                .await?;
                Ok(json!(true))
            }
            "models.list" => {
                let key = self.config.lock().await.key.clone();
                agent_models_list(key).await
            }
            "project.open" => {
                let root = agent_tools::workspace(str_arg(p, "path")?)?;
                let branch = agent_tools::git(&root, &["branch", "--show-current"])
                    .await
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                Ok(
                    json!({"path":root.to_string_lossy(),"name":root.file_name().unwrap_or_default().to_string_lossy(),"branch":branch}),
                )
            }
            "session.create" => {
                let root = agent_tools::workspace(str_arg(p, "project")?)?;
                let session = self.storage.create(root.to_string_lossy().into()).await?;
                let budget = self.config.lock().await.context_budget_bytes;
                agent_core::capture_context(
                    &session,
                    &self.storage,
                    budget,
                    &CancellationToken::new(),
                )
                .await?;
                Ok(serde_json::to_value(session)?)
            }
            "context.refresh" => {
                let jobs = self.runtime.jobs.lock().await;
                ensure!(
                    jobs.is_empty(),
                    "Context refreshes automatically between model turns while an agent runs"
                );
                let session = self.storage.get(str_arg(p, "session_id")?).await?;
                let budget = self.config.lock().await.context_budget_bytes;
                let packet = agent_core::capture_context(
                    &session,
                    &self.storage,
                    budget,
                    &CancellationToken::new(),
                )
                .await?;
                drop(jobs);
                self.runtime.emit("context.updated",json!({"session_id":session.id,"snapshot_id":packet.manifest.id,"revision":packet.manifest.revision})).await;
                Ok(serde_json::to_value(packet.manifest)?)
            }
            "context.get" => {
                let session = self.storage.get(str_arg(p, "session_id")?).await?;
                let snapshot = self
                    .storage
                    .context(&session.id, p["snapshot_id"].as_str())
                    .await?;
                let Some(snapshot) = snapshot else {
                    return Ok(
                        json!({"snapshot":null,"stale":false,"changed_paths":[],"checked_at":agent_context::now_ms()}),
                    );
                };
                let root = agent_tools::workspace(&session.project)?;
                let head = agent_tools::git(&root, &["rev-parse", "HEAD"])
                    .await
                    .ok()
                    .map(|s| s.trim().to_string());
                let prior = snapshot.clone();
                let budget = self.config.lock().await.context_budget_bytes;
                let session_id = session.id.clone();
                let task = agent_core::selection_task(&session);
                let current = tokio::task::spawn_blocking(move || {
                    agent_context::scan(
                        &root,
                        &session_id,
                        budget,
                        Some(&prior),
                        head,
                        &CancellationToken::new(),
                        &task,
                    )
                })
                .await??;
                let stale = current.manifest.digest != snapshot.digest;
                Ok(
                    json!({"snapshot":snapshot,"stale":stale,"changed_paths":if stale{current.manifest.changes.iter().map(|c|c.path.clone()).collect::<Vec<_>>()}else{vec![]},"checked_at":agent_context::now_ms()}),
                )
            }
            "context.history" => Ok(json!(
                self.storage
                    .context_history(str_arg(p, "session_id")?)
                    .await?
            )),
            "context.file" => Ok(
                json!({"content":self.storage.context_file(str_arg(p,"session_id")?,str_arg(p,"snapshot_id")?,str_arg(p,"path")?).await?}),
            ),
            "usage.get" => {
                let session = self.storage.get(str_arg(p, "session_id")?).await?;
                self.storage.usage(&session.id).await
            }
            "session.list" => Ok(serde_json::to_value(self.storage.list().await?)?),
            "session.resume" => {
                let session = self.storage.get(str_arg(p, "session_id")?).await?;
                let jobs = self.runtime.jobs.lock().await;
                let job = jobs.get(&session.id);
                let mut result = serde_json::to_value(session)?;
                result["running"] = json!(job.is_some());
                if let Some(job) = job {
                    result["approval_mode"] = json!(*job.approval_mode.borrow());
                }
                Ok(result)
            }
            "agent.start" => {
                let sid = str_arg(p, "session_id")?.to_owned();
                let task = str_arg(p, "task")?.to_owned();
                ensure!(
                    !task.trim().is_empty() && task.len() <= 64000,
                    "Task must contain 1–64000 bytes"
                );
                let mut jobs = self.runtime.jobs.lock().await;
                ensure!(
                    !jobs.contains_key(&sid),
                    "This conversation is already running"
                );
                let session = self.storage.get(&sid).await?;
                ensure!(!jobs.keys().any(|other| other == &sid), "Session busy");
                // One running agent in this foundation release prevents overlapping workspace edits.
                ensure!(
                    jobs.is_empty(),
                    "Another agent is running. Stop it before starting a new task."
                );
                let c = self.config.lock().await;
                let selection_mode = match p.get("context_selection") {
                    None => agent_core::SelectionMode::Auto,
                    Some(mode) if mode == "auto" => agent_core::SelectionMode::Auto,
                    Some(mode) if mode == "snap" => agent_core::SelectionMode::Snap,
                    Some(mode) if mode == "local" => agent_core::SelectionMode::Local,
                    _ => anyhow::bail!("context_selection must be auto, local, or snap"),
                };
                let snippet_packing = match p.get("context_packing") {
                    None => true,
                    Some(mode) if mode == "snippets" => true,
                    Some(mode) if mode == "whole_files" => false,
                    _ => anyhow::bail!("context_packing must be snippets or whole_files"),
                };
                let subq_review = p.get("subq_review").map_or(Ok(true), |value| {
                    value.as_bool().context("subq_review must be a boolean")
                })?;
                let approval_mode = p
                    .get("approval_mode")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                let required_checks: Vec<String> = p
                    .get("required_checks")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()?
                    .unwrap_or_default();
                ensure!(
                    required_checks.len() <= 16
                        && required_checks
                            .iter()
                            .all(|check| !check.trim().is_empty() && check.len() <= 8192),
                    "Provide at most 16 nonempty required check commands"
                );
                let (approval_mode, _) = watch::channel(approval_mode);
                let isolated_checks = match p.get("check_mode") {
                    None => None,
                    Some(mode) if mode == "approved" => None,
                    Some(mode) if mode == "isolated" => {
                        Some(agent_tools::IsolatedChecks::from_environment()?)
                    }
                    _ => anyhow::bail!("check_mode must be approved or isolated"),
                };
                let (model, provider): (String, Box<dyn agent_models::ModelProvider>) =
                    if let Some(model) = p.get("eval_codex_model") {
                        let model = model.as_str().context("Invalid Codex eval model")?;
                        ensure!(
                            model.starts_with("gpt-")
                                && model.len() < 120
                                && model
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
                            "Invalid Codex eval model"
                        );
                        let executable = std::env::var_os("SUBSTRATE_EVAL_CODEX")
                            .context("Codex eval is disabled for this server")?;
                        let selector = if c.key.is_empty() {
                            ensure!(
                                selection_mode == agent_core::SelectionMode::Local,
                                "Use local selection when evaluating Codex without a SubQ key"
                            );
                            None
                        } else {
                            Some(agent_models::Subquadratic::new(c.key.clone())?)
                        };
                        (
                            model.into(),
                            Box::new(agent_models::Codex::new(executable.into(), selector)?),
                        )
                    } else {
                        ensure!(
                            !c.key.is_empty(),
                            "Add your Subquadratic API key in Settings"
                        );
                        (
                            c.model.clone(),
                            Box::new(agent_models::Subquadratic::new(c.key.clone())?),
                        )
                    };
                let config = agent_core::RunConfig {
                    selection_mode,
                    snippet_packing,
                    subq_review,
                    required_checks,
                    isolated_checks,
                    approval_mode: approval_mode.clone(),
                    model,
                    max_steps: c.max_steps,
                    context_budget_bytes: c.context_budget_bytes,
                };
                drop(c);
                let token = CancellationToken::new();
                jobs.insert(
                    sid.clone(),
                    Job {
                        cancel: token.clone(),
                        approval_mode,
                    },
                );
                drop(jobs);
                let server = self.clone();
                tokio::spawn(async move {
                    let result = agent_core::run(
                        session,
                        task,
                        config,
                        server.storage.clone(),
                        server.runtime.clone(),
                        token.clone(),
                        provider,
                    )
                    .await;
                    server.runtime.jobs.lock().await.remove(&sid);
                    let (method, message) = match result {
                        Ok(_) => ("agent.completed", None),
                        Err(e) if token.is_cancelled() => {
                            ("agent.interrupted", Some(e.to_string()))
                        }
                        Err(e) => ("agent.failed", Some(e.to_string())),
                    };
                    server
                        .runtime
                        .emit(method, json!({"session_id":sid,"message":message}))
                        .await;
                });
                Ok(json!({"started":true}))
            }
            "agent.set_approval_mode" => {
                let sid = str_arg(p, "session_id")?;
                let mode = serde_json::from_value(p["approval_mode"].clone())?;
                if let Some(job) = self.runtime.jobs.lock().await.get(sid) {
                    job.approval_mode.send_replace(mode);
                }
                Ok(json!(true))
            }
            "agent.interrupt" | "session.cancel" => {
                if let Some(job) = self
                    .runtime
                    .jobs
                    .lock()
                    .await
                    .get(str_arg(p, "session_id")?)
                {
                    job.cancel.cancel();
                }
                Ok(json!(true))
            }
            "tool.approve" | "tool.reject" => {
                self.runtime
                    .resolve(
                        str_arg(p, "session_id")?,
                        str_arg(p, "tool_id")?,
                        r.method == "tool.approve",
                    )
                    .await?;
                Ok(json!(true))
            }
            "files.list" => {
                agent_tools::entries(&self.root(p).await?, p["path"].as_str().unwrap_or("")).await
            }
            "files.read" => Ok(
                json!({"content":agent_tools::read(&self.root(p).await?,str_arg(p,"path")?).await?}),
            ),
            "git.branch" => Ok(json!(
                agent_tools::git(&self.root(p).await?, &["branch", "--show-current"])
                    .await
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            )),
            "git.status" => agent_tools::changes(&self.root(p).await?).await,
            "git.file" => {
                let root = self.root(p).await?;
                let path = str_arg(p, "path")?;
                agent_tools::resolve(&root, path)?;
                let before = agent_tools::git(
                    &root,
                    &[
                        "show",
                        "--no-ext-diff",
                        "--no-textconv",
                        &format!("HEAD:{path}"),
                    ],
                )
                .await
                .unwrap_or_default();
                let after = agent_tools::read(&root, path).await.unwrap_or_default();
                Ok(json!({"path":path,"before":before,"after":after}))
            }
            _ => anyhow::bail!("Unknown method: {}", r.method),
        }
    }
}
async fn agent_models_list(key: String) -> Result<Value> {
    agent_models::Subquadratic::new(key)?.models().await
}
#[tokio::main]
async fn main() -> Result<()> {
    let db = std::env::var_os("SUBSTRATE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".substrate/sessions.db")
        });
    let storage = Storage::open(&db)
        .await
        .context("Opening session database")?;
    storage.recover_interrupted().await?;
    let config_path = db.with_file_name("config.json");
    let saved: Value = tokio::fs::read(&config_path)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let (tx, mut rx) = mpsc::channel::<Value>(256);
    let runtime = Runtime::new(tx.clone());
    let server = Server {
        storage,
        runtime: runtime.clone(),
        config_path,
        config: Arc::new(Mutex::new(Config {
            key: std::env::var("SUBQ_API_KEY").unwrap_or_default(),
            model: agent_models::current_model(
                saved["model"]
                    .as_str()
                    .unwrap_or(agent_models::DEFAULT_MODEL),
            )
            .into(),
            max_steps: saved["max_steps"]
                .as_u64()
                .unwrap_or(DEFAULT_MAX_STEPS)
                .clamp(1, MAX_MAX_STEPS) as usize,
            context_budget_bytes: saved["context_budget_bytes"]
                .as_u64()
                .unwrap_or(agent_context::DEFAULT_REPO_BUDGET as u64)
                .clamp(32 * 1024, 4 * 1024 * 1024) as usize,
        })),
    };
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(v) = rx.recv().await {
            let mut b = serde_json::to_vec(&v)?;
            b.push(b'\n');
            out.write_all(&b).await?;
            out.flush().await?;
        }
        Ok::<_, anyhow::Error>(())
    });
    let mut stdin = BufReader::new(tokio::io::stdin());
    loop {
        let mut bytes = Vec::new();
        let count = (&mut stdin)
            .take((agent_protocol::MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .await?;
        if count == 0 {
            break;
        }
        if bytes.len() > agent_protocol::MAX_FRAME {
            eprintln!("Protocol frame exceeded limit");
            break;
        }
        let line = String::from_utf8(bytes);
        let request = line
            .map_err(anyhow::Error::from)
            .and_then(|line| Request::parse(&line));
        match request {
            Ok(r) => {
                let s = server.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let value = match s.dispatch(&r).await {
                        Ok(result) => success(r.id, result),
                        Err(e) => failure(r.id, -32000, &e.to_string()),
                    };
                    let _ = tx.send(value).await;
                });
            }
            Err(e) => {
                let _ = tx.send(failure(Value::Null, -32700, &e.to_string())).await;
            }
        }
    }
    for job in runtime.jobs.lock().await.values() {
        job.cancel.cancel();
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    writer.abort();
    Ok(())
}
