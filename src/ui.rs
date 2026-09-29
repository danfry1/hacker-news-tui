//! All rendering. Pure functions of `&App` (plus the bits of `ListState` that
//! `render_stateful_widget` needs to mutate).

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Wrap,
};

use crate::api::{Feed, Item};
use crate::app::{App, Load, PromptKind, SETTINGS_COUNT, View};
use crate::color::{self, ColorMode};
use crate::util;

const ORANGE: Color = Color::Rgb(255, 102, 0);
const BG: Color = Color::Rgb(20, 22, 26);
const DIM: Color = Color::Rgb(130, 130, 138);
const FAINT: Color = Color::Rgb(90, 90, 98);
const ACCENT: Color = Color::Rgb(120, 170, 255);
const SELECT_BG: Color = Color::Rgb(38, 42, 50);
// Visited titles: clearly muted vs. unread white, but still comfortably legible.
const READ: Color = Color::Rgb(176, 178, 186);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &mut App, colors: ColorMode) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer
    ])
    .split(area);

    draw_header(frame, app, chunks[0]);
    match app.view {
        View::List => draw_list(frame, app, chunks[1]),
        View::Comments => draw_comments(frame, app, chunks[1]),
        View::Bookmarks => draw_bookmarks(frame, app, chunks[1]),
    }
    draw_footer(frame, app, chunks[2]);

    if app.show_help {
        draw_help(frame, area);
    }
    if app.show_settings {
        draw_settings(frame, app, area);
    }

    color::apply(frame.buffer_mut(), colors, BG);
}

fn spinner(app: &App) -> &'static str {
    SPINNER[app.spinner % SPINNER.len()]
}

// ── header ──────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            " Y ",
            Style::default()
                .bg(ORANGE)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " Hacker News ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];

    for (i, feed) in Feed::ALL.iter().enumerate() {
        let selected = *feed == app.feed && app.view == View::List;
        let style = if selected {
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let label = format!("{}·{}", i + 1, feed.title());
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("  "));
    }

    // Saved tab.
    let saved_style = if app.view == View::Bookmarks {
        Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    spans.push(Span::styled(
        format!("★ Saved ({})", app.saved.len()),
        saved_style,
    ));

    // Right-aligned spinner / status.
    let right = if app.is_loading() {
        Line::from(vec![
            Span::styled(spinner(app), Style::default().fg(ORANGE)),
            Span::styled(" loading ", Style::default().fg(DIM)),
        ])
    } else {
        Line::from(Span::styled("● live ", Style::default().fg(Color::Green)))
    };

    // On narrow terminals, drop the "Hacker News" label rather than let the
    // tabs run into the status on the right.
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used + right.width() > area.width as usize {
        spans.remove(1);
    }
    let line = Line::from(spans);

    frame.render_widget(Paragraph::new(line), area);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

// ── story list ───────────────────────────────────────────────────────────────

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let stories = match &app.stories {
        Load::Loading => {
            return draw_center(
                frame,
                area,
                &format!("{} fetching stories…", spinner(app)),
                ORANGE,
            );
        }
        Load::Failed(e) => {
            return draw_center(
                frame,
                area,
                &format!("couldn't load stories\n{e}\n\npress r to retry"),
                Color::Red,
            );
        }
        Load::Ready(s) if s.is_empty() => return draw_center(frame, area, "no stories here", DIM),
        Load::Ready(s) => s,
    };

    let query = app.highlight_query();
    let cols = RowCols::new(stories.len(), area.width);
    let items: Vec<ListItem> = stories
        .iter()
        .enumerate()
        .map(|(i, story)| {
            story_row(
                i,
                story,
                app.visited.contains(&story.id),
                app.is_saved(story.id),
                cols,
                query,
            )
        })
        .collect();

    let items = mark_selected(items, app.list_state.selected());
    let list = story_list(items);
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

// ── bookmarks ────────────────────────────────────────────────────────────────

fn draw_bookmarks(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.saved.is_empty() {
        return draw_center(
            frame,
            area,
            "no bookmarks yet — press s on a story to save it",
            DIM,
        );
    }

    let cols = RowCols::new(app.saved.len(), area.width);
    let items: Vec<ListItem> = app
        .saved
        .iter()
        .enumerate()
        .map(|(i, story)| {
            let read = app.visited.contains(&story.id);
            story_row(i, story, read, true, cols, app.highlight_query())
        })
        .collect();

    let items = mark_selected(items, app.bookmark_state.selected());
    let list = story_list(items);
    frame.render_stateful_widget(list, area, &mut app.bookmark_state);
}

/// Column layout shared by every row of a story list.
#[derive(Clone, Copy)]
struct RowCols {
    /// Digits in the largest row number, so `9.` and `120.` line up.
    num: usize,
    /// Room left for the title.
    title: usize,
}

impl RowCols {
    fn new(rows: usize, area_width: u16) -> Self {
        let num = rows.to_string().len().max(2);
        // highlight symbol + number + ". " + a little breathing room
        let title = (area_width as usize).saturating_sub(num + 4);
        RowCols { num, title }
    }
}

/// A single two-line story row, shared by the feed list and the bookmarks view.
fn story_row(
    i: usize,
    story: &Item,
    read: bool,
    saved: bool,
    cols: RowCols,
    query: Option<&str>,
) -> ListItem<'static> {
    let title_style = if read {
        Style::default().fg(READ)
    } else {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };

    let mut title_spans = vec![Span::styled(
        format!("{:>w$}. ", i + 1, w = cols.num),
        Style::default().fg(ORANGE),
    )];
    if saved {
        title_spans.push(Span::styled("★ ", Style::default().fg(ORANGE)));
    }
    let budget = cols.title.saturating_sub(if saved { 2 } else { 0 });
    let (title_width, domain) = fit_title(
        story.title.chars().count(),
        story.url.as_deref().and_then(util::domain),
        budget,
    );
    title_spans.extend(highlighted(
        &truncate(&story.title, title_width),
        query,
        title_style,
    ));
    if let Some(dom) = domain {
        title_spans.push(Span::styled("  (", Style::default().fg(FAINT)));
        title_spans.extend(highlighted(&dom, query, Style::default().fg(FAINT)));
        title_spans.push(Span::styled(")", Style::default().fg(FAINT)));
    }

    let meta = Line::from(vec![
        Span::raw(" ".repeat(cols.num + 2)),
        Span::styled(format!("▲ {}", story.score), Style::default().fg(ORANGE)),
        Span::styled(format!("  by {}", story.by), Style::default().fg(DIM)),
        Span::styled(
            format!("  · {}", util::time_ago(story.time)),
            Style::default().fg(DIM),
        ),
        Span::styled(
            format!("  · 💬 {}", story.comment_count()),
            Style::default().fg(ACCENT),
        ),
    ]);

    ListItem::new(Text::from(vec![Line::from(title_spans), meta]))
}

/// Give the selected row its background as the item's own base style. The
/// list's `highlight_style` would be patched over the spans instead, hiding
/// any background they set (search hits), so it is left empty.
fn mark_selected(items: Vec<ListItem<'_>>, selected: Option<usize>) -> Vec<ListItem<'_>> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            if Some(i) == selected {
                item.style(Style::default().bg(SELECT_BG))
            } else {
                item
            }
        })
        .collect()
}

/// Split a row's `budget` between the title and its `  (domain)` suffix,
/// returning the width to truncate the title to and the domain to show. The
/// title gets priority: the domain is dropped when keeping it would squeeze
/// the title below a readable length.
fn fit_title(title: usize, domain: Option<String>, budget: usize) -> (usize, Option<String>) {
    const MIN_TITLE: usize = 24;
    let Some(dom) = domain else {
        return (budget, None);
    };
    let suffix = dom.chars().count() + 4; // "  (" + ")"
    if title + suffix <= budget || budget >= suffix + MIN_TITLE {
        (budget.saturating_sub(suffix), Some(dom))
    } else {
        (budget, None)
    }
}

fn story_list(items: Vec<ListItem<'static>>) -> List<'static> {
    List::new(items)
        .highlight_symbol("▌")
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
}

// ── comments ─────────────────────────────────────────────────────────────────

fn draw_comments(frame: &mut Frame, app: &mut App, area: Rect) {
    let width = area.width as usize;
    let header_lines = story_header_lines(app, width.saturating_sub(2));
    let header_height = (header_lines.len() as u16 + 2).min(area.height.saturating_sub(1));

    let chunks =
        Layout::vertical([Constraint::Length(header_height), Constraint::Min(0)]).split(area);

    let header = Paragraph::new(header_lines)
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(FAINT))
                .padding(Padding::horizontal(1)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(header, chunks[0]);

    let body = chunks[1];
    match &app.comments {
        Load::Loading => return draw_comment_skeleton(frame, body, app),
        Load::Failed(e) => return draw_center(frame, body, e, Color::Red),
        Load::Ready(c) if c.is_empty() => {
            return draw_center(frame, body, "no comments yet — be the first on HN", DIM);
        }
        Load::Ready(_) => {}
    }

    let op = app.story.as_ref().map(|s| s.by.clone()).unwrap_or_default();
    let text_width = body.width.saturating_sub(1) as usize;

    let query = app.highlight_query();
    let visible = app.visible_comments();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|flat| {
            let indent = "│ ".repeat(flat.depth);
            let bar = Style::default().fg(thread_color(flat.depth));

            let marker = if flat.has_children {
                if flat.collapsed { "▸ " } else { "▾ " }
            } else {
                "• "
            };
            let is_op = flat.by == op && !op.is_empty();

            let mut head = vec![
                Span::styled(indent.clone(), bar),
                Span::styled(
                    marker,
                    Style::default().fg(if flat.collapsed { ORANGE } else { DIM }),
                ),
            ];
            head.extend(highlighted(
                &flat.by,
                query,
                Style::default()
                    .fg(if is_op { ORANGE } else { ACCENT })
                    .add_modifier(Modifier::BOLD),
            ));
            if is_op {
                head.push(Span::styled(" OP", Style::default().fg(ORANGE)));
            }
            head.push(Span::styled(
                format!("  {}", util::time_ago(flat.time)),
                Style::default().fg(FAINT),
            ));
            if flat.links > 0 && !flat.collapsed {
                let label = if flat.links == 1 { "link" } else { "links" };
                head.push(Span::styled(
                    format!("  ↗ {} {label}", flat.links),
                    Style::default().fg(ACCENT),
                ));
            }
            if flat.collapsed {
                head.push(Span::styled(
                    format!("  [+{} hidden]", flat.hidden),
                    Style::default().fg(DIM),
                ));
            }

            let mut lines = vec![Line::from(head)];
            if !flat.collapsed {
                let body_width = text_width.saturating_sub(flat.depth * 2);
                for wl in util::wrap(&flat.text, body_width) {
                    let mut spans = vec![Span::styled(indent.clone(), bar)];
                    spans.extend(highlighted(&wl, query, Style::default()));
                    lines.push(Line::from(spans));
                }
            }
            lines.push(Line::from(""));
            ListItem::new(Text::from(lines))
        })
        .collect();

    let items = mark_selected(items, app.comment_state.selected());
    let list = List::new(items)
        .highlight_symbol("▌")
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);

    frame.render_stateful_widget(list, body, &mut app.comment_state);
}

fn story_header_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let Some(story) = &app.story else {
        return vec![Line::from("")];
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let title_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    for wl in util::wrap(&story.title, width.max(10)) {
        lines.push(Line::from(Span::styled(wl, title_style)));
    }

    let mut meta = vec![
        Span::styled(format!("▲ {}", story.score), Style::default().fg(ORANGE)),
        Span::styled(format!("  by {}", story.by), Style::default().fg(DIM)),
        Span::styled(
            format!("  · {}", util::time_ago(story.time)),
            Style::default().fg(DIM),
        ),
        Span::styled(
            format!("  · 💬 {}", story.comment_count()),
            Style::default().fg(ACCENT),
        ),
    ];
    if let Some(dom) = story.url.as_deref().and_then(util::domain) {
        meta.push(Span::styled(
            format!("  · {dom}"),
            Style::default().fg(FAINT),
        ));
    }
    lines.push(Line::from(meta));

    if app.comments_truncated {
        lines.push(Line::from(Span::styled(
            format!(
                "showing the first {} of {} comments · O opens the full thread on HN",
                app.comments_loaded,
                story.comment_count()
            ),
            Style::default().fg(ORANGE),
        )));
    }

    // Self/Ask post body, if any.
    if let Some(text) = &story.text {
        let cleaned = util::clean_html(text);
        if !cleaned.is_empty() {
            for wl in util::wrap(&cleaned, width.max(10)).into_iter().take(6) {
                lines.push(Line::from(Span::styled(wl, Style::default().fg(DIM))));
            }
        }
    }
    lines
}

/// An animated placeholder shown while a discussion is being fetched: a banner
/// with the spinner plus comment-shaped bars that shimmer as `app.spinner` ticks,
/// making it obvious that content is on the way.
fn draw_comment_skeleton(frame: &mut Frame, area: Rect, app: &App) {
    // Three greys; a per-row offset driven by the spinner creates a moving wave.
    const SHADES: [Color; 3] = [
        Color::Rgb(48, 50, 56),
        Color::Rgb(70, 72, 80),
        Color::Rgb(96, 98, 108),
    ];
    let shade = |n: usize| SHADES[(n + app.spinner) % SHADES.len()];

    let body_w = area.width.saturating_sub(2) as usize;
    let bar = |cols: usize, color: Color| {
        Span::styled("█".repeat(cols.max(1)), Style::default().fg(color))
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!(" {} ", spinner(app)), Style::default().fg(ORANGE)),
            Span::styled(
                "loading discussion…",
                Style::default().fg(DIM).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];

    // (indent depth, first text-line width fraction, second fraction (0 = none)).
    let rows: [(usize, f32, f32); 8] = [
        (0, 0.72, 0.42),
        (1, 0.60, 0.30),
        (1, 0.50, 0.0),
        (0, 0.78, 0.50),
        (1, 0.58, 0.26),
        (2, 0.46, 0.34),
        (0, 0.70, 0.44),
        (1, 0.55, 0.0),
    ];

    for (i, (depth, f1, f2)) in rows.iter().enumerate() {
        if lines.len() as u16 + 1 >= area.height {
            break;
        }
        let indent = "│ ".repeat(*depth);
        let avail = body_w.saturating_sub(depth * 2);
        let indent_span = || Span::styled(indent.clone(), Style::default().fg(FAINT));

        // author + timestamp bars
        lines.push(Line::from(vec![
            indent_span(),
            bar(10, shade(i + 1)),
            Span::raw("  "),
            bar(5, shade(i)),
        ]));
        lines.push(Line::from(vec![
            indent_span(),
            bar((avail as f32 * f1) as usize, shade(i)),
        ]));
        if *f2 > 0.0 {
            lines.push(Line::from(vec![
                indent_span(),
                bar((avail as f32 * f2) as usize, shade(i + 2)),
            ]));
        }
        lines.push(Line::from(""));
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1))),
        area,
    );
}

fn thread_color(depth: usize) -> Color {
    const COLORS: [Color; 5] = [
        Color::Rgb(255, 140, 60),
        Color::Rgb(120, 170, 255),
        Color::Rgb(120, 200, 140),
        Color::Rgb(200, 140, 220),
        Color::Rgb(220, 200, 110),
    ];
    if depth == 0 {
        FAINT
    } else {
        COLORS[(depth - 1) % COLORS.len()]
    }
}

// ── footer ───────────────────────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    // An open prompt owns the footer, ahead of any toast.
    if let Some(prompt) = &app.prompt {
        let (sigil, hint) = match prompt.kind {
            PromptKind::Jump => (":", "story number · enter jump · esc cancel"),
            PromptKind::Search => ("/", "enter search · n/N next/prev · esc cancel"),
        };
        let text = format!(" {sigil}{}", prompt.input);
        let cursor_x = area.x + text.chars().count() as u16;
        let line = Line::from(vec![
            Span::styled(
                text,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   {hint}"), Style::default().fg(FAINT)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        frame.set_cursor_position((cursor_x.min(area.right().saturating_sub(1)), area.y));
        return;
    }

    if let Some((msg, _)) = &app.toast {
        let line = Line::from(Span::styled(
            format!(" ✓ {msg} "),
            Style::default().fg(Color::Black).bg(Color::Green),
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // Most important first: when the terminal is too narrow for all of them,
    // hints are dropped from the end, but `? help` and `q quit` always stay.
    let hints: &[(&str, &str)] = match app.view {
        View::List => &[
            ("j/k", "move"),
            ("enter", "comments"),
            ("o", "open"),
            ("/", "search"),
            ("tab", "feed"),
            ("s", "save"),
            ("b", "saved"),
            (":", "jump"),
            ("O", "discussion"),
            (",", "settings"),
        ],
        View::Comments => &[
            ("j/k", "move"),
            ("space", "collapse"),
            ("esc", "back"),
            ("o", "article"),
            ("u", "links"),
            ("/", "search"),
            ("s", "save"),
            ("O", "discussion"),
        ],
        View::Bookmarks => &[
            ("j/k", "move"),
            ("enter", "comments"),
            ("o", "open"),
            ("b/esc", "back"),
            ("s", "unsave"),
            ("/", "search"),
        ],
    };
    let hints = fit_hints(hints, &[("?", "help"), ("q", "quit")], area.width);

    let mut spans = vec![Span::raw(" ")];
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(FAINT)));
        }
        spans.push(Span::styled(
            *key,
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {desc}"), Style::default().fg(DIM)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Display width of a hint row laid out as ` key desc  key desc…`.
fn hints_width(hints: &[(&str, &str)]) -> usize {
    let items: usize = hints
        .iter()
        .map(|(k, d)| k.chars().count() + 1 + d.chars().count())
        .sum();
    1 + items + 2 * hints.len().saturating_sub(1)
}

/// The longest prefix of `optional` that fits in `width` alongside `always`.
fn fit_hints<'a>(
    optional: &[(&'a str, &'a str)],
    always: &[(&'a str, &'a str)],
    width: u16,
) -> Vec<(&'a str, &'a str)> {
    let mut kept: Vec<_> = optional.iter().chain(always).copied().collect();
    let mut n = optional.len();
    while n > 0 && hints_width(&kept) > width as usize {
        n -= 1;
        kept.remove(n);
    }
    kept
}

// ── overlays ─────────────────────────────────────────────────────────────────

fn draw_help(frame: &mut Frame, area: Rect) {
    let popup = centered(58, 22, area);
    frame.render_widget(Clear, popup);

    let key = Style::default().fg(ORANGE).add_modifier(Modifier::BOLD);
    let txt = Style::default().fg(Color::White);
    let head = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);

    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<14}"), key),
            Span::styled(d.to_string(), txt),
        ])
    };

    let lines = vec![
        Line::from(Span::styled("  Stories", head)),
        row("j / k  ↑ ↓", "move selection"),
        row("g / G", "jump to top / bottom"),
        row(":10", "jump to story 10"),
        row("/ · n / N", "search titles · next / previous"),
        row("enter", "open comments"),
        row("o / O", "open article / HN discussion"),
        row("s / b", "save / view bookmarks"),
        row("1–6 / tab", "switch feed"),
        row("r  /  ,", "refresh / settings"),
        Line::from(""),
        Line::from(Span::styled("  Comments", head)),
        row("space / enter", "collapse / expand"),
        row("/ · n / N", "search comments · next / previous"),
        row("o / O", "open article / HN discussion"),
        row("u", "open links in the comment"),
        row("s", "save / unsave the story"),
        row("esc / h", "back"),
        Line::from(""),
        Line::from(Span::styled("  q  quit      ?  close this help", DIM_STYLE)),
    ];

    let block = Block::default()
        .title(Span::styled(
            " keyboard shortcuts ",
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ORANGE))
        .style(Style::default().bg(BG));

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

const DIM_STYLE: Style = Style::new().fg(DIM);

fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
    let popup = centered(54, 11, area);
    frame.render_widget(Clear, popup);

    let toggles: [(&str, bool); SETTINGS_COUNT] = [
        ("Remember read stories", app.settings.remember_read),
        ("Remember bookmarks", app.settings.remember_bookmarks),
    ];

    let mut lines = vec![Line::from("")];
    for (i, (label, on)) in toggles.iter().enumerate() {
        let selected = i == app.settings_index;
        let marker = if selected { "›" } else { " " };
        let checkbox = if *on { "[✓]" } else { "[ ]" };
        let label_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(READ)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {marker} "),
                Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                checkbox,
                Style::default().fg(if *on { Color::Green } else { DIM }),
            ),
            Span::raw("  "),
            Span::styled(label.to_string(), label_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  data is written to disk only while enabled",
        DIM_STYLE,
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  j/k move   space toggle   ,/esc close",
        DIM_STYLE,
    )));

    let block = Block::default()
        .title(Span::styled(
            " settings ",
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ORANGE))
        .style(Style::default().bg(BG));

    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Render a (possibly multi-line) message centered both ways in `area`.
fn draw_center(frame: &mut Frame, area: Rect, msg: &str, color: Color) {
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    // Wrap up front so the block's height is known and it can be centered
    // vertically, rather than squeezing the paragraph into a fixed-size slot.
    let lines: Vec<Line> = util::wrap(msg, inner.width as usize)
        .into_iter()
        .map(Line::from)
        .collect();
    let height = (lines.len() as u16).min(inner.height);
    let slot = Rect {
        y: inner.y + (inner.height - height) / 2,
        height,
        ..inner
    };
    let para = Paragraph::new(lines)
        .style(Style::default().fg(color))
        .alignment(Alignment::Center);
    frame.render_widget(para, slot);
}

fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Split `text` into spans in `base` style, with case-insensitive matches of
/// `query` picked out so search hits stand out.
fn highlighted(text: &str, query: Option<&str>, base: Style) -> Vec<Span<'static>> {
    let ranges = query.map(|q| util::find_ci(text, q)).unwrap_or_default();
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    // Underlined as well as coloured, so hits still show without colour (where
    // the selected row and the hit are both drawn in reverse video).
    let hit = base
        .fg(Color::Black)
        .bg(ORANGE)
        .add_modifier(Modifier::UNDERLINED);
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut at = 0;
    for r in ranges {
        if r.start > at {
            spans.push(Span::styled(text[at..r.start].to_string(), base));
        }
        spans.push(Span::styled(text[r.clone()].to_string(), hit));
        at = r.end;
    }
    if at < text.len() {
        spans.push(Span::styled(text[at..].to_string(), base));
    }
    spans
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(w: u16, h: u16, msg: &str) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| draw_center(f, f.area(), msg, Color::Red))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn centered_message_shows_at_even_and_odd_heights() {
        for h in [10, 11] {
            let rows = render(40, h, "first line\nsecond line");
            let first = rows.iter().position(|r| r.contains("first line"));
            let second = rows.iter().position(|r| r.contains("second line"));
            assert!(first.is_some(), "height {h}: first line missing");
            assert_eq!(second, first.map(|y| y + 1), "height {h}");
            // Roughly vertically centered.
            assert!((h as usize / 2).abs_diff(first.unwrap()) <= 1);
        }
    }

    #[test]
    fn fit_hints_drops_optional_from_the_end_but_keeps_always() {
        let optional = [("j/k", "move"), ("enter", "comments"), ("o", "open")];
        let always = [("?", "help"), ("q", "quit")];
        let all = fit_hints(&optional, &always, 200);
        assert_eq!(all.len(), 5);
        assert_eq!(
            hints_width(&all),
            " j/k move  enter comments  o open  ? help  q quit".len()
        );

        let narrow = fit_hints(&optional, &always, 30);
        assert_eq!(narrow, [("j/k", "move"), ("?", "help"), ("q", "quit")]);
        assert!(hints_width(&narrow) <= 30);

        let tiny = fit_hints(&optional, &always, 5);
        assert_eq!(tiny, always); // never drops the essentials
    }

    #[test]
    fn title_and_domain_share_the_row() {
        let dom = || Some("example.com".to_string()); // suffix is 15 wide
        // Plenty of room: both shown, title not truncated.
        assert_eq!(fit_title(20, dom(), 80), (65, dom()));
        // Long title: truncated to leave room for the domain.
        assert_eq!(fit_title(100, dom(), 60), (45, dom()));
        // Too tight to keep a readable title alongside the domain: drop it.
        assert_eq!(fit_title(100, dom(), 30), (30, None));
        assert_eq!(fit_title(100, None, 30), (30, None));
    }

    #[test]
    fn row_number_column_grows_with_the_list() {
        assert_eq!(RowCols::new(9, 80).num, 2);
        assert_eq!(RowCols::new(99, 80).num, 2);
        assert_eq!(RowCols::new(120, 80).num, 3);
    }
}
