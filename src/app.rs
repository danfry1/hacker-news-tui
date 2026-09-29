//! Application state, input handling, and async orchestration.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{self, Client, Comment, Feed, Item};
use crate::store::Settings;
use crate::util;

/// Number of toggles shown in the settings pane.
pub const SETTINGS_COUNT: usize = 2;

/// A background unit of work handed to the spawner.
type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
/// How background work is run. Real builds use Tokio; tests use a no-op so the
/// state machine can be exercised deterministically without touching the network.
type Spawner = Box<dyn Fn(Task) + Send>;

/// Stories materialized for the very first paint — about one screenful, kept
/// small so the initial interaction is as snappy as possible.
const FIRST_PAGE: usize = 20;
/// Stories materialized per batch when scrolling further down.
const PAGE: usize = 30;

/// Messages sent from background fetch tasks back to the UI loop.
pub enum Msg {
    Stories {
        seq: u64,
        result: Result<(Vec<u64>, Vec<Item>), String>,
    },
    MoreStories {
        seq: u64,
        items: Vec<Item>,
    },
    Comments {
        seq: u64,
        result: Vec<Comment>,
    },
}

/// Loading lifecycle for an async resource.
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum View {
    List,
    Comments,
    Bookmarks,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum PromptKind {
    /// `:N` — jump to the story numbered N.
    Jump,
    /// `/text` — find rows containing text.
    Search,
}

/// A vim-style command line shown in the footer while the user types.
pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
    /// Selection when the prompt opened, restored if it is cancelled (search
    /// moves the selection live as the query is typed).
    origin: Option<usize>,
}

pub struct App {
    client: Client,
    tx: UnboundedSender<Msg>,
    spawn: Spawner,

    pub view: View,
    pub show_help: bool,
    pub show_settings: bool,
    pub settings_index: usize,
    pub should_quit: bool,
    pub spinner: usize,

    pub feed: Feed,
    pub stories: Load<Vec<Item>>,
    pub list_state: ListState,
    story_ids: Vec<u64>,
    ids_loaded: usize,
    loading_more: bool,
    story_gen: u64,
    /// A `:N` target beyond the stories loaded so far; pages are fetched until
    /// it is reachable, then it is selected.
    pending_jump: Option<usize>,

    pub prompt: Option<Prompt>,
    /// The last submitted search, reused by `n` / `N` and highlighted on screen.
    pub search: Option<String>,

    pub story: Option<Item>,
    pub comments: Load<Vec<Comment>>,
    pub comment_state: ListState,
    pub collapsed: HashSet<u64>,
    /// Flattened, depth-annotated comment rows honoring collapsed subtrees.
    /// Materialized once whenever `comments` or `collapsed` changes, so drawing
    /// and navigation are cheap reads rather than a per-keystroke re-flatten.
    visible: Vec<FlatComment>,
    comment_gen: u64,
    comments_origin: View,

    pub visited: HashSet<u64>,
    pub saved: Vec<Item>,
    pub bookmark_state: ListState,
    pub settings: Settings,
    dirty: bool,
    pub toast: Option<(String, Instant)>,
}

impl App {
    pub fn new(client: Client, tx: UnboundedSender<Msg>) -> Self {
        App::with_spawner(
            client,
            tx,
            Box::new(|task| {
                tokio::spawn(task);
            }),
        )
    }

    /// Construct with an explicit spawner. The default [`App::new`] uses Tokio;
    /// tests inject a no-op spawner to drive the state machine without I/O.
    pub fn with_spawner(client: Client, tx: UnboundedSender<Msg>, spawn: Spawner) -> Self {
        let mut app = App {
            client,
            tx,
            spawn,
            view: View::List,
            show_help: false,
            show_settings: false,
            settings_index: 0,
            should_quit: false,
            spinner: 0,
            feed: Feed::Top,
            stories: Load::Loading,
            list_state: ListState::default(),
            story_ids: Vec::new(),
            ids_loaded: 0,
            loading_more: false,
            story_gen: 0,
            pending_jump: None,
            prompt: None,
            search: None,
            story: None,
            comments: Load::Loading,
            comment_state: ListState::default(),
            collapsed: HashSet::new(),
            visible: Vec::new(),
            comment_gen: 0,
            comments_origin: View::List,
            visited: HashSet::new(),
            saved: Vec::new(),
            bookmark_state: ListState::default(),
            settings: Settings::default(),
            dirty: false,
            toast: None,
        };
        app.load_feed();
        app
    }

    /// Seed the app with previously persisted settings and data.
    pub fn restore(&mut self, settings: Settings, read: HashSet<u64>, saved: Vec<Item>) {
        self.settings = settings;
        self.visited = read;
        self.saved = saved;
        self.clamp_bookmark_selection();
        self.dirty = false;
    }

    /// Whether persistent state has changed since the last save.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_persisted(&mut self) {
        self.dirty = false;
    }

    /// The cached flattened comment rows (see [`App::visible`]).
    pub fn visible_comments(&self) -> &[FlatComment] {
        &self.visible
    }

    /// Recompute the cached flattened view. Call after any change to the comment
    /// tree or the collapsed set.
    fn rebuild_visible(&mut self) {
        let mut out = Vec::new();
        if let Load::Ready(roots) = &self.comments {
            flatten(roots, &self.collapsed, 0, &mut out);
        }
        self.visible = out;
    }

    // ── async loads ────────────────────────────────────────────────────────

    pub fn load_feed(&mut self) {
        self.story_gen += 1;
        let seq = self.story_gen;
        self.stories = Load::Loading;
        self.list_state.select(None);
        self.story_ids.clear();
        self.ids_loaded = 0;
        self.loading_more = false;
        self.pending_jump = None;
        let (client, feed, tx) = (self.client.clone(), self.feed, self.tx.clone());
        (self.spawn)(Box::pin(async move {
            let result = match api::fetch_ids(&client, feed).await {
                Ok(ids) => {
                    let page: Vec<u64> = ids.iter().take(FIRST_PAGE).copied().collect();
                    let items = api::fetch_items(client, page).await;
                    Ok((ids, items))
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(Msg::Stories { seq, result });
        }));
    }

    /// Append the next page of stories once the selection nears the bottom.
    fn load_more(&mut self) {
        if self.loading_more || self.ids_loaded >= self.story_ids.len() {
            return;
        }
        let start = self.ids_loaded;
        // A pending `:N` jump widens the batch so the target arrives in one go.
        let want = self.pending_jump.map_or(0, |target| target + 1);
        let end = (start + PAGE).max(want).min(self.story_ids.len());
        let batch = self.story_ids[start..end].to_vec();
        self.ids_loaded = end; // advance now so we don't double-fetch this page
        self.loading_more = true;

        let seq = self.story_gen;
        let (client, tx) = (self.client.clone(), self.tx.clone());
        (self.spawn)(Box::pin(async move {
            let items = api::fetch_items(client, batch).await;
            let _ = tx.send(Msg::MoreStories { seq, items });
        }));
    }

    fn open_comments(&mut self) {
        let Some(story) = self.active_story() else {
            return;
        };
        self.mark_visited(story.id);
        self.comments_origin = self.view;
        self.comment_gen += 1;
        let seq = self.comment_gen;
        self.comments = Load::Loading;
        self.collapsed.clear();
        self.rebuild_visible();
        self.comment_state.select(Some(0));
        self.view = View::Comments;

        let (client, kids, tx) = (self.client.clone(), story.kids.clone(), self.tx.clone());
        self.story = Some(story);
        (self.spawn)(Box::pin(async move {
            let result = api::fetch_comments(client, kids, 250).await;
            let _ = tx.send(Msg::Comments { seq, result });
        }));
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Stories { seq, result } if seq == self.story_gen => {
                self.stories = match result {
                    Ok((ids, stories)) => {
                        self.ids_loaded = ids.len().min(FIRST_PAGE);
                        self.story_ids = ids;
                        self.list_state.select((!stories.is_empty()).then_some(0));
                        Load::Ready(stories)
                    }
                    Err(e) => Load::Failed(e),
                };
            }
            Msg::MoreStories { seq, mut items } if seq == self.story_gen => {
                self.loading_more = false;
                if let Load::Ready(stories) = &mut self.stories {
                    stories.append(&mut items);
                }
                self.resolve_pending_jump();
            }
            Msg::Comments { seq, result } if seq == self.comment_gen => {
                self.comment_state.select((!result.is_empty()).then_some(0));
                self.comments = Load::Ready(result);
                self.rebuild_visible();
            }
            _ => {} // stale generation — ignore
        }
    }

    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
        if let Some((_, until)) = &self.toast {
            if Instant::now() >= *until {
                self.toast = None;
            }
        }
    }

    // ── input ──────────────────────────────────────────────────────────────

    pub fn on_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.show_settings {
            self.on_key_settings(key);
            return;
        }
        if self.prompt.is_some() {
            self.on_key_prompt(key);
            return;
        }
        match self.view {
            View::List => self.on_key_list(key),
            View::Comments => self.on_key_comments(key),
            View::Bookmarks => self.on_key_bookmarks(key),
        }
    }

    fn on_key_settings(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('q') => self.show_settings = false,
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_index = (self.settings_index + 1) % SETTINGS_COUNT;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_index = (self.settings_index + SETTINGS_COUNT - 1) % SETTINGS_COUNT;
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_setting(),
            _ => {}
        }
    }

    fn toggle_setting(&mut self) {
        match self.settings_index {
            0 => self.settings.remember_read = !self.settings.remember_read,
            1 => self.settings.remember_bookmarks = !self.settings.remember_bookmarks,
            _ => {}
        }
        self.dirty = true;
    }

    fn on_key_list(&mut self, key: KeyEvent) {
        // Any further input supersedes a `:N` jump still waiting on the network,
        // so the selection is never yanked away after the user moves on.
        self.pending_jump = None;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(',') => self.open_settings(),
            KeyCode::Char(':') => self.open_prompt(PromptKind::Jump),
            KeyCode::Char('/') => self.open_prompt(PromptKind::Search),
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_list(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_list(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_list(0),
            KeyCode::Char('G') | KeyCode::End => self.select_list(isize::MAX),
            KeyCode::PageDown => self.move_list(10),
            KeyCode::PageUp => self.move_list(-10),
            KeyCode::Enter => self.open_comments(),
            KeyCode::Char('o') => self.open_active(),
            KeyCode::Char('s') => self.toggle_bookmark(),
            KeyCode::Char('b') => {
                self.view = View::Bookmarks;
                self.clamp_bookmark_selection();
            }
            KeyCode::Char('r') => {
                self.toast("refreshing…");
                self.load_feed();
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.feed = self.feed.next();
                self.load_feed();
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.feed = self.feed.prev();
                self.load_feed();
            }
            KeyCode::Char(c @ '1'..='6') => {
                let idx = c as usize - '1' as usize;
                self.feed = Feed::ALL[idx];
                self.load_feed();
            }
            _ => {}
        }
    }

    fn on_key_comments(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(',') => self.open_settings(),
            KeyCode::Char('/') => self.open_prompt(PromptKind::Search),
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                self.view = self.comments_origin;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_comments(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_comments(-1),
            KeyCode::Char('g') | KeyCode::Home => self.comment_state.select(Some(0)),
            KeyCode::Char('G') | KeyCode::End => {
                let n = self.visible.len();
                self.comment_state.select(n.checked_sub(1));
            }
            KeyCode::PageDown => self.move_comments(10),
            KeyCode::PageUp => self.move_comments(-10),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_collapse(),
            KeyCode::Char('o') => self.open_active(),
            KeyCode::Char('s') => self.toggle_bookmark(),
            _ => {}
        }
    }

    fn on_key_bookmarks(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(',') => self.open_settings(),
            KeyCode::Char(':') => self.open_prompt(PromptKind::Jump),
            KeyCode::Char('/') => self.open_prompt(PromptKind::Search),
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('b') => {
                self.view = View::List;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_bookmarks(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_bookmarks(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_bookmark(0),
            KeyCode::Char('G') | KeyCode::End => self.select_bookmark(isize::MAX),
            KeyCode::PageDown => self.move_bookmarks(10),
            KeyCode::PageUp => self.move_bookmarks(-10),
            KeyCode::Enter => self.open_comments(),
            KeyCode::Char('o') => self.open_active(),
            KeyCode::Char('s') => self.toggle_bookmark(),
            _ => {}
        }
    }

    fn open_settings(&mut self) {
        self.show_settings = true;
        self.settings_index = 0;
    }

    // ── jump & search prompt ────────────────────────────────────────────────

    fn open_prompt(&mut self, kind: PromptKind) {
        self.prompt = Some(Prompt {
            kind,
            input: String::new(),
            origin: self.selected_row(),
        });
    }

    fn on_key_prompt(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.cancel_prompt(),
            KeyCode::Enter => self.submit_prompt(),
            KeyCode::Backspace => {
                // Backspacing past the start closes the prompt, as in vim.
                if prompt.input.pop().is_none() {
                    self.cancel_prompt();
                } else {
                    self.preview_search();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if prompt.kind == PromptKind::Jump && !c.is_ascii_digit() {
                    return;
                }
                prompt.input.push(c);
                self.preview_search();
            }
            _ => {}
        }
    }

    fn cancel_prompt(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            if let Some(origin) = prompt.origin {
                self.select_row(origin);
            }
        }
    }

    fn submit_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt.kind {
            PromptKind::Jump => {
                if let Ok(n) = prompt.input.parse::<usize>() {
                    self.jump_to(n);
                }
            }
            PromptKind::Search if prompt.input.is_empty() => {
                // An empty search clears the active query and its highlighting.
                self.search = None;
            }
            PromptKind::Search => {
                if self.find_row(&prompt.input, 0, true).is_none() {
                    self.toast(format!("no matches for “{}”", prompt.input));
                }
                self.search = Some(prompt.input);
            }
        }
    }

    /// Incremental search: move to the first match at or after where the prompt
    /// was opened, or back to that origin when nothing (or no query) matches.
    fn preview_search(&mut self) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        if prompt.kind != PromptKind::Search {
            return;
        }
        let origin = prompt.origin.unwrap_or(0);
        let target = self.find_row(&prompt.input, origin, true).or(prompt.origin);
        if let Some(idx) = target {
            self.select_row(idx);
        }
    }

    /// `n` / `N`: move to the next / previous row matching the last search,
    /// wrapping around the ends.
    fn search_step(&mut self, forward: bool) {
        let Some(query) = self.search.clone() else {
            self.toast("no search yet — press / to search");
            return;
        };
        let len = self.row_count();
        if len == 0 {
            return;
        }
        let cur = self.selected_row().unwrap_or(0);
        let start = if forward {
            (cur + 1) % len
        } else {
            (cur + len - 1) % len
        };
        match self.find_row(&query, start, forward) {
            Some(idx) => {
                if (forward && idx <= cur) || (!forward && idx >= cur) {
                    self.toast(if forward {
                        "search wrapped to top"
                    } else {
                        "search wrapped to bottom"
                    });
                }
                self.select_row(idx);
            }
            None => self.toast(format!("no matches for “{query}”")),
        }
    }

    /// The first row matching `needle`, scanning from `start` in the given
    /// direction and wrapping around.
    fn find_row(&self, needle: &str, start: usize, forward: bool) -> Option<usize> {
        let len = self.row_count();
        if len == 0 || needle.is_empty() {
            return None;
        }
        let start = start.min(len - 1);
        (0..len)
            .map(|k| {
                if forward {
                    (start + k) % len
                } else {
                    (start + len - k) % len
                }
            })
            .find(|&i| self.row_matches(i, needle))
    }

    fn row_matches(&self, i: usize, needle: &str) -> bool {
        match self.view {
            View::List => match &self.stories {
                Load::Ready(s) => s.get(i).is_some_and(|it| story_matches(it, needle)),
                _ => false,
            },
            View::Bookmarks => self
                .saved
                .get(i)
                .is_some_and(|it| story_matches(it, needle)),
            View::Comments => self.visible.get(i).is_some_and(|c| {
                util::contains_ci(&c.text, needle) || util::contains_ci(&c.by, needle)
            }),
        }
    }

    /// The query to highlight on screen: the one being typed, else the last one.
    pub fn highlight_query(&self) -> Option<&str> {
        match &self.prompt {
            Some(p) if p.kind == PromptKind::Search => Some(p.input.as_str()),
            _ => self.search.as_deref(),
        }
        .filter(|q| !q.is_empty())
    }

    /// `:N` — select the story numbered `n` (1-based, as displayed). In the feed,
    /// a number past the loaded stories fetches ahead until it can be reached.
    fn jump_to(&mut self, n: usize) {
        let idx = n.saturating_sub(1);
        match self.view {
            View::Bookmarks => self.select_bookmark(idx as isize),
            View::List => {
                let more = self.ids_loaded < self.story_ids.len();
                if idx >= self.story_len() && more && matches!(self.stories, Load::Ready(_)) {
                    self.pending_jump = Some(idx);
                    self.toast(format!("loading stories up to #{n}…"));
                    // Parks on the last loaded story and, being near the end,
                    // starts a fetch widened to reach the target.
                    self.select_list(isize::MAX);
                } else {
                    self.select_list(idx as isize);
                }
            }
            View::Comments => {}
        }
    }

    /// Select a pending `:N` target once enough stories have arrived, or keep
    /// fetching. Deleted/dead stories are filtered out of a page, so a batch can
    /// fall short of the target and need another round.
    fn resolve_pending_jump(&mut self) {
        let Some(target) = self.pending_jump else {
            return;
        };
        if target < self.story_len() || self.ids_loaded >= self.story_ids.len() {
            self.pending_jump = None;
            self.select_list(target as isize);
        } else {
            self.load_more();
        }
    }

    // ── selection helpers ───────────────────────────────────────────────────

    fn selected_story(&self) -> Option<&Item> {
        match &self.stories {
            Load::Ready(s) => self.list_state.selected().and_then(|i| s.get(i)),
            _ => None,
        }
    }

    fn story_len(&self) -> usize {
        match &self.stories {
            Load::Ready(s) => s.len(),
            _ => 0,
        }
    }

    /// Selection in whichever list the current view shows.
    fn selected_row(&self) -> Option<usize> {
        match self.view {
            View::List => self.list_state.selected(),
            View::Comments => self.comment_state.selected(),
            View::Bookmarks => self.bookmark_state.selected(),
        }
    }

    fn row_count(&self) -> usize {
        match self.view {
            View::List => self.story_len(),
            View::Comments => self.visible.len(),
            View::Bookmarks => self.saved.len(),
        }
    }

    fn select_row(&mut self, idx: usize) {
        match self.view {
            View::List => self.select_list(idx as isize),
            View::Comments => {
                let n = self.visible.len();
                self.comment_state.select((n > 0).then(|| idx.min(n - 1)));
            }
            View::Bookmarks => self.select_bookmark(idx as isize),
        }
    }

    fn move_list(&mut self, delta: isize) {
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        self.select_list(cur + delta);
    }

    fn select_list(&mut self, idx: isize) {
        let len = self.story_len();
        if len == 0 {
            return;
        }
        let clamped = idx.clamp(0, len as isize - 1) as usize;
        self.list_state.select(Some(clamped));
        // Prefetch the next page as the selection approaches the end.
        if clamped + 3 >= len {
            self.load_more();
        }
    }

    fn move_comments(&mut self, delta: isize) {
        let len = self.visible.len();
        if len == 0 {
            return;
        }
        let cur = self.comment_state.selected().unwrap_or(0) as isize;
        let clamped = (cur + delta).clamp(0, len as isize - 1) as usize;
        self.comment_state.select(Some(clamped));
    }

    fn toggle_collapse(&mut self) {
        let Some(sel) = self.comment_state.selected() else {
            return;
        };
        let Some(flat) = self.visible.get(sel) else {
            return;
        };
        if !flat.has_children {
            return;
        }
        let id = flat.id;
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        // Collapsing only hides rows below the selected node, so the selection
        // stays in range; just refresh the materialized view.
        self.rebuild_visible();
    }

    // ── bookmarks ─────────────────────────────────────────────────────────────

    /// The story relevant to the current view: the selected list/bookmark row,
    /// or the story whose comments are open.
    fn active_story(&self) -> Option<Item> {
        match self.view {
            View::List => self.selected_story().cloned(),
            View::Bookmarks => self.selected_bookmark().cloned(),
            View::Comments => self.story.clone(),
        }
    }

    fn selected_bookmark(&self) -> Option<&Item> {
        self.bookmark_state
            .selected()
            .and_then(|i| self.saved.get(i))
    }

    pub fn is_saved(&self, id: u64) -> bool {
        self.saved.iter().any(|s| s.id == id)
    }

    fn toggle_bookmark(&mut self) {
        let Some(story) = self.active_story() else {
            return;
        };
        if let Some(pos) = self.saved.iter().position(|s| s.id == story.id) {
            self.saved.remove(pos);
            self.toast("removed bookmark");
        } else {
            self.saved.insert(0, story);
            self.toast("bookmarked ★");
        }
        self.dirty = true;
        self.clamp_bookmark_selection();
    }

    fn move_bookmarks(&mut self, delta: isize) {
        let cur = self.bookmark_state.selected().unwrap_or(0) as isize;
        self.select_bookmark(cur + delta);
    }

    fn select_bookmark(&mut self, idx: isize) {
        let len = self.saved.len();
        if len == 0 {
            return;
        }
        let clamped = idx.clamp(0, len as isize - 1) as usize;
        self.bookmark_state.select(Some(clamped));
    }

    fn clamp_bookmark_selection(&mut self) {
        if self.saved.is_empty() {
            self.bookmark_state.select(None);
        } else {
            let last = self.saved.len() - 1;
            let cur = self.bookmark_state.selected().unwrap_or(0).min(last);
            self.bookmark_state.select(Some(cur));
        }
    }

    // ── browser ──────────────────────────────────────────────────────────────

    fn mark_visited(&mut self, id: u64) {
        if self.visited.insert(id) {
            self.dirty = true;
        }
    }

    fn open_active(&mut self) {
        if let Some(story) = self.active_story() {
            self.mark_visited(story.id);
            self.open(&story.target_url());
        }
    }

    fn open(&mut self, url: &str) {
        let label = util::domain(url).unwrap_or_else(|| "link".to_string());
        if util::open_in_browser(url) {
            self.toast(format!("opened {label} in browser"));
        } else {
            self.toast(format!("couldn't open {label}"));
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now() + Duration::from_secs(2)));
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.stories, Load::Loading)
            || self.loading_more
            || (self.view == View::Comments && matches!(self.comments, Load::Loading))
    }

    /// Whether anything on screen is animating and so needs periodic ticks: the
    /// loading spinner, or a toast counting down to expiry. When false, the event
    /// loop can idle on input alone instead of redrawing several times a second.
    pub fn is_animating(&self) -> bool {
        self.is_loading() || self.toast.is_some()
    }
}

/// Whether a story's title or domain contains `needle` (ASCII case-insensitive).
fn story_matches(story: &Item, needle: &str) -> bool {
    util::contains_ci(&story.title, needle)
        || story
            .url
            .as_deref()
            .and_then(util::domain)
            .is_some_and(|d| util::contains_ci(&d, needle))
}

/// A comment row materialized for display: the depth-annotated, flattened view
/// of the tree with collapsed subtrees omitted. Owns its display data so the
/// cache ([`App::visible`]) is self-contained.
pub struct FlatComment {
    pub id: u64,
    pub by: String,
    pub time: u64,
    pub text: String,
    pub depth: usize,
    pub collapsed: bool,
    pub has_children: bool,
    /// Descendants hidden beneath this node when it is collapsed (for the badge).
    pub hidden: usize,
}

fn flatten(list: &[Comment], collapsed: &HashSet<u64>, depth: usize, out: &mut Vec<FlatComment>) {
    for c in list {
        let is_collapsed = collapsed.contains(&c.id);
        out.push(FlatComment {
            id: c.id,
            by: c.by.clone(),
            time: c.time,
            // A collapsed node's body is not rendered, so don't bother cloning it.
            text: if is_collapsed {
                String::new()
            } else {
                c.text.clone()
            },
            depth,
            collapsed: is_collapsed,
            has_children: !c.children.is_empty(),
            hidden: if is_collapsed {
                c.descendant_count()
            } else {
                0
            },
        });
        if !is_collapsed {
            flatten(&c.children, collapsed, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Item;
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    /// Build an App whose background tasks are dropped (no I/O), so the state
    /// machine can be driven deterministically. The receiver is returned to keep
    /// the channel open.
    fn app() -> (App, UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = App::with_spawner(reqwest::Client::new(), tx, Box::new(|_task| {}));
        (app, rx)
    }

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn items(ids: &[u64]) -> Vec<Item> {
        ids.iter()
            .map(|&id| Item {
                id,
                title: format!("Story {id}"),
                by: "alice".into(),
                ..Default::default()
            })
            .collect()
    }

    fn leaf(id: u64) -> Comment {
        Comment {
            id,
            by: "bob".into(),
            time: 0,
            text: "text".into(),
            children: vec![],
        }
    }

    /// App with a feed of `n` ids and the first page of items already loaded.
    fn loaded(n: u64) -> (App, UnboundedReceiver<Msg>) {
        let (mut app, rx) = app();
        let ids: Vec<u64> = (1..=n).collect();
        let first = (FIRST_PAGE as u64).min(n) as usize;
        let seq = app.story_gen;
        app.on_msg(Msg::Stories {
            seq,
            result: Ok((ids.clone(), items(&ids[..first]))),
        });
        (app, rx)
    }

    fn selected(app: &App) -> Option<usize> {
        app.list_state.selected()
    }

    // ── loading lifecycle ────────────────────────────────────────────────────

    #[test]
    fn starts_loading_top_feed() {
        let (app, _rx) = app();
        assert_eq!(app.feed, Feed::Top);
        assert!(matches!(app.stories, Load::Loading));
        assert_eq!(app.story_gen, 1); // initial load kicked off
        assert_eq!(selected(&app), None);
    }

    #[test]
    fn stories_ready_selects_first_and_records_ids() {
        let (app, _rx) = loaded(40);
        match &app.stories {
            Load::Ready(s) => assert_eq!(s.len(), FIRST_PAGE),
            _ => panic!("expected Ready"),
        }
        assert_eq!(app.story_ids.len(), 40);
        assert_eq!(app.ids_loaded, FIRST_PAGE);
        assert_eq!(selected(&app), Some(0));
    }

    #[test]
    fn stories_error_sets_failed() {
        let (mut app, _rx) = app();
        let seq = app.story_gen;
        app.on_msg(Msg::Stories {
            seq,
            result: Err("boom".into()),
        });
        assert!(matches!(app.stories, Load::Failed(ref e) if e == "boom"));
    }

    #[test]
    fn stale_stories_message_is_ignored() {
        let (mut app, _rx) = app();
        let stale = app.story_gen; // current generation, about to be superseded
        // Switch feed → story_gen advances, previous messages become stale.
        app.on_key(key(KeyCode::Tab));
        let bumped = app.story_gen;
        assert_ne!(stale, bumped);
        app.on_msg(Msg::Stories {
            seq: stale,
            result: Ok(((1..=5).collect(), items(&[1, 2, 3]))),
        });
        assert!(matches!(app.stories, Load::Loading)); // ignored
    }

    // ── navigation ───────────────────────────────────────────────────────────

    #[test]
    fn down_up_clamp_within_bounds() {
        let (mut app, _rx) = loaded(40); // 20 items loaded
        app.on_key(key(KeyCode::Up)); // already at 0, stays
        assert_eq!(selected(&app), Some(0));
        for _ in 0..5 {
            app.on_key(ch('j'));
        }
        assert_eq!(selected(&app), Some(5));
        app.on_key(ch('k'));
        assert_eq!(selected(&app), Some(4));
    }

    #[test]
    fn g_and_shift_g_jump_to_ends() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('G'));
        assert_eq!(selected(&app), Some(FIRST_PAGE - 1));
        app.on_key(ch('g'));
        assert_eq!(selected(&app), Some(0));
    }

    // ── infinite scroll ──────────────────────────────────────────────────────

    #[test]
    fn nearing_bottom_triggers_load_more() {
        let (mut app, _rx) = loaded(40);
        assert!(!app.loading_more);
        app.on_key(ch('G')); // jump to bottom → within prefetch threshold
        assert!(app.loading_more);
        assert_eq!(app.ids_loaded, 40); // next page of ids reserved
    }

    #[test]
    fn load_more_is_guarded_against_double_fetch() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('G'));
        assert_eq!(app.ids_loaded, 40);
        app.on_key(ch('G')); // still loading → must not advance again
        assert_eq!(app.ids_loaded, 40);
    }

    #[test]
    fn more_stories_appends_and_clears_flag() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('G'));
        let seq = app.story_gen;
        app.on_msg(Msg::MoreStories {
            seq,
            items: items(&(21..=40).collect::<Vec<_>>()),
        });
        assert!(!app.loading_more);
        match &app.stories {
            Load::Ready(s) => assert_eq!(s.len(), 40),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn stale_more_stories_is_ignored() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('G'));
        let stale = app.story_gen;
        app.on_key(key(KeyCode::Tab)); // switch feed → generation advances
        app.on_msg(Msg::MoreStories {
            seq: stale,
            items: items(&[99]),
        });
        // Feed switch reset stories to Loading; stale append must not apply.
        assert!(matches!(app.stories, Load::Loading));
    }

    #[test]
    fn load_more_stops_when_ids_exhausted() {
        let (mut app, _rx) = loaded(15); // fewer than FIRST_PAGE
        app.on_key(ch('G'));
        assert!(!app.loading_more); // nothing more to fetch
        assert_eq!(app.ids_loaded, 15);
    }

    // ── feeds ──────────────────────────────────────────────────────────────────

    #[test]
    fn tab_and_backtab_switch_feeds() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.feed, Feed::New);
        assert!(matches!(app.stories, Load::Loading)); // reloads
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.feed, Feed::Top);
    }

    #[test]
    fn number_keys_select_feed() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('4'));
        assert_eq!(app.feed, Feed::Ask);
        app.on_key(ch('6'));
        assert_eq!(app.feed, Feed::Jobs);
    }

    #[test]
    fn switching_feed_clears_scroll_state() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('G'));
        app.on_key(ch('l')); // next feed
        assert!(app.story_ids.is_empty());
        assert_eq!(app.ids_loaded, 0);
        assert!(!app.loading_more);
    }

    // ── comments ───────────────────────────────────────────────────────────────

    #[test]
    fn enter_opens_comments_and_marks_visited() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('j')); // select story id 2
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Comments);
        assert!(matches!(app.comments, Load::Loading));
        assert_eq!(app.story.as_ref().unwrap().id, 2);
        assert!(app.visited.contains(&2));
    }

    #[test]
    fn comments_ready_populates_and_selects_first() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        app.on_msg(Msg::Comments {
            seq,
            result: vec![leaf(10), leaf(11)],
        });
        assert_eq!(app.visible_comments().len(), 2);
        assert_eq!(app.comment_state.selected(), Some(0));
    }

    #[test]
    fn collapse_hides_descendants_and_toggles_back() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        let tree = vec![
            Comment {
                children: vec![leaf(2), leaf(3)],
                ..leaf(1)
            },
            leaf(4),
        ];
        app.on_msg(Msg::Comments { seq, result: tree });
        assert_eq!(app.visible_comments().len(), 4); // 1,2,3,4

        app.comment_state.select(Some(0)); // node 1 (has children)
        app.on_key(ch(' ')); // collapse
        assert_eq!(app.visible_comments().len(), 2); // 1,4
        app.on_key(key(KeyCode::Enter)); // expand
        assert_eq!(app.visible_comments().len(), 4);
    }

    #[test]
    fn collapse_noop_on_childless_comment() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        app.on_msg(Msg::Comments {
            seq,
            result: vec![leaf(1)],
        });
        app.comment_state.select(Some(0));
        app.on_key(ch(' '));
        assert_eq!(app.visible_comments().len(), 1);
        assert!(app.collapsed.is_empty());
    }

    #[test]
    fn flattened_rows_carry_depth_author_text_and_child_flags() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        let tree = vec![
            Comment {
                children: vec![leaf(2), leaf(3)],
                ..leaf(1)
            },
            leaf(4),
        ];
        app.on_msg(Msg::Comments { seq, result: tree });

        let v = app.visible_comments();
        assert_eq!(v.len(), 4);
        // Root node: depth 0, has children, author/text materialized.
        assert_eq!(v[0].id, 1);
        assert_eq!(v[0].depth, 0);
        assert!(v[0].has_children);
        assert!(!v[0].collapsed);
        assert_eq!(v[0].by, "bob");
        assert_eq!(v[0].text, "text");
        // Reply sits one level deeper and is a leaf.
        assert_eq!(v[1].id, 2);
        assert_eq!(v[1].depth, 1);
        assert!(!v[1].has_children);
        // Sibling of the root is back at depth 0.
        assert_eq!(v[3].id, 4);
        assert_eq!(v[3].depth, 0);
    }

    #[test]
    fn collapsing_marks_row_counts_hidden_and_drops_body() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        let tree = vec![Comment {
            children: vec![leaf(2), leaf(3)],
            ..leaf(1)
        }];
        app.on_msg(Msg::Comments { seq, result: tree });
        app.comment_state.select(Some(0));
        app.on_key(ch(' ')); // collapse node 1

        let v = app.visible_comments();
        assert_eq!(v.len(), 1);
        assert!(v[0].collapsed);
        assert_eq!(v[0].hidden, 2); // both descendants hidden
        assert!(v[0].text.is_empty()); // collapsed body is not materialized
    }

    #[test]
    fn is_animating_tracks_toasts() {
        let (mut app, _rx) = loaded(40);
        assert!(!app.is_animating()); // idle: stories ready, no toast
        app.on_key(ch('s')); // bookmark → sets a toast, no load
        assert!(app.is_animating());
        app.toast = Some(("x".into(), Instant::now() - Duration::from_secs(1)));
        app.tick(); // expires the toast
        assert!(!app.is_animating());
    }

    #[test]
    fn comment_navigation_clamps() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        app.on_msg(Msg::Comments {
            seq,
            result: vec![leaf(1), leaf(2), leaf(3)],
        });
        app.on_key(key(KeyCode::End));
        assert_eq!(app.comment_state.selected(), Some(2));
        app.on_key(ch('j')); // past end, clamps
        assert_eq!(app.comment_state.selected(), Some(2));
    }

    #[test]
    fn esc_leaves_comments_for_list() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Comments);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::List);
        assert!(!app.should_quit);
    }

    // ── help, quit, toasts ─────────────────────────────────────────────────────

    #[test]
    fn help_opens_and_any_key_closes_without_acting() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('?'));
        assert!(app.show_help);
        app.on_key(ch('q')); // consumed by help, must NOT quit
        assert!(!app.show_help);
        assert!(!app.should_quit);
    }

    #[test]
    fn q_quits_in_list_and_comments() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('q'));
        assert!(app.should_quit);

        let (mut app2, _rx2) = loaded(40);
        app2.on_key(key(KeyCode::Enter));
        app2.on_key(ch('q'));
        assert!(app2.should_quit);
    }

    #[test]
    fn esc_quits_from_list() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_always_quits() {
        let (mut app, _rx) = loaded(40);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn refresh_sets_toast_and_reloads() {
        let (mut app, _rx) = loaded(40);
        let before = app.story_gen;
        app.on_key(ch('r'));
        assert!(app.toast.is_some());
        assert_eq!(app.story_gen, before + 1);
        assert!(matches!(app.stories, Load::Loading));
    }

    #[test]
    fn tick_clears_expired_toast() {
        let (mut app, _rx) = loaded(40);
        // Manually install an already-expired toast.
        app.toast = Some(("hi".into(), Instant::now() - Duration::from_secs(1)));
        app.tick();
        assert!(app.toast.is_none());
    }

    #[test]
    fn is_loading_reflects_all_pending_work() {
        let (mut app, _rx) = loaded(40);
        assert!(!app.is_loading());
        app.on_key(ch('G'));
        assert!(app.is_loading()); // loading_more
    }

    // ── bookmarks ──────────────────────────────────────────────────────────────

    #[test]
    fn save_toggles_bookmark_and_marks_dirty() {
        let (mut app, _rx) = loaded(40); // story id 1 selected
        assert!(!app.is_dirty());
        app.on_key(ch('s'));
        assert!(app.is_saved(1));
        assert_eq!(app.saved.len(), 1);
        assert!(app.is_dirty());
        app.mark_persisted();

        app.on_key(ch('s')); // unsave
        assert!(!app.is_saved(1));
        assert!(app.saved.is_empty());
        assert!(app.is_dirty());
    }

    #[test]
    fn newest_bookmark_is_first() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('s')); // save id 1
        app.on_key(ch('j'));
        app.on_key(ch('s')); // save id 2
        assert_eq!(app.saved[0].id, 2);
        assert_eq!(app.saved[1].id, 1);
    }

    #[test]
    fn b_opens_saved_view_and_back_returns() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('s'));
        app.on_key(ch('b'));
        assert_eq!(app.view, View::Bookmarks);
        assert_eq!(app.bookmark_state.selected(), Some(0));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::List);
    }

    #[test]
    fn enter_from_bookmarks_opens_comments_and_returns_to_bookmarks() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('s')); // bookmark id 1
        app.on_key(ch('b'));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Comments);
        assert_eq!(app.story.as_ref().unwrap().id, 1);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Bookmarks); // not List
    }

    #[test]
    fn unsaving_in_bookmarks_keeps_selection_in_range() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('s'));
        app.on_key(ch('j'));
        app.on_key(ch('s')); // two bookmarks
        app.on_key(ch('b'));
        app.on_key(ch('G')); // select last
        app.on_key(ch('s')); // unsave it
        assert_eq!(app.saved.len(), 1);
        assert_eq!(app.bookmark_state.selected(), Some(0));
    }

    // ── settings ───────────────────────────────────────────────────────────────

    #[test]
    fn comma_opens_settings_and_esc_closes() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(','));
        assert!(app.show_settings);
        assert_eq!(app.settings_index, 0);
        app.on_key(key(KeyCode::Esc));
        assert!(!app.show_settings);
    }

    #[test]
    fn settings_toggle_flips_flag_and_persists() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(','));
        assert!(!app.settings.remember_read); // opt-in: off by default
        app.on_key(ch(' ')); // toggle item 0
        assert!(app.settings.remember_read);
        assert!(app.is_dirty());

        app.on_key(ch('j')); // move to item 1
        app.on_key(key(KeyCode::Enter)); // toggle bookmarks on
        assert!(app.settings.remember_bookmarks);
    }

    #[test]
    fn settings_navigation_wraps() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(','));
        app.on_key(ch('k')); // up from 0 wraps to last
        assert_eq!(app.settings_index, SETTINGS_COUNT - 1);
        app.on_key(ch('j')); // wraps back to 0
        assert_eq!(app.settings_index, 0);
    }

    #[test]
    fn settings_overlay_swallows_keys() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(','));
        app.on_key(ch('q')); // closes settings, must not quit
        assert!(!app.show_settings);
        assert!(!app.should_quit);
    }

    // ── jump & search ──────────────────────────────────────────────────────────

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(ch(c));
        }
    }

    /// Loaded feed whose first stories have distinctive titles to search for.
    fn titled(titles: &[&str]) -> (App, UnboundedReceiver<Msg>) {
        let (mut app, rx) = loaded(40);
        if let Load::Ready(stories) = &mut app.stories {
            for (story, title) in stories.iter_mut().zip(titles) {
                story.title = title.to_string();
            }
        }
        (app, rx)
    }

    #[test]
    fn colon_jumps_to_numbered_story() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(':'));
        typed(&mut app, "10");
        assert_eq!(app.prompt.as_ref().unwrap().input, "10");
        app.on_key(key(KeyCode::Enter));
        assert!(app.prompt.is_none());
        assert_eq!(selected(&app), Some(9)); // 1-based as displayed
    }

    #[test]
    fn jump_prompt_ignores_non_digits_and_swallows_keys() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(':'));
        typed(&mut app, "q3j"); // q must not quit, j must not move
        assert!(!app.should_quit);
        assert_eq!(app.prompt.as_ref().unwrap().input, "3");
        assert_eq!(selected(&app), Some(0));
    }

    #[test]
    fn esc_and_backspace_past_start_cancel_prompt() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch(':'));
        typed(&mut app, "5");
        app.on_key(key(KeyCode::Esc));
        assert!(app.prompt.is_none());
        assert!(!app.should_quit); // esc closed the prompt, not the app
        assert_eq!(selected(&app), Some(0));

        app.on_key(ch('/'));
        app.on_key(key(KeyCode::Backspace));
        assert!(app.prompt.is_none());
    }

    #[test]
    fn jump_past_loaded_fetches_ahead_then_selects() {
        let (mut app, _rx) = loaded(100); // 20 of 100 loaded
        app.on_key(ch(':'));
        typed(&mut app, "75");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.pending_jump, Some(74));
        assert_eq!(selected(&app), Some(FIRST_PAGE - 1)); // parked at the bottom
        assert_eq!(app.ids_loaded, 75); // one widened batch, not page by page

        let seq = app.story_gen;
        app.on_msg(Msg::MoreStories {
            seq,
            items: items(&(21..=75).collect::<Vec<_>>()),
        });
        assert_eq!(app.pending_jump, None);
        assert_eq!(selected(&app), Some(74));
    }

    #[test]
    fn pending_jump_keeps_fetching_when_a_batch_falls_short() {
        let (mut app, _rx) = loaded(100);
        app.on_key(ch(':'));
        typed(&mut app, "30");
        app.on_key(key(KeyCode::Enter));
        let seq = app.story_gen;
        // Some stories in the batch were dead and got filtered out.
        app.on_msg(Msg::MoreStories {
            seq,
            items: items(&(21..=25).collect::<Vec<_>>()),
        });
        assert_eq!(app.pending_jump, Some(29));
        assert!(app.loading_more); // next batch requested
    }

    #[test]
    fn keypress_cancels_pending_jump() {
        let (mut app, _rx) = loaded(100);
        app.on_key(ch(':'));
        typed(&mut app, "75");
        app.on_key(key(KeyCode::Enter));
        app.on_key(ch('k')); // user moves on while it loads
        let seq = app.story_gen;
        app.on_msg(Msg::MoreStories {
            seq,
            items: items(&(21..=75).collect::<Vec<_>>()),
        });
        assert_eq!(selected(&app), Some(FIRST_PAGE - 2)); // not yanked to 74
    }

    #[test]
    fn jump_beyond_feed_clamps_to_last() {
        let (mut app, _rx) = loaded(15);
        app.on_key(ch(':'));
        typed(&mut app, "999");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(selected(&app), Some(14));
        assert_eq!(app.pending_jump, None);
    }

    #[test]
    fn search_moves_selection_as_you_type() {
        let (mut app, _rx) = titled(&["Alpha", "Rust 2.0", "Beta", "rustls audit"]);
        app.on_key(ch('/'));
        typed(&mut app, "rust");
        assert_eq!(selected(&app), Some(1));
        assert_eq!(app.highlight_query(), Some("rust"));
        app.on_key(key(KeyCode::Esc)); // cancel restores the origin
        assert_eq!(selected(&app), Some(0));
        assert_eq!(app.highlight_query(), None);
    }

    #[test]
    fn n_and_shift_n_cycle_matches_and_wrap() {
        let (mut app, _rx) = titled(&["Alpha", "Rust 2.0", "Beta", "rustls audit"]);
        app.on_key(ch('/'));
        typed(&mut app, "RUST");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.search.as_deref(), Some("RUST"));
        assert_eq!(selected(&app), Some(1));
        app.on_key(ch('n'));
        assert_eq!(selected(&app), Some(3));
        app.on_key(ch('n')); // wraps around
        assert_eq!(selected(&app), Some(1));
        assert!(app.toast.is_some());
        app.on_key(ch('N')); // backwards wraps to the last match
        assert_eq!(selected(&app), Some(3));
    }

    #[test]
    fn search_matches_domain() {
        let (mut app, _rx) = loaded(40);
        if let Load::Ready(stories) = &mut app.stories {
            stories[5].url = Some("https://github.com/x/y".into());
        }
        app.on_key(ch('/'));
        typed(&mut app, "github");
        assert_eq!(selected(&app), Some(5));
    }

    #[test]
    fn unmatched_search_stays_put_and_says_so() {
        let (mut app, _rx) = titled(&["Alpha", "Beta"]);
        app.on_key(ch('j'));
        app.on_key(ch('/'));
        typed(&mut app, "zzz");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(selected(&app), Some(1));
        assert!(app.toast.is_some());
    }

    #[test]
    fn empty_search_clears_highlight() {
        let (mut app, _rx) = titled(&["Alpha"]);
        app.on_key(ch('/'));
        typed(&mut app, "alp");
        app.on_key(key(KeyCode::Enter));
        assert!(app.search.is_some());
        app.on_key(ch('/'));
        app.on_key(key(KeyCode::Enter));
        assert!(app.search.is_none());
        assert_eq!(app.highlight_query(), None);
    }

    #[test]
    fn n_without_search_hints() {
        let (mut app, _rx) = loaded(40);
        app.on_key(ch('n'));
        assert_eq!(selected(&app), Some(0));
        assert!(app.toast.is_some());
    }

    #[test]
    fn search_in_comments_matches_text_and_author() {
        let (mut app, _rx) = loaded(40);
        app.on_key(key(KeyCode::Enter));
        let seq = app.comment_gen;
        let mut c2 = leaf(2);
        c2.text = "I love the borrow checker".into();
        let mut c3 = leaf(3);
        c3.by = "dang".into();
        app.on_msg(Msg::Comments {
            seq,
            result: vec![leaf(1), c2, c3],
        });
        app.on_key(ch('/'));
        typed(&mut app, "borrow");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.comment_state.selected(), Some(1));
        app.on_key(ch('/'));
        typed(&mut app, "dang");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.comment_state.selected(), Some(2));
        assert_eq!(app.view, View::Comments);
    }

    #[test]
    fn jump_and_search_work_in_bookmarks() {
        let (mut app, _rx) = app();
        let mut saved = items(&[1, 2, 3]);
        saved[2].title = "Show HN: a TUI".into();
        app.restore(Settings::default(), HashSet::new(), saved);
        app.on_key(ch('b'));
        app.on_key(ch(':'));
        typed(&mut app, "2");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.bookmark_state.selected(), Some(1));
        app.on_key(ch('/'));
        typed(&mut app, "show hn");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.bookmark_state.selected(), Some(2));
    }

    // ── persistence ────────────────────────────────────────────────────────────

    #[test]
    fn restore_seeds_state_without_dirtying() {
        let (mut app, _rx) = app();
        let mut read = HashSet::new();
        read.insert(7);
        app.restore(
            Settings {
                remember_read: false,
                remember_bookmarks: true,
            },
            read,
            items(&[100, 101]),
        );
        assert!(app.visited.contains(&7));
        assert_eq!(app.saved.len(), 2);
        assert!(!app.settings.remember_read);
        assert!(!app.is_dirty()); // restoring is not a change to persist
    }

    #[test]
    fn opening_comments_marks_dirty_via_visited() {
        let (mut app, _rx) = loaded(40);
        assert!(!app.is_dirty());
        app.on_key(key(KeyCode::Enter)); // visits story 1
        assert!(app.is_dirty());
    }
}
