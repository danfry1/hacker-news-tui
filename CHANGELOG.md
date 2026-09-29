# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-09-29

### Added

- Jump straight to a story by its number with `:` (e.g. `:10` then `enter`),
  vim-style. Numbers beyond the stories loaded so far fetch ahead until the
  target is reachable. Also works in the bookmarks view.
- Search with `/`, with `n` / `N` for the next / previous match (wrapping
  around). The selection moves as the query is typed, `esc` restores it, and
  matches are highlighted. In the feed and bookmarks it searches titles and
  domains; in a discussion it searches comment text and authors. Thanks to
  @csabaxyz for suggesting this (#13).
- `O` opens a story's Hacker News discussion page in the browser (from the
  feed, bookmarks, or an open discussion), whatever the story links to.
- Colour support for more terminals. Where 24-bit colour isn't available
  (detected for macOS Terminal.app), colours are mapped to the nearest of the
  standard 256 instead of rendering incorrectly. `NO_COLOR` is honored, with
  the selection and search matches shown in reverse video. `HN_TUI_COLOR`
  (`truecolor`, `256`, or `none`) overrides detection.
- `u` opens links from the selected comment, stepping through them on repeat
  presses. Links are taken from the comment's HTML, so URLs that Hacker News
  abbreviates in the text still open in full. Comments with links show a
  `↗ N links` badge.
- Discussions longer than the loading limit (250 comments) now say so, e.g.
  "showing the first 250 of 1,204 comments", and point to `O` for the full
  thread, instead of silently showing part of it.
- Optional mouse support, enabled in the settings pane (`,`): the wheel moves
  the selection, clicking a story selects it, and clicking it again opens its
  comments. It is off by default, since capturing the mouse takes over the
  terminal's own text selection. Changing a preference like this is saved on
  its own, without enabling any data persistence.

### Changed

- Refreshing (`r`) keeps the selected story selected at its new position in
  the ranking, loading further stories if it has dropped below the first
  page, instead of jumping back to the top.

### Fixed

- Saving state is now atomic (written to a temporary file, then renamed into
  place). Previously an interrupted save could leave a truncated `state.json`
  that loaded as empty and was then saved over, losing bookmarks.
- The remembered read history is capped at the 5,000 most recent stories, so
  the state file no longer grows without bound.
- Centered status messages ("fetching stories…", load errors, empty views)
  could be invisible, depending on the terminal height, because they were
  squeezed into a zero-height area. They now always render, and multi-line
  load errors show their details along with a "press r to retry" hint.
- Story numbers of 100 and above no longer shift their titles out of
  alignment with the rest of the list.
- Long titles are truncated to leave room for the `(domain)` suffix instead of
  pushing it off-screen; the domain is dropped only when the row is too narrow
  to show a readable title beside it.
- On narrow terminals, the footer drops lower-priority key hints instead of
  running off the edge (`? help` and `q quit` are always shown), and the
  header drops its "Hacker News" label rather than colliding with the
  loading/live status.

### Security

- On Windows, links were opened with `cmd /C start`, so a URL containing `&`
  (for example in a story's link, or a crafted query string) could end the
  command and run arbitrary commands. Links are now opened through the
  system's URL protocol handler with no command-line parsing, and on every
  platform only `http(s)` links are opened, with characters that are invalid
  in URLs but meaningful to shells percent-encoded first.
- Update `ratatui` to 0.30 (and `crossterm` to 0.29). This drops the
  transitive `lru` 0.12 (RUSTSEC-2026-0002, RUSTSEC-2026-0253, unsound) and
  the unmaintained `paste` (RUSTSEC-2024-0436), leaving `cargo audit` with no
  warnings. The minimum supported Rust version rises to 1.88, as ratatui 0.30
  requires.
- Update `rustls` to 0.23.45 (and `rustls-webpki` to 0.103.15) to address
  [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285), in
  which TLS 1.3 handshake messages could be accepted across encryption level
  boundaries.

## [0.1.4] - 2026-07-09

### Added

- Choose the browser used to open links via the `$HN_TUI_BROWSER` environment
  variable (e.g. `lynx` or `firefox --new-window`). Because it is app-specific,
  it can be exported in a shell profile without changing the system default
  browser. The standard `$BROWSER` is honored as a fallback, and the OS default
  is used when neither is set. A failed launch (e.g. a mistyped command) now
  shows a toast instead of failing silently. Thanks to @gsmitheidw for
  suggesting this (#9).

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

[Unreleased]: https://github.com/danfry1/hacker-news-tui/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/danfry1/hacker-news-tui/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/danfry1/hacker-news-tui/releases/tag/v0.1.0
