use agent_protocol::{Request, failure, success};
use agent_runtime::Runtime;
use agent_storage::Storage;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;
struct Config {
    key: String,
    model: String,
    max_steps: usize,
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
                Ok(json!({"model":c.model,"has_key":!c.key.is_empty(),"max_steps":c.max_steps}))
            }
            "config.credentials" => {
                let key = str_arg(p, "key")?;
                ensure!(key.len() < 8192, "Invalid API key");
                self.config.lock().await.key = key.into();
                Ok(json!(true))
            }
            "config.update" => {
                let model = str_arg(p, "model")?;
                ensure!(
                    model.starts_with("subq-") && !model.contains("snap") && model.len() < 120,
                    "Select a SubQ generative model; Snap uses a separate Decisions API"
                );
                let max_steps = p["max_steps"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
                let mut c = self.config.lock().await;
                c.model = model.into();
                c.max_steps = max_steps;
                tokio::fs::write(
                    &self.config_path,
                    serde_json::to_vec(&json!({"model":model,"max_steps":max_steps}))?,
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
                Ok(serde_json::to_value(
                    self.storage.create(root.to_string_lossy().into()).await?,
                )?)
            }
            "session.list" => Ok(serde_json::to_value(self.storage.list().await?)?),
            "session.resume" => {
                let session = self.storage.get(str_arg(p, "session_id")?).await?;
                let running = self.runtime.jobs.lock().await.contains_key(&session.id);
                let mut result = serde_json::to_value(session)?;
                result["running"] = json!(running);
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
                ensure!(
                    !c.key.is_empty(),
                    "Add your Subquadratic API key in Settings"
                );
                let config = agent_core::RunConfig {
                    key: c.key.clone(),
                    model: c.model.clone(),
                    max_steps: c.max_steps,
                };
                drop(c);
                let token = CancellationToken::new();
                jobs.insert(sid.clone(), token.clone());
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
            "agent.interrupt" | "session.cancel" => {
                if let Some(t) = self
                    .runtime
                    .jobs
                    .lock()
                    .await
                    .get(str_arg(p, "session_id")?)
                {
                    t.cancel();
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
            model: saved["model"].as_str().unwrap_or("subq-preview").into(),
            max_steps: saved["max_steps"].as_u64().unwrap_or(20).clamp(1, 50) as usize,
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
    for token in runtime.jobs.lock().await.values() {
        token.cancel();
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    writer.abort();
    Ok(())
}
