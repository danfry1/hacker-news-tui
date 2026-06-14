//! A delightful terminal UI for browsing Hacker News.

mod api;
mod app;
mod store;
mod ui;
mod util;

use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;

use app::App;

const HELP: &str = "\
hacker-news-tui — a delightful terminal UI for browsing Hacker News

USAGE:
    hacker-news-tui

OPTIONS:
    -h, --help       Print this help and exit
    -V, --version    Print version and exit

Once running, press ? for the in-app keyboard shortcuts.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Handle the standard CLI flags before taking over the terminal.
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-V" | "--version" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            _ => {} // unknown args are ignored; the TUI takes over
        }
    }

    let client = reqwest::Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout(Duration::from_secs(15))
        .build()?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(client, tx);

    // Restore persisted settings, read-state, and bookmarks.
    let persisted = store::load();
    app.restore(
        persisted.settings,
        persisted.read.into_iter().collect(),
        persisted.saved,
    );

    let mut terminal = ratatui::init();
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(120));

    let result = loop {
        if let Err(e) = terminal.draw(|frame| ui::draw(frame, &mut app)) {
            break Err(e.into());
        }

        tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                Some(Ok(_)) => {}            // resize, mouse, focus — redraw on next loop
                Some(Err(e)) => break Err(e.into()),
                None => break Ok(()),        // input stream closed
            },
            Some(msg) = rx.recv() => app.on_msg(msg),
            _ = ticker.tick() => app.tick(),
        }

        // Flush persistent state whenever it changes (deliberate actions only).
        if app.is_dirty() {
            store::save(&app.settings, &app.visited, &app.saved);
            app.mark_persisted();
        }

        if app.should_quit {
            break Ok(());
        }
    };

    ratatui::restore();
    result
}
