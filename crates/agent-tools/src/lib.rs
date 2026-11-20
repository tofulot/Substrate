use agent_protocol::Preview;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
mod verification;
pub use npm::{npm_start_script, verify_npm_app};
pub use verification::IsolatedChecks;

pub fn is_shell_tool(name: &str) -> bool {
    matches!(name, "execute_command" | "run_check")
}
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
async fn read_for_model(root: &Path, args: &Value) -> Result<Value> {
    let path = string(args, "path")?;
    let content = read(root, path).await?;
    if args.get("start_line").is_none() && args.get("end_line").is_none() {
        return Ok(json!({"content":content}));
    }
    let start = args["start_line"].as_u64().unwrap_or(1) as usize;
    let end = args["end_line"]
        .as_u64()
        .map_or(start + 199, |line| line as usize);
    ensure!(
        start <= end && end - start < 200,
        "Request at most 200 lines"
    );
    let total = content.lines().count();
    ensure!(
        start <= total,
        "Start line is beyond the end of the file ({total} lines)"
    );
    let mut excerpt = String::new();
    let mut lines = 0;
    for line in content
        .split_inclusive('\n')
        .skip(start - 1)
        .take(end - start + 1)
    {
        let remaining = (24 * 1024usize).saturating_sub(excerpt.len());
        if line.len() > remaining {
            if lines == 0 {
                let boundary = line.floor_char_boundary(remaining);
                return Ok(
                    json!({"path":path,"content":&line[..boundary],"start_line":start,
                    "end_line":start,"total_lines":total,"truncated_line":true,
                    "next_line":(start < total).then_some(start + 1),
                    "sha256":format!("{:x}",Sha256::digest(content.as_bytes()))}),
                );
            }
            break;
        }
        excerpt.push_str(line);
        lines += 1;
    }
    Ok(
        json!({"path":path,"content":excerpt,"start_line":start,"end_line":start + lines - 1,
        "total_lines":total,"next_line":(start + lines <= total).then_some(start + lines),
        "sha256":format!("{:x}",Sha256::digest(content.as_bytes()))}),
    )
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
    let mut cmd = shell(script)?;
    run_command(root, &mut cmd, cancel, output).await
}

fn shell(script: &str) -> Result<Command> {
    ensure!(
        !script.trim().is_empty() && script.len() < 16_000,
        "Invalid command"
    );
    Ok(if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/D", "/S", "/C", script]);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", script]);
        c
    })
}

async fn run_command(
    root: &Path,
    cmd: &mut Command,
    cancel: &CancellationToken,
    output: mpsc::Sender<String>,
) -> Result<Value> {
    let (mut child, group, stdout, stderr) = spawn_command(root, cmd, output)?;
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
            "Read a UTF-8 workspace file. Prefer a line range around a symbol or search match. With no range, reads the whole file.",
            json!({"path":{"type":"string"},"start_line":{"type":"integer","minimum":1,"maximum":524288,"description":"First line, inclusive and 1-based; defaults to 1 when end_line is supplied."},"end_line":{"type":"integer","minimum":1,"maximum":524288,"description":"Last line, inclusive. At most 200 lines; a start without an end reads up to 200 lines."}}),
        ),
        (
            "list_files",
            "List a directory. Use an empty path for the workspace root.",
            json!({"path":{"type":"string"}}),
        ),
        (
            "search_repository",
            "Find text in the workspace. Use literal search for names and source text, and regex mode for patterns. Scope to a file or directory when possible.",
            json!({"query":{"type":"string"},"path":{"type":"string","description":"Optional workspace-relative file or directory. Defaults to the workspace root."},"mode":{"type":"string","enum":["literal","regex"],"description":"Defaults to literal."},"context_lines":{"type":"integer","minimum":0,"maximum":5,"description":"Lines to show on either side of each match. Defaults to zero."}}),
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
            "run_check",
            "Run a targeted test or reproduction. State the behavior it checks. Use kind probe for exploratory checks that may deliberately fail, or verification for a check that must pass. A required command always counts as verification. Requires approval. 120 second timeout.",
            json!({"command":{"type":"string"},"purpose":{"type":"string"},"kind":{"type":"string","enum":["probe","verification"]}}),
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
        let required: Vec<_> = match name {
            "read_file" => vec!["path"],
            "search_repository" => vec!["query"],
            "run_check" => vec!["command", "purpose"],
            _ => properties.as_object().unwrap().keys().map(String::as_str).collect(),
        };
        json!({"type":"function","name":name,"description":description,"strict":true,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
    }).collect()
}

pub fn available_in_isolated_checks(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "list_files" | "search_repository" | "apply_patch" | "run_check" | "git_diff"
    )
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
        def["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .all(|key| obj.contains_key(key.as_str().unwrap()))
            && obj
                .iter()
                .all(|(key, value)| props.get(key).is_some_and(|schema| {
                    if schema["type"] == "integer" {
                        value.as_u64().is_some_and(|n| {
                            n >= schema["minimum"].as_u64().unwrap()
                                && n <= schema["maximum"].as_u64().unwrap()
                        })
                    } else {
                        value.is_string()
                            && schema["enum"]
                                .as_array()
                                .is_none_or(|values| values.contains(value))
                    }
                })),
        "Tool arguments do not match schema"
    );
    if name == "run_check" {
        let command = string(args, "command")?;
        let command = command
            .trim()
            .trim_end_matches(';')
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(
            !matches!(
                command.as_slice(),
                [] | ["true" | "/bin/true" | "/usr/bin/true" | ":"] | ["exit", "0"]
            ),
            "A check must exercise the workspace or validate an observable result"
        );
        let purpose = string(args, "purpose")?;
        ensure!(
            !purpose.trim().is_empty() && purpose.len() <= 512,
            "A check requires a purpose of at most 512 bytes"
        );
    }
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
        "read_file" => read_for_model(root, args).await,
        "list_files" => entries(root, string(args, "path")?).await,
        "git_diff" => Ok(json!(
            git(
                root,
                &["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"]
            )
            .await?
        )),
        "execute_command" | "run_check" => {
            command(root, string(args, "command")?, cancel, output).await
        }
        "verify_npm_app" => verify_npm_app(root, cancel, output).await,
        "search_repository" => {
            let path = args["path"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(".");
            let scoped = resolve(root, path)?.canonicalize()?;
            let scoped = scoped.strip_prefix(root)?;
            ensure!(
                !Path::new(path)
                    .components()
                    .chain(scoped.components())
                    .any(|part| {
                        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
                        name == ".git" || name.starts_with(".env") || name.ends_with(".pem")
                    }),
                "Search path is excluded"
            );
            let context = args["context_lines"].as_u64().unwrap_or(0).to_string();
            let mode = if args["mode"] == "regex" {
                "--no-fixed-strings"
            } else {
                "--fixed-strings"
            };
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                Command::new(std::env::var_os("SUBSTRATE_RG").unwrap_or_else(|| "rg".into()))
                    .args([
                        "--no-config",
                        "--with-filename",
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
                        mode,
                        "--context",
                        &context,
                        "--",
                        string(args, "query")?,
                    ])
                    .arg(Path::new(".").join(scoped))
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
        for command in ["true", ":", "exit 0"] {
            assert!(validate("run_check", &json!({"command":command,"purpose":"verify"})).is_err());
        }
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
