use agent_protocol::Preview;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
mod npm;
pub use npm::{npm_start_script, verify_npm_app};
pub const MAX_FILE: u64 = 512 * 1024;
pub fn workspace(path: &str) -> Result<PathBuf> {
    let root = std::fs::canonicalize(path)?;
    ensure!(root.is_dir(), "Select a directory");
    Ok(root)
}
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty() && !path.is_absolute(),
        "Path must be relative to the workspace"
    );
    ensure!(
        path.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "Path traversal is forbidden"
    );
    ensure!(
        !path
            .components()
            .any(|c| matches!(c, Component::Normal(n) if n == ".git")),
        "Git internals are protected"
    );
    let joined = root.join(path);
    let mut ancestor = joined.as_path();
    while !ancestor.exists() {
        // A dangling symlink must not be treated as a safe missing file.
        ensure!(
            std::fs::symlink_metadata(ancestor).is_err(),
            "Dangling symlink is forbidden"
        );
        ancestor = ancestor.parent().context("Invalid path")?;
    }
    ensure!(
        std::fs::canonicalize(ancestor)?.starts_with(root),
        "Path escapes the workspace"
    );
    Ok(joined)
}
pub fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing string argument: {key}"))
}
pub async fn read(root: &Path, path: &str) -> Result<String> {
    let file = resolve(root, path)?;
    ensure!(
        tokio::fs::metadata(&file).await?.len() <= MAX_FILE,
        "File exceeds 512 KiB"
    );
    tokio::fs::read_to_string(file)
        .await
        .context("Cannot read file as UTF-8")
}
pub async fn entries(root: &Path, path: &str) -> Result<Value> {
    let dir = if path.is_empty() {
        root.to_path_buf()
    } else {
        resolve(root, path)?
    };
    let mut reader = tokio::fs::read_dir(dir).await?;
    let mut result = vec![];
    while let Some(entry) = reader.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if [
            ".git",
            "node_modules",
            "target",
            ".DS_Store",
            "out",
            "release",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let ft = entry.file_type().await?;
        if ft.is_symlink() {
            continue;
        }
        result.push(json!({"name":name,"path":entry.path().strip_prefix(root)?.to_string_lossy(),"directory":ft.is_dir()}));
        if result.len() >= 1000 {
            break;
        }
    }
    result.sort_by_key(|v| {
        (
            !v["directory"].as_bool().unwrap_or(false),
            v["name"].as_str().unwrap_or("").to_lowercase(),
        )
    });
    Ok(json!(result))
}
pub async fn preview(root: &Path, args: &Value) -> Result<Preview> {
    let path = string(args, "path")?;
    let file = resolve(root, path)?;
    let old = string(args, "old_text")?;
    let new = string(args, "new_text")?;
    ensure!(new.len() as u64 <= MAX_FILE, "Replacement exceeds 512 KiB");
    let before = if file.exists() {
        read(root, path).await?
    } else {
        String::new()
    };
    let after = if old.is_empty() {
        ensure!(
            !file.exists(),
            "Empty old_text is allowed only for a new file"
        );
        new.to_owned()
    } else {
        ensure!(
            before.matches(old).count() == 1,
            "old_text must match exactly once; read the file and retry"
        );
        before.replacen(old, new, 1)
    };
    ensure!(after.len() as u64 <= MAX_FILE, "Result exceeds 512 KiB");
    Ok(Preview {
        path: path.into(),
        before,
        after,
    })
}
pub async fn apply(root: &Path, p: &Preview) -> Result<Value> {
    let file = resolve(root, &p.path)?;
    let current = if file.exists() {
        read(root, &p.path).await?
    } else {
        String::new()
    };
    ensure!(
        current == p.before,
        "File changed since approval was requested; edit cancelled"
    );
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Validate again after creating parents to catch external symlink changes.
    let file = resolve(root, &p.path)?;
    let temp = file.with_file_name(format!(".substrate-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&temp, &p.after).await?;
    if let Ok(metadata) = tokio::fs::metadata(&file).await {
        tokio::fs::set_permissions(&temp, metadata.permissions()).await?;
    }
    tokio::fs::rename(&temp, &file).await?;
    Ok(json!({"path":p.path,"bytes":p.after.len(),"applied":true}))
}
// Kill the entire process group on Unix, including grandchildren of the shell.
struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}
async fn drain<R: AsyncRead + Unpin>(mut pipe: R, tx: mpsc::Sender<String>) -> Result<String> {
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if output.len() < 64 * 1024 {
            let take = n.min(64 * 1024 - output.len());
            output.extend_from_slice(&buf[..take]);
            let _ = tx
                .send(String::from_utf8_lossy(&buf[..take]).to_string())
                .await;
        }
    }
    Ok(String::from_utf8_lossy(&output).to_string())
}
pub async fn command(
    root: &Path,
    script: &str,
    cancel: &CancellationToken,
    output: mpsc::Sender<String>,
) -> Result<Value> {
    ensure!(
        !script.trim().is_empty() && script.len() < 16_000,
        "Invalid command"
    );
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/D", "/S", "/C", script]);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", script]);
        c
    };
    let (mut child, group, stdout, stderr) = spawn_command(root, &mut cmd, output)?;
    let exit = tokio::select! {
        _=cancel.cancelled()=>Err(anyhow::anyhow!("Command cancelled")),
        result=tokio::time::timeout(Duration::from_secs(120),child.wait())=>match result {
            Ok(status)=>Ok(status?),
            Err(_)=>Err(anyhow::anyhow!("Command timed out after 120 seconds")),
        }
    };
    // A daemon may retain the pipes after its parent exits.
    drop(group);
    if exit.is_err() {
        let _ = child.kill().await;
        stdout.abort();
        stderr.abort();
    }
    Ok(json!({"exit_code":exit?.code(),"stdout":stdout.await??,"stderr":stderr.await??}))
}

type OutputReader = tokio::task::JoinHandle<Result<String>>;

fn spawn_command(
    root: &Path,
    cmd: &mut Command,
    output: mpsc::Sender<String>,
) -> Result<(Child, ProcessGroup, OutputReader, OutputReader)> {
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Credentials must never leak into tool subprocesses.
    for name in ["SUBQ_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
        cmd.env_remove(name);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    let group = ProcessGroup(child.id());
    let out = child.stdout.take().context("No stdout")?;
    let err = child.stderr.take().context("No stderr")?;
    let a = tokio::spawn(drain(out, output.clone()));
    let b = tokio::spawn(drain(err, output));
    Ok((child, group, a, b))
}
pub async fn git(root: &Path, args: &[&str]) -> Result<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("git")
            .args([
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(root)
            .env("GIT_PAGER", "cat")
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        result.status.success(),
        "Git: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.stdout.len() <= 2 * 1024 * 1024,
        "Git output exceeds 2 MiB"
    );
    Ok(String::from_utf8_lossy(&result.stdout).into())
}
pub async fn changes(root: &Path) -> Result<Value> {
    let output = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?;
    let mut parts = output.split('\0');
    let mut changes = vec![];
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let status = &entry[..2];
        changes.push(json!({"status":status.trim(),"path":&entry[3..]}));
        if status.contains('R') || status.contains('C') {
            parts.next();
        }
    }
    Ok(json!(changes))
}
pub fn definitions() -> Vec<Value> {
    let defs = [
        (
            "read_file",
            "Read a UTF-8 file inside the workspace.",
            json!({"path":{"type":"string"}}),
        ),
        (
            "list_files",
            "List a directory. Use an empty path for the workspace root.",
            json!({"path":{"type":"string"}}),
        ),
        (
            "search_repository",
            "Search repository content with ripgrep; returns bounded matching lines.",
            json!({"query":{"type":"string"}}),
        ),
        (
            "apply_patch",
            "Make one exact replacement or create a new file. Approval follows the current mode.",
            json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}}),
        ),
        (
            "execute_command",
            "Run a shell command in the workspace on the host. Requires approval. 120 second timeout.",
            json!({"command":{"type":"string"}}),
        ),
        (
            "verify_npm_app",
            "Verify a new root npm app with a start script. First install its dependencies using an approved npm install if needed. This tool checks installed direct dependency versions against the registry and runs npm start. Electron apps must open a renderer page. Requests approval and stops only the launch it owns.",
            json!({}),
        ),
        (
            "git_diff",
            "Inspect the current tracked Git diff.",
            json!({}),
        ),
    ];
    defs.into_iter().map(|(name,description,properties)| {
        let required:Vec<_>=properties.as_object().unwrap().keys().cloned().collect();
        json!({"type":"function","name":name,"description":description,"strict":true,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
    }).collect()
}
pub fn validate(name: &str, args: &Value) -> Result<()> {
    let defs = definitions();
    let def = defs
        .iter()
        .find(|d| d["name"] == name)
        .context("Unknown tool")?;
    let obj = args
        .as_object()
        .context("Tool arguments must be an object")?;
    let props = def["parameters"]["properties"].as_object().unwrap();
    ensure!(
        obj.len() == props.len()
            && props
                .keys()
                .all(|k| obj.get(k).is_some_and(Value::is_string)),
        "Tool arguments do not match schema"
    );
    Ok(())
}
pub async fn execute(
    root: &Path,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
    output: mpsc::Sender<String>,
) -> Result<Value> {
    validate(name, args)?;
    match name {
        "read_file" => Ok(json!({"content":read(root,string(args,"path")?).await?})),
        "list_files" => entries(root, string(args, "path")?).await,
        "git_diff" => Ok(json!(
            git(
                root,
                &["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"]
            )
            .await?
        )),
        "execute_command" => command(root, string(args, "command")?, cancel, output).await,
        "verify_npm_app" => verify_npm_app(root, cancel, output).await,
        "search_repository" => {
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                Command::new(std::env::var_os("SUBSTRATE_RG").unwrap_or_else(|| "rg".into()))
                    .args([
                        "--line-number",
                        "--no-heading",
                        "--color",
                        "never",
                        "--max-count",
                        "30",
                        "--max-columns",
                        "300",
                        "--glob",
                        "!.env*",
                        "--glob",
                        "!*.pem",
                        "--",
                        string(args, "query")?,
                        ".",
                    ])
                    .current_dir(root)
                    .kill_on_drop(true)
                    .output(),
            )
            .await??;
            ensure!(
                result.status.code().is_some_and(|c| c <= 1),
                "Search failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let text = String::from_utf8_lossy(&result.stdout);
            Ok(json!(text.chars().take(24_000).collect::<String>()))
        }
        _ => bail!("Unsupported tool"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let p = std::env::temp_dir().join(format!("substrate-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::canonicalize(p).unwrap()
    }
    #[test]
    fn blocks_escape_and_invalid_args() {
        let p = root();
        assert!(resolve(&p, "../secret").is_err());
        assert!(resolve(&p, "/etc/passwd").is_err());
        assert!(resolve(&p, ".git/config").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", p.join("escape")).unwrap();
            assert!(resolve(&p, "escape/file").is_err());
        }
        assert!(validate("read_file", &json!({"path":"a","extra":1})).is_err());
        assert!(validate("execute_command", &json!({"command":1})).is_err());
        std::fs::remove_dir_all(p).unwrap();
    }
    #[tokio::test]
    async fn exact_patch_and_stale_approval() {
        let p = root();
        std::fs::write(p.join("file"), "hello hello").unwrap();
        assert!(
            preview(
                &p,
                &json!({"path":"file","old_text":"hello","new_text":"bye"})
            )
            .await
            .is_err()
        );
        let patch = preview(
            &p,
            &json!({"path":"file","old_text":"hello hello","new_text":"bye"}),
        )
        .await
        .unwrap();
        std::fs::write(p.join("file"), "changed").unwrap();
        assert!(apply(&p, &patch).await.is_err());
        let patch = preview(
            &p,
            &json!({"path":"new.txt","old_text":"","new_text":"new"}),
        )
        .await
        .unwrap();
        apply(&p, &patch).await.unwrap();
        assert_eq!(read(&p, "new.txt").await.unwrap(), "new");
        std::fs::remove_dir_all(p).unwrap();
    }
}
