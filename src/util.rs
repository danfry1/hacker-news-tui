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
            if entity.len() <= 10 {
                if let Some(ch) = decode_one(entity) {
                    out.push(ch);
                    rest = &tail[semi + 1..];
                    continue;
                }
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

/// Open a URL in the user's default browser without pulling in a dependency.
pub fn open_in_browser(url: &str) {
    let _ = browser_command(url).map(|mut c| {
        let _ = c.spawn();
    });
}

#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> Option<std::process::Command> {
    let mut c = std::process::Command::new("open");
    c.arg(url);
    Some(c)
}

#[cfg(target_os = "windows")]
fn browser_command(url: &str) -> Option<std::process::Command> {
    let mut c = std::process::Command::new("cmd");
    c.args(["/C", "start", "", url]);
    Some(c)
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
    fn wrap_respects_width_and_blank_lines() {
        let out = wrap("the quick brown fox\n\njumps", 9);
        assert_eq!(out, vec!["the quick", "brown fox", "", "jumps"]);
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
}
