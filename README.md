# Substrate

A native macOS workspace for working on code with **Subquadratic models**. Electron and React provide the desktop interface; a custom, independent Rust harness owns the agent loop, tools, approvals, streaming, and persistence.

## Run

Requires Node.js 22.12+ (24 recommended), a stable Rust toolchain, Git, and ripgrep (`brew install ripgrep`). On macOS, install the Xcode command line tools if needed: `xcode-select --install`.

```sh
npm ci
npm run dev
```

1. Open **Settings** (`⌘,`), enter your **Subquadratic API key**, and test the connection.
2. Open a local repository (`⌘O`). Any local directory works; Git features require a Git repository.
3. Describe a coding task and press Enter. Shift+Enter inserts a new line.
4. Review proposed edits and commands, then approve or decline each action. Escape or the stop button interrupts the task.
5. Open the right panel for files and Git diffs, and the terminal button for live command output.

The default model is `subq-preview`, served directly by `https://api.subq.ai/v1/chat/completions`. **SubQ Preview requires organization early-access permission.** A valid key without this permission will receive HTTP 403. Settings checks `/v1/models` for access. `subq-snap-1.1` is a decision model, so it is deliberately excluded from the generative coding loop. There is no silent fallback to another provider.

For development, `SUBQ_API_KEY` can supply a key to the harness instead of entering one in Settings. Never commit a key. The UI never receives saved credentials back from the backend.

## Build a Mac application

```sh
npm run package:mac
# release/mac-arm64/Substrate.app on Apple Silicon
open release/mac-arm64/Substrate.app
```

`npm run dist:mac` also creates a DMG. These local builds are unsigned and unnotarized. A distribution release needs your own Apple signing identity and notarization setup. The release Rust binary and ripgrep are bundled in the application; Cargo is not needed by users of the built app. Build on the target Mac architecture.

## Automatic GitHub releases

The [Release macOS workflow](.github/workflows/release.yml) builds and publishes an Apple Silicon DMG, ZIP, and `SHA256SUMS.txt` when you push a version tag such as `v0.2.0`. Builds run on a native ARM Mac. Source checks, Rust tests, a packaged-app desktop test, and installer integrity checks must pass before publication.

Before tagging, set the same version in `package.json`, `package-lock.json`, and `Cargo.toml` under `[workspace.package]`. For example:

```sh
npm version 0.2.0 --no-git-tag-version
# Set workspace.package.version = "0.2.0" in Cargo.toml.
cargo check --workspace          # Updates local crate versions in Cargo.lock.
node scripts/release-info.mjs    # Checks that all release versions agree.
git add package.json package-lock.json Cargo.toml Cargo.lock
git commit -m "Release 0.2.0"
git tag v0.2.0
git push origin main
git push origin v0.2.0
```

A stable tag publishes a normal release; versions such as `0.2.0-rc.1` publish a prerelease. Invalid tags and mismatched versions stop before the Mac build. Tags must point to a commit containing this workflow. Existing published releases are never overwritten. An interrupted draft can be resumed by rerunning the failed workflow.

No repository secrets or provider API keys are required: the publish job uses GitHub's built-in token with `contents: write`; build jobs only have read access. Actions are pinned to commit SHAs. CI bundles an upstream ripgrep binary pinned by checksum and checks that it links only to macOS system libraries. The downloads remain unsigned and unnotarized until Apple signing credentials are configured.

For a build-only check, run **Actions → Release macOS → Run workflow**, or:

```sh
gh workflow run release.yml --ref main
```

Manual runs execute the same build and test steps and save the downloadable files as Actions artifacts for 14 days. They do not create a tag or release. Failed desktop tests save available screenshots for diagnosis.

## Verify

```sh
npm run check             # Rust formatting, Clippy, strict TypeScript
npm test                  # Rust unit tests + real-server integration tests
npx electron-vite build   # build renderer, main, and preload
npm run test:desktop      # launches the real Electron app and Rust harness
npm audit
```

Tests use local HTTP fixtures implementing the documented SubQ streaming shape. They do not spend API credits or require a real API key. Integration tests cover multiple tool calls, fragmented arguments and Unicode, approvals, rejection without writes, command output, cancellation, workspace boundaries, JSON-RPC correlation, and SQLite restoration. Desktop tests exercise the actual preload and IPC, Monaco diffs, terminal output, Settings model discovery, restored conversations, crash recovery, and renderer isolation. Only the native folder picker is replaced with a deterministic test selection. Screenshots are written to `test-results/`.

**Live Subquadratic inference must be verified using your own enabled API key.** Fixture tests validate the integration contract, not model quality or your account access.

## Architecture

```text
electron/                 Native lifecycle, encrypted key storage, child supervision, IPC
src/                      React + TypeScript, Zustand, Tailwind, shadcn-style primitives
shared/                   Renderer/preload TypeScript protocol types
crates/
  agent-protocol/         JSON-RPC envelopes, message/session/tool schemas
  agent-models/           Provider interface, direct SubQ HTTP + streaming parser
  agent-context/          Repository instructions and explicit context budget
  agent-tools/            File read/list/search, exact patches, shell commands, Git
  agent-runtime/          Cancellation tokens, events, approval coordination
  agent-storage/          SQLx + SQLite sessions, context, messages, tool history
  agent-core/             Custom model → validated tools → results execution loop
  agent-server/           Independent JSON-RPC stdio service
```

The Rust server has no Electron dependency. Run it independently:

```sh
cargo run -p agent-server
# One JSON-RPC 2.0 request per line on stdin; replies/events are stdout only.
# Example: {"jsonrpc":"2.0","id":1,"method":"ping","params":{}}
```

See [docs/protocol.md](docs/protocol.md) for methods and events. React has no Node access; the sandboxed, context-isolated preload exposes a fixed API. The Electron main process validates the sender and method allowlist, correlates requests, and owns the Rust process. Remote navigation, new windows, and Chromium permission requests are blocked. Monaco, fonts, and editor workers are bundled locally.

## Data and execution boundaries

- Desktop data lives in Electron's `app.getPath('userData')` (normally `~/Library/Application Support/Substrate/`). SQLite persists conversation history, complete model context, and tool arguments/results. It is local and **not encrypted**.
- Keys entered in Settings are encrypted using Electron `safeStorage` and OS-provided cryptography, saved with mode `0600`, decrypted only in the main process, and sent to Rust over its private stdin pipe. Linux plaintext credential backends are refused. API keys are not written to SQLite or returned through configuration RPC.
- Model inference sends task history and tool results to Subquadratic. Requests use `store: false`; this does not mean inference happens locally. Conversations are saved locally by Substrate.
- File tools reject absolute paths, parent traversal, Git internals, and symlinks that escape the selected workspace. Directory listings skip symlinks, `.git`, dependency folders, and build output. Read/edit size is capped at 512 KiB.
- Every patch and shell command requires a separate approval. An edit preview shows exact before/after text. The write fails if the file has changed while awaiting approval. Patches use exact, unique text matches and an atomic rename.
- **Approval and path validation are not an OS security sandbox.** Approved commands run with your user permissions and can access the host filesystem and network. Run only on repositories you trust, and read commands before approving. This release has no “approve everything” switch.
- Shell commands have a 120-second timeout, bounded captured output, closed stdin, and cancellation. Unix process groups are killed to clean up shell descendants. Provider credential environment variables are removed from tool subprocesses. On other platforms, process-tree isolation needs additional work.
- The custom loop allows up to 20 model turns by default and one active agent per app. Multiple calls in a turn run sequentially. Invalid tool arguments and tool failures return recoverable model-visible errors. No command or edit is automatically retried.
- The context budget is an explicit conservative **360 KB serialized-input limit**, not a claim about the model's maximum context or an exact token count. On reaching it, start a new conversation. History is never silently compacted or truncated.

## Current limits

This is the functional foundation and coding vertical slice, not the entire roadmap. The editor is a read-only file/diff viewer; edits go through reviewed agent tools. The terminal displays actual agent command output and is not an interactive PTY. Tree-sitter indexing, semantic context selection, compaction, additional providers, worktree isolation, checkpoints, and concurrent/subagents remain future work. There are no simulated agent actions in the application.

`SUBSTRATE_DB` overrides the database path for standalone development/tests. `SUBSTRATE_API_BASE` overrides the provider base URL for local contract fixtures; it is a process-level development setting and is never configurable by the renderer. Leaving it unset uses only Subquadratic.
