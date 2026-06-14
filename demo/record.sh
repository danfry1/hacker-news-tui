#!/usr/bin/env bash
# Regenerate demo/hacker-news-tui.gif. Requires: vhs (brew install vhs).
#
# Builds the release binary and runs VHS against demo/demo.tape. The app fetches
# live Hacker News data, so the recorded content reflects whatever is on the
# front page at capture time.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo build --release --manifest-path "$REPO/Cargo.toml"

export HACKER_NEWS_TUI_BIN="$REPO/target/release/hacker-news-tui"
( cd "$REPO/demo" && vhs demo.tape )
echo "wrote $REPO/demo/hacker-news-tui.gif"
