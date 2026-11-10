#!/usr/bin/env bash
set -euo pipefail

# The Homebrew build links to Homebrew's PCRE2 dylib. Bundle the portable upstream
# binary instead, pinned by version and checksum, so users don't need Homebrew.
test "$(uname -s)" = Darwin
test "$(uname -m)" = arm64
rg_version=15.2.0
rg_sha256=3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4
archive="ripgrep-${rg_version}-aarch64-apple-darwin.tar.gz"
tool_dir="${RUNNER_TEMP:?RUNNER_TEMP must be set}/substrate-ripgrep"
mkdir -p "$tool_dir"
curl --fail --silent --show-error --location --retry 3 \
  "https://github.com/BurntSushi/ripgrep/releases/download/${rg_version}/${archive}" \
  --output "$tool_dir/$archive"
printf '%s  %s\n' "$rg_sha256" "$tool_dir/$archive" | shasum -a 256 --check
tar -xzf "$tool_dir/$archive" -C "$tool_dir"
bin_dir="$tool_dir/ripgrep-${rg_version}-aarch64-apple-darwin"

while IFS= read -r library; do
  case "$library" in
    /usr/lib/*|/System/Library/*) ;;
    *) echo "ripgrep depends on a non-system library: $library" >&2; exit 1 ;;
  esac
done < <(otool -L "$bin_dir/rg" | tail -n +2 | awk '{print $1}')

"$bin_dir/rg" --version
printf '%s\n' "$bin_dir" >> "${GITHUB_PATH:?GITHUB_PATH must be set}"
