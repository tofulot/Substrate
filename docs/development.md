# Development and releases

## Getting started

On macOS, install Node.js 22.12 or newer (CI uses Node.js 24), a stable Rust toolchain, Git, and ripgrep. Install ripgrep with `brew install ripgrep` and the Xcode Command Line Tools with `xcode-select --install` if needed. Install Python 3 to run the integration tests and context evaluations.

```sh
npm ci
npm run dev
```

`npm run dev` builds the debug Rust server and starts Electron with Vite. In **Settings**, save a Subquadratic API key with access to SubQ Savant. **Test connection** checks the models available to that key. For development, the harness also accepts `SUBQ_API_KEY` and `BRAVE_SEARCH_API_KEY` from the environment.

## Build commands

| Command | Output |
| --- | --- |
| `npm run build` | Electron bundles in `out/` and the release Rust server in `target/release/` |
| `npm run package:mac` | An unpacked macOS app in `release/` |
| `npm run dist:mac` | A macOS DMG in `release/` |

Run the packaging commands on the target Mac architecture. The app bundle includes the Rust server and ripgrep. Local packaging copies the ripgrep executable on your PATH. CI uses a portable upstream binary verified by checksum and checks that it links only to macOS system libraries. The app is unsigned and unnotarized with the current packaging configuration.

On Apple Silicon, open the unpacked app with `open release/mac-arm64/Substrate.app`.

## Tests

```sh
npm run check
npm test
node --test tests/release.test.mjs
npx --no-install electron-vite build
npm run test:desktop
```

`npm run check` checks Electron import boundaries, Rust formatting, Clippy, and TypeScript. `npm test` runs Rust unit tests and Python integration tests against the real Rust server. The desktop smoke test starts Electron and the Rust server and saves screenshots under `test-results/`. To smoke-test a packaged build, set `SUBSTRATE_TEST_EXECUTABLE` to the executable inside its app bundle.

These tests use local HTTP fixtures for SubQ streaming and the Decisions API. They exercise the provider contract, context selection, tool approvals, cancellation, persistence, and desktop IPC without an API key. The [pull request workflow](../.github/workflows/pr-checks.yml) also runs the scripted context evaluation when harness or test inputs change.

## Context evaluation

```sh
npm run eval:context
npm run --silent eval:context -- --live --modes local snap auto --repeats 3 > context-report.json
npm run --silent eval:context -- --generator codex --model MODEL_ID --modes local --repeats 3 --jobs 3 --timeout 240 > codex-report.json
```

The scripted evaluation exercises retrieval, promotion of files missed by context selection, and edit approval. Live SubQ evaluation uses `SUBQ_API_KEY` and API credits. For the Codex comparison, use an authenticated Codex CLI and replace `MODEL_ID` with the Codex model to evaluate. The CLI supplies structured generation turns from a temporary directory with its built-in shell and web tools disabled. Substrate runs the same tasks with its local selector and tool approval rules. The desktop app uses Subquadratic.

The suite runs three lookups and two edits on small and large fixture repositories. Each run starts with a fresh workspace and conversation. The report tracks answer or file correctness, verification after the last edit, retrieval stages, calls, timing, and provider-reported input tokens.

## GitHub releases

The [Release macOS workflow](../.github/workflows/release.yml) publishes an Apple Silicon DMG, ZIP, and `SHA256SUMS.txt` for a matching version tag. It runs source checks, Rust and integration tests, a packaged-app smoke test, and installer integrity checks before publishing.

Set the same version in `package.json`, `package-lock.json`, and `[workspace.package]` in `Cargo.toml`. For example, to prepare version `0.4.0`:

```sh
npm version 0.4.0 --no-git-tag-version
# Set workspace.package.version to "0.4.0" in Cargo.toml.
cargo check --workspace
node scripts/release-info.mjs
```

`cargo check` updates crate versions in `Cargo.lock`. Commit the four version files, tag the commit `v0.4.0`, and push the commit and tag. Prerelease tags such as `v0.4.0-rc.1` publish GitHub prereleases. The workflow can resume an interrupted draft on a rerun and leaves published releases intact. Publishing uses GitHub's built-in token.

For a build without publishing, run **Actions → Release macOS → Run workflow** or `gh workflow run release.yml --ref main`. Manual runs save build artifacts for 14 days. Failed desktop tests upload available diagnostics.
