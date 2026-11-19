use super::{command, read, spawn_command};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{process::Command, sync::mpsc, time::Instant};
use tokio_util::sync::CancellationToken;

pub async fn npm_start_script(root: &Path) -> Option<String> {
    let manifest: Value = serde_json::from_str(&read(root, "package.json").await.ok()?).ok()?;
    start_script(&manifest).map(str::to_owned)
}

fn start_script(manifest: &Value) -> Option<&str> {
    manifest["scripts"]["start"]
        .as_str()
        .filter(|script| !script.trim().is_empty())
}

async fn npm_json(
    root: &Path,
    script: &str,
    allow_outdated: bool,
    cancel: &CancellationToken,
    output: &mpsc::Sender<String>,
) -> Result<Value> {
    let result = command(root, script, cancel, output.clone()).await?;
    let data: Value =
        serde_json::from_str(result["stdout"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    let code = result["exit_code"].as_i64();
    ensure!(
        !data.is_null()
            && data.get("error").is_none()
            && (code == Some(0) || (allow_outdated && code == Some(1))),
        "{script} failed: {}",
        result["stderr"].as_str().unwrap_or_default()
    );
    Ok(data)
}

fn stable_major(version: &Value) -> Option<u64> {
    let version = version.as_str()?;
    if version.contains('-') {
        return None;
    }
    version.split_once('.')?.0.parse().ok()
}

pub async fn verify_npm_app(
    root: &Path,
    cancel: &CancellationToken,
    output: mpsc::Sender<String>,
) -> Result<Value> {
    ensure!(
        cfg!(unix),
        "Managed npm launch is unavailable on this platform"
    );
    let manifest: Value = serde_json::from_str(&read(root, "package.json").await?)?;
    ensure!(
        start_script(&manifest).is_some(),
        "package.json needs an explicit start script"
    );
    let sections = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ];
    let dependencies = sections
        .iter()
        .filter_map(|section| manifest[section].as_object());
    let electron = dependencies
        .clone()
        .any(|deps| deps.contains_key("electron"));
    let mut installed = json!({});
    let mut outdated = json!({});
    if dependencies.clone().any(|deps| !deps.is_empty()) {
        let tree = npm_json(root, "npm ls --depth=0 --json", false, cancel, &output)
            .await
            .context(
                "Check that the project's dependencies are installed and satisfy package.json",
            )?;
        installed = tree["dependencies"].clone();
        outdated = npm_json(
            root,
            "npm outdated --depth=0 --json --prefer-online",
            true,
            cancel,
            &output,
        )
        .await?;
    }
    if electron {
        let latest = npm_json(
            root,
            "npm view electron@latest version --json --prefer-online",
            false,
            cancel,
            &output,
        )
        .await?;
        let current = &installed["electron"]["version"];
        let latest_major =
            stable_major(&latest).context("npm did not report a stable Electron release")?;
        // Electron maintains the three most recent stable major releases.
        ensure!(
            stable_major(current).is_some_and(|major| (latest_major.saturating_sub(2)
                ..=latest_major)
                .contains(&major)),
            "Installed Electron {} is outside the supported stable major versions (npm latest is {}). Choose a supported version, install it, and verify again.",
            current,
            latest
        );
    }

    let (renderer, stdout, stderr) = launch(root, electron, cancel, output).await?;
    Ok(json!({
        "verified": true,
        "installed": installed,
        "outdated": outdated,
        "launch": if electron { "Electron renderer observed" } else { "npm start stayed running for the smoke test" },
        "renderer": renderer,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

async fn launch(
    root: &Path,
    electron: bool,
    cancel: &CancellationToken,
    output: mpsc::Sender<String>,
) -> Result<(Option<Value>, String, String)> {
    let profile = tempfile::tempdir()?;
    let mut cmd = Command::new("npm");
    cmd.arg("start");
    if electron {
        cmd.args([
            "--",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
        ]);
        cmd.arg(format!("--user-data-dir={}", profile.path().display()));
        cmd.env_remove("ELECTRON_RUN_AS_NODE");
    }
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(600))
        .build()?;
    let (mut child, group, stdout, stderr) = spawn_command(root, &mut cmd, output)?;
    let deadline = Instant::now() + Duration::from_secs(if electron { 15 } else { 4 });
    let inspect = async {
        if !electron {
            tokio::time::sleep_until(deadline).await;
            return Ok(None);
        }
        let port_file = profile.path().join("DevToolsActivePort");
        let mut first_page_seen = None;
        loop {
            if let Ok(contents) = tokio::fs::read_to_string(&port_file).await
                && let Some(port) = contents
                    .lines()
                    .next()
                    .and_then(|line| line.parse::<u16>().ok())
                && let Ok(response) = http
                    .get(format!("http://127.0.0.1:{port}/json/list"))
                    .send()
                    .await
                && let Ok(pages) = response.json::<Vec<Value>>().await
                && let Some(page) = pages.iter().find(|page| {
                    page["type"] == "page"
                        && page["url"].as_str().is_some_and(|url| {
                            ["file:", "http:", "https:", "app:"]
                                .iter()
                                .any(|prefix| url.starts_with(prefix))
                        })
                })
            {
                if first_page_seen.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_secs(1)
                {
                    return Ok(Some(json!({"title":page["title"],"url":page["url"]})));
                }
            } else {
                first_page_seen = None;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(anyhow::anyhow!("Launch cancelled")),
        status = child.wait() => Err(anyhow::anyhow!("npm start exited before verification: {}", status?)),
        result = tokio::time::timeout_at(deadline + Duration::from_secs(1), inspect) =>
            result.unwrap_or_else(|_| Err(anyhow::anyhow!("No Electron renderer page opened during startup"))),
    };
    drop(group);
    let _ = child.wait().await;
    let stdout = stdout.await??;
    let stderr = stderr.await??;
    match result {
        Ok(page) => Ok((page, stdout, stderr)),
        Err(error) => bail!("{error}\n{stderr}\n{stdout}"),
    }
}
