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
use crate::app::{App, Load, SETTINGS_COUNT, View};
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

pub fn draw(frame: &mut Frame, app: &mut App) {
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

    let line = Line::from(spans);

    // Right-aligned spinner / status.
    let right = if app.is_loading() {
        Line::from(vec![
            Span::styled(spinner(app), Style::default().fg(ORANGE)),
            Span::styled(" loading ", Style::default().fg(DIM)),
        ])
    } else {
        Line::from(Span::styled("● live ", Style::default().fg(Color::Green)))
    };

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
                &format!("couldn't load stories\n{e}"),
                Color::Red,
            );
        }
        Load::Ready(s) if s.is_empty() => return draw_center(frame, area, "no stories here", DIM),
        Load::Ready(s) => s,
    };

    let width = area.width.saturating_sub(6) as usize;
    let items: Vec<ListItem> = stories
        .iter()
        .enumerate()
        .map(|(i, story)| {
            story_row(
                i,
                story,
                app.visited.contains(&story.id),
                app.is_saved(story.id),
                width,
            )
        })
        .collect();

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

    let width = area.width.saturating_sub(6) as usize;
    let items: Vec<ListItem> = app
        .saved
        .iter()
        .enumerate()
        .map(|(i, story)| story_row(i, story, app.visited.contains(&story.id), true, width))
        .collect();

    let list = story_list(items);
    frame.render_stateful_widget(list, area, &mut app.bookmark_state);
}

/// A single two-line story row, shared by the feed list and the bookmarks view.
fn story_row(i: usize, story: &Item, read: bool, saved: bool, width: usize) -> ListItem<'static> {
    let title_style = if read {
        Style::default().fg(READ)
    } else {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };

    let mut title_spans = vec![Span::styled(
        format!("{:>2}. ", i + 1),
        Style::default().fg(ORANGE),
    )];
    if saved {
        title_spans.push(Span::styled("★ ", Style::default().fg(ORANGE)));
    }
    title_spans.push(Span::styled(truncate(&story.title, width), title_style));
    if let Some(dom) = story.url.as_deref().and_then(util::domain) {
        title_spans.push(Span::styled(
            format!("  ({dom})"),
            Style::default().fg(FAINT),
        ));
    }

    let meta = Line::from(vec![
        Span::raw("    "),
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

fn story_list(items: Vec<ListItem<'static>>) -> List<'static> {
    List::new(items)
        .highlight_style(Style::default().bg(SELECT_BG))
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
        Load::Loading => {
            return draw_center(
                frame,
                body,
                &format!("{} loading discussion…", spinner(app)),
                ORANGE,
            );
        }
        Load::Failed(e) => return draw_center(frame, body, e, Color::Red),
        Load::Ready(c) if c.is_empty() => {
            return draw_center(frame, body, "no comments yet — be the first on HN", DIM);
        }
        Load::Ready(_) => {}
    }

    let op = app.story.as_ref().map(|s| s.by.clone()).unwrap_or_default();
    let text_width = body.width.saturating_sub(1) as usize;

    let visible = app.visible_comments();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|flat| {
            let c = flat.comment;
            let indent = "│ ".repeat(flat.depth);
            let bar = Style::default().fg(thread_color(flat.depth));

            let marker = if flat.has_children {
                if flat.collapsed { "▸ " } else { "▾ " }
            } else {
                "• "
            };
            let is_op = c.by == op && !op.is_empty();

            let mut head = vec![
                Span::styled(indent.clone(), bar),
                Span::styled(
                    marker,
                    Style::default().fg(if flat.collapsed { ORANGE } else { DIM }),
                ),
                Span::styled(
                    c.by.clone(),
                    Style::default()
                        .fg(if is_op { ORANGE } else { ACCENT })
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            if is_op {
                head.push(Span::styled(" OP", Style::default().fg(ORANGE)));
            }
            head.push(Span::styled(
                format!("  {}", util::time_ago(c.time)),
                Style::default().fg(FAINT),
            ));
            if flat.collapsed {
                head.push(Span::styled(
                    format!("  [+{} hidden]", c.descendant_count()),
                    Style::default().fg(DIM),
                ));
            }

            let mut lines = vec![Line::from(head)];
            if !flat.collapsed {
                let body_width = text_width.saturating_sub(flat.depth * 2);
                for wl in util::wrap(&c.text, body_width) {
                    lines.push(Line::from(vec![
                        Span::styled(indent.clone(), bar),
                        Span::raw(wl),
                    ]));
                }
            }
            lines.push(Line::from(""));
            ListItem::new(Text::from(lines))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(SELECT_BG))
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
    if let Some((msg, _)) = &app.toast {
        let line = Line::from(Span::styled(
            format!(" ✓ {msg} "),
            Style::default().fg(Color::Black).bg(Color::Green),
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let hints: &[(&str, &str)] = match app.view {
        View::List => &[
            ("j/k", "move"),
            ("enter", "comments"),
            ("o", "open"),
            ("s", "save"),
            ("b", "saved"),
            ("tab", "feed"),
            (",", "settings"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::Comments => &[
            ("j/k", "move"),
            ("space", "collapse"),
            ("o", "article"),
            ("s", "save"),
            ("esc", "back"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::Bookmarks => &[
            ("j/k", "move"),
            ("enter", "comments"),
            ("o", "open"),
            ("s", "unsave"),
            ("b/esc", "back"),
            ("?", "help"),
            ("q", "quit"),
        ],
    };

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

// ── overlays ─────────────────────────────────────────────────────────────────

fn draw_help(frame: &mut Frame, area: Rect) {
    let popup = centered(58, 19, area);
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
        row("enter", "open comments"),
        row("o", "open article in browser"),
        row("s / b", "save / view bookmarks"),
        row("1–6 / tab", "switch feed"),
        row("r  /  ,", "refresh / settings"),
        Line::from(""),
        Line::from(Span::styled("  Comments", head)),
        row("space / enter", "collapse / expand"),
        row("o  /  s", "open article / save"),
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

fn draw_center(frame: &mut Frame, area: Rect, msg: &str, color: Color) {
    let para = Paragraph::new(msg.to_string())
        .style(Style::default().fg(color))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: area.height / 2,
    });
    frame.render_widget(para, inner);
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}
