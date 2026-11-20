use super::run_command;
use anyhow::{Result, ensure};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::{process::Command, sync::mpsc};
use tokio_util::sync::CancellationToken;

const SANDBOX: &str = "/usr/bin/sandbox-exec";
const CHECK_SHELL: &str = "/bin/dash";

/// Read roots are chosen by the operator of the local harness, never by the model.
pub struct IsolatedChecks {
    read_roots: Vec<PathBuf>,
}

impl IsolatedChecks {
    pub fn from_environment() -> Result<Self> {
        ensure!(
            cfg!(target_os = "macos")
                && Path::new(SANDBOX).is_file()
                && Path::new(CHECK_SHELL).is_file(),
            "Isolated checks require macOS sandbox-exec and dash"
        );
        let roots = std::env::var_os("SUBSTRATE_CHECK_READ_ROOTS").unwrap_or_default();
        let mut read_roots = Vec::new();
        if !roots.is_empty() {
            for root in std::env::split_paths(&roots) {
                ensure!(root.is_absolute(), "Check read roots must be absolute");
                let root = root.canonicalize()?;
                ensure!(
                    root.is_dir() && root.parent().is_some(),
                    "Check read roots must be directories narrower than the filesystem root"
                );
                read_roots.push(root);
            }
        }
        Ok(Self { read_roots })
    }

    pub async fn run(
        &self,
        root: &Path,
        script: &str,
        cancel: &CancellationToken,
        output: mpsc::Sender<String>,
    ) -> Result<Value> {
        ensure!(
            !script.trim().is_empty() && script.len() < 16_000,
            "Invalid command"
        );
        let temporary = tempfile::Builder::new()
            .prefix("substrate-check-")
            .tempdir()?;
        let scratch = temporary.path().canonicalize()?;
        let profile = self.profile(root, &scratch);
        let mut cmd = Command::new(SANDBOX);
        let paths = self.read_roots.iter().map(|path| path.join("bin")).chain(
            [
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
        cmd.args(["-p", &profile, CHECK_SHELL, "-c", script])
            .env_clear()
            .env("PATH", std::env::join_paths(paths)?)
            .env("HOME", &scratch)
            .env("TMPDIR", &scratch)
            .env("TMP", &scratch)
            .env("TEMP", &scratch)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("LC_ALL", "en_US.UTF-8");
        run_command(root, &mut cmd, cancel, output).await
    }

    fn profile(&self, root: &Path, scratch: &Path) -> String {
        let runtime = [
            "/System",
            "/usr/bin",
            "/usr/sbin",
            "/usr/lib",
            "/usr/libexec",
            "/usr/share",
            "/usr/local/bin",
            "/usr/local/lib",
            "/bin",
            "/sbin",
            "/Library/Apple",
            "/Library/Developer",
            "/opt/homebrew/bin",
            "/opt/homebrew/lib",
            "/opt/homebrew/share",
            "/opt/homebrew/opt",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/Frameworks",
            "/private/var/db/dyld",
            "/private/var/db/timezone",
        ];
        let readable = runtime
            .into_iter()
            .map(Path::new)
            .chain(self.read_roots.iter().map(PathBuf::as_path))
            .chain([root, scratch])
            .map(|path| format!("(subpath {})", serde_json::json!(path.to_string_lossy())))
            .collect::<Vec<_>>()
            .join(" ");
        let scratch = serde_json::json!(scratch.to_string_lossy());
        format!(
            "(version 1)\n(allow default)\n(deny network*)\n(deny mach-lookup)\n(deny appleevent-send)\n(deny file-read-data)\n(deny file-write*)\n\
             (allow file-read-data {readable} (literal \"/\" \"/dev/null\" \"/dev/urandom\" \"/dev/random\" \"/private/etc/localtime\"))\n\
             (allow file-write* (subpath {scratch}) (literal \"/dev/null\"))"
        )
    }
}
