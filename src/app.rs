//! Application state, input handling, and async orchestration.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{self, Client, Comment, Feed, Item};
use crate::store::Settings;
use crate::util;

/// Number of toggles shown in the settings pane.
pub const SETTINGS_COUNT: usize = 3;

/// A background unit of work handed to the spawner.
type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
/// How background work is run. Real builds use Tokio; tests use a no-op so the
/// state machine can be exercised deterministically without touching the network.
type Spawner = Box<dyn Fn(Task) + Send>;
/// How URLs are opened; returns whether the browser launched. Tests swap in a
/// recorder so `o` / `O` / `u` can be exercised without launching anything.
type Opener = Box<dyn Fn(&str) -> bool + Send>;

/// Stories materialized for the very first paint — about one screenful, kept
/// small so the initial interaction is as snappy as possible.
const FIRST_PAGE: usize = 20;
/// Stories materialized per batch when scrolling further down.
const PAGE: usize = 30;
/// Most comments fetched for one discussion, so huge threads stay responsive.
const COMMENT_LIMIT: usize = 250;

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
        /// Whether [`COMMENT_LIMIT`] (or the depth bound) left comments out.
        truncated: bool,
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
    opener: Opener,

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
    /// The story selected when the feed was refreshed, to select again once
    /// the new ranking arrives (it has usually moved).
    reselect: Option<u64>,

    pub prompt: Option<Prompt>,
    /// The last submitted search, reused by `n` / `N` and highlighted on screen.
    pub search: Option<String>,

    pub story: Option<Item>,
    pub comments: Load<Vec<Comment>>,
    pub comment_state: ListState,
    /// Comments loaded for the open discussion, and whether that is less than
    /// the whole thread (see [`COMMENT_LIMIT`]).
    pub comments_loaded: usize,
    pub comments_truncated: bool,
    /// The comment whose links `u` is stepping through, and the next index.
    link_cursor: Option<(u64, usize)>,
    pub collapsed: HashSet<u64>,
    /// Flattened, depth-annotated comment rows honoring collapsed subtrees.
    /// Materialized once whenever `comments` or `collapsed` changes, so drawing
    /// and navigation are cheap reads rather than a per-keystroke re-flatten.
    visible: Vec<FlatComment>,
    comment_gen: u64,
    comments_origin: View,

    /// Where the story or bookmark list was last drawn, for mapping clicks to
    /// rows. Set by the renderer.
    pub list_area: Rect,

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
            opener: Box::new(util::open_in_browser),
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
            reselect: None,
            prompt: None,
            search: None,
            story: None,
            comments: Load::Loading,
            comment_state: ListState::default(),
            comments_loaded: 0,
            comments_truncated: false,
            link_cursor: None,
            collapsed: HashSet::new(),
            visible: Vec::new(),
            comment_gen: 0,
            comments_origin: View::List,
            list_area: Rect::default(),
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
        self.reselect = None;
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
        self.comments_loaded = 0;
        self.comments_truncated = false;
        self.link_cursor = None;
        self.collapsed.clear();
        self.rebuild_visible();
        self.comment_state.select(Some(0));
        self.view = View::Comments;

        let (client, kids, tx) = (self.client.clone(), story.kids.clone(), self.tx.clone());
        self.story = Some(story);
        (self.spawn)(Box::pin(async move {
            let (result, truncated) = api::fetch_comments(client, kids, COMMENT_LIMIT).await;
            let _ = tx.send(Msg::Comments {
                seq,
                result,
                truncated,
            });
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
                self.restore_selection();
            }
            Msg::MoreStories { seq, mut items } if seq == self.story_gen => {
                self.loading_more = false;
                if let Load::Ready(stories) = &mut self.stories {
                    stories.append(&mut items);
                }
                self.resolve_pending_jump();
            }
            Msg::Comments {
                seq,
                result,
                truncated,
            } if seq == self.comment_gen => {
                self.comment_state.select((!result.is_empty()).then_some(0));
                self.comments_loaded = result.iter().map(|c| 1 + c.descendant_count()).sum();
                self.comments_truncated = truncated;
                self.comments = Load::Ready(result);
                self.rebuild_visible();
            }
            _ => {} // stale generation — ignore
        }
    }

    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
        if let Some((_, until)) = &self.toast
            && Instant::now() >= *until
        {
            self.toast = None;
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
        // Any further input supersedes a `:N` jump (or a refresh reselect) still
        // waiting on the network, so the selection is never yanked away after
        // the user moves on.
        self.pending_jump = None;
        self.reselect = None;
        if self.on_key_shared(key) {
            return;
        }
        match self.view {
            View::List => self.on_key_list(key),
            View::Comments => self.on_key_comments(key),
            View::Bookmarks => self.on_key_bookmarks(key),
        }
    }

    /// Mouse input (only delivered while the mouse setting is on): the wheel
    /// moves the selection; clicking a story selects it, and clicking the
    /// selected story opens its comments.
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        if !self.settings.mouse || self.show_help || self.show_settings || self.prompt.is_some() {
            return;
        }
        let step: isize = match ev.kind {
            MouseEventKind::ScrollDown => 1,
            MouseEventKind::ScrollUp => -1,
            MouseEventKind::Down(MouseButton::Left) => {
                self.click(ev.column, ev.row);
                return;
            }
            _ => return,
        };
        self.pending_jump = None;
        self.reselect = None;
        self.move_selection(step);
    }

    fn click(&mut self, column: u16, row: u16) {
        let area = self.list_area;
        let inside =
            column >= area.x && column < area.right() && row >= area.y && row < area.bottom();
        let state = match self.view {
            View::List => &self.list_state,
            View::Bookmarks => &self.bookmark_state,
            View::Comments => return, // variable-height rows; use the keyboard
        };
        if !inside {
            return;
        }
        // Story rows are two lines tall.
        let idx = state.offset() + (row - area.y) as usize / 2;
        if idx >= self.row_count() {
            return;
        }
        if self.selected_row() == Some(idx) {
            self.open_comments();
        } else {
            self.pending_jump = None;
            self.reselect = None;
            self.select_row(idx);
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
            2 => self.settings.mouse = !self.settings.mouse,
            _ => {}
        }
        self.dirty = true;
    }

    /// Keys that behave the same in every view: navigation of whichever list
    /// is showing, search, opening, bookmarking, and the overlays. Returns
    /// whether the key was handled.
    fn on_key_shared(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(',') => self.open_settings(),
            KeyCode::Char('/') => self.open_prompt(PromptKind::Search),
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_row(0),
            KeyCode::Char('G') | KeyCode::End => self.select_row(usize::MAX),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Char('o') => self.open_active(),
            KeyCode::Char('O') => self.open_discussion(),
            KeyCode::Char('s') => self.toggle_bookmark(),
            _ => return false,
        }
        true
    }

    fn on_key_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Char(':') => self.open_prompt(PromptKind::Jump),
            KeyCode::Enter => self.open_comments(),
            KeyCode::Char('b') => {
                self.view = View::Bookmarks;
                self.clamp_bookmark_selection();
            }
            KeyCode::Char('r') => {
                self.toast("refreshing…");
                let current = self.selected_story().map(|s| s.id);
                self.load_feed();
                self.reselect = current;
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
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                self.view = self.comments_origin;
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_collapse(),
            KeyCode::Char('u') => self.open_comment_link(),
            _ => {}
        }
    }

    fn on_key_bookmarks(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('b') => {
                self.view = View::List;
            }
            KeyCode::Char(':') => self.open_prompt(PromptKind::Jump),
            KeyCode::Enter => self.open_comments(),
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
        if let Some(prompt) = self.prompt.take()
            && let Some(origin) = prompt.origin
        {
            self.select_row(origin);
        }
    }

    fn submit_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt.kind {
            PromptKind::Jump => {
                // Only digits can be typed, so a parse failure means the
                // number overflowed: treat it as "the end".
                if !prompt.input.is_empty() {
                    self.jump_to(prompt.input.parse().unwrap_or(usize::MAX));
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
            View::Bookmarks => self.select_row_in(View::Bookmarks, idx),
            View::List => {
                let more = self.ids_loaded < self.story_ids.len();
                if idx >= self.story_len() && more && matches!(self.stories, Load::Ready(_)) {
                    self.pending_jump = Some(idx);
                    self.toast(format!("loading stories up to #{n}…"));
                    // Parks on the last loaded story and, being near the end,
                    // starts a fetch widened to reach the target.
                    self.select_row_in(View::List, usize::MAX);
                } else {
                    self.select_row_in(View::List, idx);
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
            // A refresh targets a story, not a number: its position in the id
            // list only bounds how far to fetch, since dead stories are
            // filtered out of the list and shift later ones up.
            let idx = self
                .reselect
                .take()
                .and_then(|id| self.story_index(id))
                .unwrap_or(target);
            self.select_row_in(View::List, idx);
        } else {
            self.load_more();
        }
    }

    /// After a refresh, select the story that was selected before it, fetching
    /// ahead if it has dropped below the first page. Stories that left the
    /// feed fall back to the top.
    fn restore_selection(&mut self) {
        let Some(id) = self.reselect else {
            return;
        };
        if let Some(idx) = self.story_index(id) {
            self.reselect = None;
            self.select_row_in(View::List, idx);
        } else if let Some(pos) = self.story_ids.iter().position(|&i| i == id)
            && pos >= self.ids_loaded
        {
            self.pending_jump = Some(pos);
            self.load_more();
        } else {
            // Gone from the feed, or already fetched but filtered out as dead.
            self.reselect = None;
        }
    }

    fn story_index(&self, id: u64) -> Option<usize> {
        match &self.stories {
            Load::Ready(s) => s.iter().position(|it| it.id == id),
            _ => None,
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

    // Each view shows one list: stories, comments, or bookmarks. These address
    // "the list" of a view, so navigation is written once for all three.

    fn list_state(&self, view: View) -> &ListState {
        match view {
            View::List => &self.list_state,
            View::Comments => &self.comment_state,
            View::Bookmarks => &self.bookmark_state,
        }
    }

    fn list_state_mut(&mut self, view: View) -> &mut ListState {
        match view {
            View::List => &mut self.list_state,
            View::Comments => &mut self.comment_state,
            View::Bookmarks => &mut self.bookmark_state,
        }
    }

    fn row_count_in(&self, view: View) -> usize {
        match view {
            View::List => self.story_len(),
            View::Comments => self.visible.len(),
            View::Bookmarks => self.saved.len(),
        }
    }

    /// Select row `idx` (clamped to the list) of `view`'s list. Selecting near
    /// the end of the feed prefetches the next page.
    fn select_row_in(&mut self, view: View, idx: usize) {
        let len = self.row_count_in(view);
        if len == 0 {
            return;
        }
        let clamped = idx.min(len - 1);
        self.list_state_mut(view).select(Some(clamped));
        if view == View::List && clamped + 3 >= len {
            self.load_more();
        }
    }

    /// Selection in whichever list the current view shows.
    fn selected_row(&self) -> Option<usize> {
        self.list_state(self.view).selected()
    }

    fn row_count(&self) -> usize {
        self.row_count_in(self.view)
    }

    fn select_row(&mut self, idx: usize) {
        self.select_row_in(self.view, idx);
    }

    fn move_selection(&mut self, delta: isize) {
        let cur = self.selected_row().unwrap_or(0);
        self.select_row(cur.saturating_add_signed(delta));
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

    /// Open the story's Hacker News discussion page, whatever it links to.
    fn open_discussion(&mut self) {
        if let Some(story) = self.active_story() {
            self.mark_visited(story.id);
            self.open(&story.hn_url());
        }
    }

    /// `u`: open a link from the selected comment. Pressing it again on the
    /// same comment steps through its links in turn.
    fn open_comment_link(&mut self) {
        let Some(flat) = self
            .comment_state
            .selected()
            .and_then(|i| self.visible.get(i))
        else {
            return;
        };
        let id = flat.id;
        let links = match &self.comments {
            Load::Ready(roots) => find_comment(roots, id).map(|c| c.links.clone()),
            _ => None,
        }
        .unwrap_or_default();
        if links.is_empty() {
            self.toast("no links in this comment");
            return;
        }
        let idx = match self.link_cursor {
            Some((cid, next)) if cid == id => next % links.len(),
            _ => 0,
        };
        self.link_cursor = Some((id, idx + 1));
        self.open(&links[idx]);
        if links.len() > 1
            && let Some((msg, _)) = &mut self.toast
        {
            msg.push_str(&format!(
                " · link {} of {} (u for next)",
                idx + 1,
                links.len()
            ));
        }
    }

    fn open(&mut self, url: &str) {
        let label = util::domain(url).unwrap_or_else(|| "link".to_string());
        if (self.opener)(url) {
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

/// Depth-first search of a comment forest by id.
fn find_comment(list: &[Comment], id: u64) -> Option<&Comment> {
    list.iter().find_map(|c| {
        (c.id == id)
            .then_some(c)
            .or_else(|| find_comment(&c.children, id))
    })
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
    /// How many links the comment contains (for the badge; `u` opens them).
    pub links: usize,
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
            links: c.links.len(),
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
mod tests;
