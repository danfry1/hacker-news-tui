//! Small, dependency-free helpers: time, URLs, HTML, wrapping, browser opening.

use std::time::{SystemTime, UNIX_EPOCH};

/// Compact "time ago" label, e.g. `5m`, `3h`, `2d`.
pub fn time_ago(t: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(t);
    let d = now.saturating_sub(t);
    match d {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86399 => format!("{}h ago", d / 3600),
        86400..=2591999 => format!("{}d ago", d / 86400),
        2592000..=31535999 => format!("{}mo ago", d / 2_592_000),
        _ => format!("{}y ago", d / 31_536_000),
    }
}

/// Extract a clean display host from a URL (drops scheme and a leading `www.`).
pub fn domain(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then(|| host.to_string())
}

/// Convert HN's HTML snippets into readable plain text: paragraphs become blank
/// lines, tags are stripped, and HTML entities are decoded.
pub fn clean_html(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let with_breaks = s
        .replace("<p>", "\n\n")
        .replace("</p>", "")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");

    // Strip remaining tags.
    let mut stripped = String::with_capacity(with_breaks.len());
    let mut in_tag = false;
    for ch in with_breaks.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => stripped.push(ch),
            _ => {}
        }
    }

    decode_entities(&stripped).trim().to_string()
}

/// The `http(s)` link targets in an HN HTML snippet, in order, deduplicated.
/// These are the real `href`s: HN abbreviates long URLs in the visible link
/// text, so the cleaned text can't be used to open them.
pub fn extract_links(html: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("href=\"") {
        rest = &rest[at + 6..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let url = decode_entities(&rest[..end]);
        rest = &rest[end..];
        let is_web = url.starts_with("https://") || url.starts_with("http://");
        if is_web && !links.contains(&url) {
            links.push(url);
        }
    }
    links
}

/// Decode the HTML entities HN actually emits (named common ones + numeric).
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // Entity names are short ASCII; `find` returns a valid char boundary, so
        // slicing up to it is always safe even when multi-byte chars follow.
        if let Some(semi) = tail.find(';') {
            let entity = &tail[1..semi];
            if entity.len() <= 10
                && let Some(ch) = decode_one(entity)
            {
                out.push(ch);
                rest = &tail[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

fn decode_one(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "hellip" => Some('…'),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        _ => {
            let num = entity.strip_prefix('#')?;
            let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            char::from_u32(code)
        }
    }
}

/// Word-wrap `text` to `width` columns, preserving blank lines between paragraphs.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        if para.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_len = 0;
        for word in para.split_whitespace() {
            let wlen = word.chars().count();
            if cur_len == 0 {
                cur.push_str(word);
                cur_len = wlen;
            } else if cur_len + 1 + wlen <= width {
                cur.push(' ');
                cur.push_str(word);
                cur_len += 1 + wlen;
            } else {
                lines.push(std::mem::take(&mut cur));
                cur.push_str(word);
                cur_len = wlen;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    lines
}

/// Byte ranges of the non-overlapping, ASCII case-insensitive occurrences of
/// `needle` in `haystack`. ASCII folding keeps byte offsets identical between
/// the folded and original strings, so the ranges index `haystack` directly and
/// always fall on char boundaries. An empty needle matches nothing.
pub fn find_ci(haystack: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let hay = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    hay.match_indices(&needle)
        .map(|(i, m)| i..i + m.len())
        .collect()
}

/// Whether `haystack` contains `needle`, ignoring ASCII case.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    !needle.is_empty()
        && haystack
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
}

/// Open a URL in a browser without pulling in a dependency.
///
/// The browser is chosen, in order, from `$HN_TUI_BROWSER` (app-specific, so it
/// can be exported globally without touching the system default), the standard
/// `$BROWSER`, and finally the OS default opener. A value may include arguments
/// (e.g. `firefox --new-window`).
///
/// URLs come from story submitters and commenters, so they are treated as
/// hostile: only `http(s)` links are opened, the URL is normalized by
/// [`safe_url`] so it contains nothing a command line could interpret, and it
/// is always passed as a discrete argument — never through a shell. Returns
/// whether the browser process was spawned successfully.
pub fn open_in_browser(url: &str) -> bool {
    let Some(url) = safe_url(url) else {
        return false;
    };
    let from_env = browser_spec().and_then(|b| build_command(&b, &url));
    match from_env.or_else(|| browser_command(&url)) {
        Some(mut c) => c.spawn().is_ok(),
        None => false,
    }
}

/// `url` made safe to hand to another program, or `None` if it isn't an
/// `http(s)` link. Whitespace, control and non-ASCII bytes, and the characters
/// that are invalid in URLs but meaningful to shells (`"`, `<`, `>`, `\`, `^`,
/// `` ` ``, `{`, `|`, `}`) are percent-encoded, which browsers treat as
/// equivalent. URL syntax such as `&`, `%`, `?` and `#` is left intact.
pub fn safe_url(url: &str) -> Option<String> {
    let scheme_ok = ["https://", "http://"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(scheme))
    });
    if !scheme_ok {
        return None;
    }
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        let unsafe_char = matches!(
            b,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        );
        if b.is_ascii_graphic() && !unsafe_char {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Some(out)
}

/// The first non-empty browser command from the environment, preferring the
/// app-specific `$HN_TUI_BROWSER` over the system-wide `$BROWSER`.
fn browser_spec() -> Option<String> {
    ["HN_TUI_BROWSER", "BROWSER"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.trim().is_empty())
}

/// Build a command from a `program arg1 arg2…` string, appending `url` as a
/// final discrete argument. Returns `None` when the string has no program.
fn build_command(spec: &str, url: &str) -> Option<std::process::Command> {
    let mut parts = spec.split_whitespace();
    let program = parts.next()?;
    let mut c = std::process::Command::new(program);
    c.args(parts);
    c.arg(url);
    Some(c)
}

#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> Option<std::process::Command> {
    let mut c = std::process::Command::new("open");
    c.arg(url);
    Some(c)
}

#[cfg(target_os = "windows")]
fn browser_command(url: &str) -> Option<std::process::Command> {
    Some(windows_command(url))
}

/// Windows' default-browser launcher. This deliberately avoids
/// `cmd /C start`: cmd.exe re-parses its command line, so an `&` in a URL's
/// query would end the `start` command and run the rest as a new one.
/// `url.dll`'s protocol handler hands the URL straight to the shell's
/// registered browser with no command-line parsing.
#[cfg(any(target_os = "windows", test))]
fn windows_command(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("rundll32.exe");
    c.args(["url.dll,FileProtocolHandler", url]);
    c
}

#[cfg(all(unix, not(target_os = "macos")))]
fn browser_command(url: &str) -> Option<std::process::Command> {
    let mut c = std::process::Command::new("xdg-open");
    c.arg(url);
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_clean() {
        assert_eq!(
            domain("https://www.example.com/a/b?x=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            domain("http://news.ycombinator.com").as_deref(),
            Some("news.ycombinator.com")
        );
        assert_eq!(domain("not a url").as_deref(), Some("not a url"));
    }

    #[test]
    fn html_is_decoded_and_stripped() {
        let raw = "<p>Tom &amp; Jerry say &quot;hi&quot;</p><p>line&#x2F;two &gt; one</p>";
        assert_eq!(clean_html(raw), "Tom & Jerry say \"hi\"\n\nline/two > one");
    }

    #[test]
    fn html_handles_multibyte_after_entity() {
        // Regression: a fixed byte-window search used to split the multi-byte
        // ’ (U+2019) and panic on a non-char-boundary slice.
        let raw = "&gt; There’s a certain level of wealth — you can’t earn that.";
        assert_eq!(
            clean_html(raw),
            "> There’s a certain level of wealth — you can’t earn that."
        );
    }

    #[test]
    fn html_lone_ampersand_is_preserved() {
        assert_eq!(clean_html("a & b &nope; c"), "a & b &nope; c");
        assert_eq!(clean_html("Q&A"), "Q&A");
    }

    #[test]
    fn html_keeps_link_text_drops_tags() {
        let raw = r#"see <a href="https://x.com" rel="nofollow">https://x.com</a> now"#;
        assert_eq!(clean_html(raw), "see https://x.com now");
    }

    #[test]
    fn links_come_from_hrefs_not_the_abbreviated_text() {
        let raw = concat!(
            r#"see <a href="https:&#x2F;&#x2F;example.com&#x2F;a&#x2F;very&#x2F;long&#x2F;path" rel="nofollow">"#,
            r#"https:&#x2F;&#x2F;example.com&#x2F;a&#x2F;very&#x2F;l...</a>"#,
            r#" and <a href="http://x.org">x</a>, again <a href="http://x.org">x</a>"#,
            r#" <a href="javascript:alert(1)">no</a>"#,
        );
        assert_eq!(
            extract_links(raw),
            ["https://example.com/a/very/long/path", "http://x.org"]
        );
        assert!(extract_links("no links here").is_empty());
    }

    #[test]
    fn wrap_respects_width_and_blank_lines() {
        let out = wrap("the quick brown fox\n\njumps", 9);
        assert_eq!(out, vec!["the quick", "brown fox", "", "jumps"]);
    }

    #[test]
    fn browser_command_splits_args_and_appends_url() {
        let c = build_command("firefox --new-window", "https://example.com").unwrap();
        assert_eq!(c.get_program(), "firefox");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["--new-window", "https://example.com"]);
    }

    #[test]
    fn only_web_urls_are_opened() {
        assert!(safe_url("https://example.com").is_some());
        assert!(safe_url("HTTP://EXAMPLE.COM").is_some());
        assert!(safe_url("file:///etc/passwd").is_none());
        assert!(safe_url("javascript:alert(1)").is_none());
        assert!(safe_url("calc.exe").is_none());
        assert!(safe_url("").is_none());
    }

    #[test]
    fn urls_are_normalized_for_the_command_line() {
        // URL syntax, including `&` and existing escapes, is kept.
        let q = "https://x.com/a?b=1&c=%20d#e";
        assert_eq!(safe_url(q).unwrap(), q);
        // Quotes, spaces, shell metacharacters and non-ASCII are encoded.
        assert_eq!(
            safe_url("https://x.com/\"a b\"|^<>`{}\\é").unwrap(),
            "https://x.com/%22a%20b%22%7C%5E%3C%3E%60%7B%7D%5C%C3%A9"
        );
        assert_eq!(
            safe_url("https://x.com/\n&whoami").unwrap(),
            "https://x.com/%0A&whoami"
        );
    }

    #[test]
    fn windows_opener_does_not_go_through_cmd() {
        let url = "https://x.com/?a=1&calc.exe";
        let c = windows_command(url);
        assert_eq!(c.get_program(), "rundll32.exe");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["url.dll,FileProtocolHandler", url]);
    }

    #[test]
    fn browser_command_blank_spec_is_none() {
        assert!(build_command("   ", "https://example.com").is_none());
    }

    #[test]
    fn time_ago_buckets() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(time_ago(now), "just now");
        assert_eq!(time_ago(now - 120), "2m ago");
        assert_eq!(time_ago(now - 7200), "2h ago");
    }

    #[test]
    fn find_ci_returns_case_insensitive_byte_ranges() {
        assert_eq!(find_ci("Rust and rust", "RUST"), vec![0..4, 9..13]);
        assert_eq!(find_ci("aaaa", "aa"), vec![0..2, 2..4]); // non-overlapping
        assert!(find_ci("anything", "").is_empty());
        // Ranges stay on char boundaries around multi-byte text.
        let s = "café Rust";
        let r = find_ci(s, "rust");
        assert_eq!(&s[r[0].clone()], "Rust");
    }

    #[test]
    fn contains_ci_ignores_case_and_rejects_empty() {
        assert!(contains_ci("Show HN: Thing", "show hn"));
        assert!(!contains_ci("Show HN: Thing", "ask"));
        assert!(!contains_ci("anything", ""));
    }
}
