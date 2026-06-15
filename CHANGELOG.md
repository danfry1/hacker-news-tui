# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3] - 2026-06-15

### Changed

- Lower idle CPU usage: the UI now redraws only in response to input or while
  something is actively loading or animating, rather than on a fixed timer.
- Comment threads load more politely. Item fetches are now capped at a bounded
  number of concurrent requests instead of fanning out all at once, which keeps
  large discussions responsive without flooding the Hacker News API.

## [0.1.2] - 2026-06-15

### Changed

- TLS now trusts the operating system's certificate store (via reqwest's
  `rustls-tls-native-roots` feature) instead of only the bundled Mozilla root
  set. This lets the app connect from behind corporate proxies that present a
  privately-issued root CA, while remaining transparent for everyone else.

## [0.1.1] - 2026-06-14

### Added

- `--version`/`-V` and `--help`/`-h` command-line flags.
- A Nix flake (`nix run`, `nix profile install`) and a Homebrew tap formula
  (`brew install danfry1/tap/hacker-news-tui`) for installation.

### Changed

- Releases no longer ship a prebuilt `x86_64-apple-darwin` (Intel macOS) binary;
  Intel-Mac users can install via `cargo install hacker-news-tui`.

## [0.1.0] - 2026-06-14

Initial release: a terminal UI for browsing Hacker News, built with Ratatui.

### Added

- Browse six feeds — Top, New, Best, Ask, Show, and Jobs — switchable with
  `tab`/`shift+tab` or number keys `1`–`6`.
- Threaded comment view with colored depth guides, collapsible subtrees, and an
  `OP` badge marking the original poster.
- Infinite scroll: the next batch of stories loads and appends automatically as
  the selection nears the end of the list.
- Bookmarks: save stories with `s` and revisit them in a dedicated `★ Saved`
  view (`b`); saved stories are marked with a star in every list.
- In-app settings pane (`,`) to opt in to remembering read-state and bookmarks
  across runs. Persistence is off by default; nothing is written to disk unless
  enabled, and disabling it removes the state file.
- Read-state tracking that dims already-visited stories.
- Open the article or discussion in the system browser with `o`.
- Help overlay (`?`), a context-sensitive footer, a loading spinner, and
  transient status toasts.
- HTML cleanup for comment and self-post text: entities are decoded, tags are
  stripped, and content is word-wrapped to the terminal width.

### Notes

- Asynchronous, non-blocking UI: feeds and whole comment trees are fetched
  concurrently while the interface stays responsive; stale responses are
  discarded via per-request generation counters.
- TLS is provided by `rustls` (no system OpenSSL dependency), and `Cargo.lock`
  is committed to pin every transitive dependency to an exact version.
- Persisted state lives in the platform data directory
  (`~/Library/Application Support/hacker-news-tui/state.json` on macOS).

[Unreleased]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/danfry1/hacker-news-tui/releases/tag/v0.1.0
