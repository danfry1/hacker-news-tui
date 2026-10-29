//! Hacker News API client: the official Firebase API for feeds and items, and
//! Algolia's HN Search API for whole comment threads.

use futures::future::{BoxFuture, FutureExt, join_all};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;

pub const BASE: &str = "https://hacker-news.firebaseio.com/v0";
/// Algolia's HN Search API, which serves a story's whole comment tree in one
/// response. The Firebase API needs a request per comment.
pub const ALGOLIA: &str = "https://hn.algolia.com/api/v1";
/// How long to wait for a thread from Algolia before falling back to Firebase.
/// Shorter than the client's own timeout, since a fallback follows.
const ALGOLIA_TIMEOUT: Duration = Duration::from_secs(8);

/// Cap on in-flight item requests. The HN Firebase API has no batch endpoint, so
/// a feed page or a comment thread is many single-item fetches; bounding how many
/// run at once keeps the connection pool sane and is polite to the API.
const MAX_CONCURRENT: usize = 16;

pub type Client = reqwest::Client;

/// The browsable Hacker News feeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feed {
    Top,
    New,
    Best,
    Ask,
    Show,
    Jobs,
}

impl Feed {
    pub const ALL: [Feed; 6] = [
        Feed::Top,
        Feed::New,
        Feed::Best,
        Feed::Ask,
        Feed::Show,
        Feed::Jobs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Feed::Top => "Top",
            Feed::New => "New",
            Feed::Best => "Best",
            Feed::Ask => "Ask",
            Feed::Show => "Show",
            Feed::Jobs => "Jobs",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Feed::Top => "topstories",
            Feed::New => "newstories",
            Feed::Best => "beststories",
            Feed::Ask => "askstories",
            Feed::Show => "showstories",
            Feed::Jobs => "jobstories",
        }
    }

    pub fn next(self) -> Feed {
        let i = Feed::ALL.iter().position(|&f| f == self).unwrap_or(0);
        Feed::ALL[(i + 1) % Feed::ALL.len()]
    }

    pub fn prev(self) -> Feed {
        let i = Feed::ALL.iter().position(|&f| f == self).unwrap_or(0);
        Feed::ALL[(i + Feed::ALL.len() - 1) % Feed::ALL.len()]
    }
}

/// A raw item from the API (story, comment, job, …).
#[derive(Deserialize, Serialize, Clone, Default, Debug)]
pub struct Item {
    pub id: u64,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub descendants: Option<i64>,
    #[serde(default)]
    pub kids: Vec<u64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub dead: bool,
    #[serde(default)]
    pub deleted: bool,
}

impl Item {
    /// The canonical Hacker News discussion page for this item.
    pub fn hn_url(&self) -> String {
        format!("https://news.ycombinator.com/item?id={}", self.id)
    }

    /// The link a story points at — its URL, or the HN page for self/Ask posts.
    pub fn target_url(&self) -> String {
        self.url.clone().unwrap_or_else(|| self.hn_url())
    }

    pub fn comment_count(&self) -> i64 {
        self.descendants.unwrap_or(0)
    }
}

/// A comment with its nested replies, text already cleaned for display.
#[derive(Clone, Debug)]
pub struct Comment {
    pub id: u64,
    pub by: String,
    pub time: u64,
    pub text: String,
    /// `http(s)` links in the comment, from the raw HTML (see [`crate::util::extract_links`]).
    pub links: Vec<String>,
    /// The text laid out as paragraphs, quotes and code, for display. `text`
    /// is the same content as plain text, for search.
    pub body: Vec<crate::util::Block>,
    pub children: Vec<Comment>,
}

impl Comment {
    /// Build a comment from its raw HTML, cleaning the text and collecting links.
    fn new(id: u64, by: String, time: u64, html: &str, children: Vec<Comment>) -> Self {
        Comment {
            id,
            by: if by.is_empty() {
                "[unknown]".to_string()
            } else {
                by
            },
            time,
            text: crate::util::clean_html(html),
            links: crate::util::extract_links(html),
            body: crate::util::blocks(html),
            children,
        }
    }

    /// Total number of descendants (replies, recursively).
    pub fn descendant_count(&self) -> usize {
        self.children.len()
            + self
                .children
                .iter()
                .map(Comment::descendant_count)
                .sum::<usize>()
    }
}

pub async fn fetch_ids(client: &Client, feed: Feed) -> Result<Vec<u64>, String> {
    let url = format!("{BASE}/{}.json", feed.endpoint());
    client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<u64>>()
        .await
        .map_err(|e| e.to_string())
}

pub async fn fetch_item(client: &Client, id: u64) -> Result<Option<Item>, String> {
    let url = format!("{BASE}/item/{id}.json");
    client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Option<Item>>()
        .await
        .map_err(|e| e.to_string())
}

/// Fetch a batch of items by id, at most [`MAX_CONCURRENT`] at a time, preserving
/// order and dropping any that are missing, dead, or deleted.
pub async fn fetch_items(client: Client, ids: Vec<u64>) -> Vec<Item> {
    let fetched: Vec<_> = futures::stream::iter(ids)
        .map(|id| fetch_item(&client, id))
        .buffered(MAX_CONCURRENT) // ordered, ≤ MAX_CONCURRENT in flight
        .collect()
        .await;
    fetched
        .into_iter()
        .filter_map(|r| r.ok().flatten())
        .filter(|it| !it.deleted && !it.dead)
        .collect()
}

/// Fetch an item while holding a concurrency permit, so the shared semaphore
/// bounds the total number of simultaneous requests across the whole forest.
async fn fetch_item_limited(
    client: &Client,
    sem: &Semaphore,
    id: u64,
) -> Result<Option<Item>, String> {
    let _permit = sem.acquire().await.ok();
    fetch_item(client, id).await
}

/// A node of an Algolia thread: the story itself or a comment under it.
#[derive(Deserialize)]
struct AlgoliaNode {
    id: u64,
    #[serde(default, rename = "type")]
    kind: String,
    /// `null` for deleted comments.
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    created_at_i: u64,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    children: Vec<AlgoliaNode>,
}

/// Convert Algolia nodes into comments. Deleted comments and anything that
/// isn't a comment are dropped with their replies, as the Firebase path does.
fn algolia_comments(nodes: Vec<AlgoliaNode>) -> Vec<Comment> {
    nodes
        .into_iter()
        .filter(|n| n.kind == "comment")
        .filter_map(|n| {
            let by = n.author?;
            let html = n.text.unwrap_or_default();
            let children = algolia_comments(n.children);
            Some(Comment::new(n.id, by, n.created_at_i, &html, children))
        })
        .collect()
}

async fn fetch_algolia(client: &Client, story: u64) -> Result<Vec<Comment>, String> {
    let node = client
        .get(format!("{ALGOLIA}/items/{story}"))
        .timeout(ALGOLIA_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json::<AlgoliaNode>()
        .await
        .map_err(|e| e.to_string())?;
    Ok(algolia_comments(node.children))
}

/// Order top-level comments as Hacker News ranks them, which is the order of
/// the story's `kids`. Algolia lists them by id instead. Comments that aren't
/// in `kids` keep their relative order at the end.
fn rank_top_level(kids: &[u64], comments: Vec<Comment>) -> Vec<Comment> {
    let mut by_id: HashMap<u64, Comment> = HashMap::with_capacity(comments.len());
    let mut unranked = Vec::new();
    let ranked_ids: HashSet<u64> = kids.iter().copied().collect();
    for c in comments {
        if ranked_ids.contains(&c.id) {
            by_id.insert(c.id, c);
        } else {
            unranked.push(c);
        }
    }
    let mut ranked: Vec<Comment> = kids.iter().filter_map(|id| by_id.remove(id)).collect();
    ranked.append(&mut unranked);
    ranked
}

/// Fetch a story's whole comment thread. Algolia serves it in one request;
/// top-level comments it hasn't indexed yet (its index trails new comments by
/// a little) are fetched from Firebase. If Algolia is unavailable, the thread
/// comes from Firebase alone, bounded to `max` comments. Also reports whether
/// a bound cut the thread short.
pub async fn fetch_thread(
    client: Client,
    story: u64,
    kids: Vec<u64>,
    max: usize,
) -> (Vec<Comment>, bool) {
    let Ok(mut comments) = fetch_algolia(&client, story).await else {
        return fetch_comments(client, kids, max).await;
    };
    let indexed: HashSet<u64> = comments.iter().map(|c| c.id).collect();
    let missing: Vec<u64> = kids
        .iter()
        .copied()
        .filter(|id| !indexed.contains(id))
        .collect();
    let mut truncated = false;
    if !missing.is_empty() {
        let (mut extra, cut) = fetch_comments(client, missing, max).await;
        comments.append(&mut extra);
        truncated = cut;
    }
    (rank_top_level(&kids, comments), truncated)
}

/// Fetch a comment forest under `root_kids`, bounded to `max` total comments so
/// even huge threads stay responsive, and to [`MAX_CONCURRENT`] in-flight requests.
/// Also reports whether those bounds cut the thread short.
pub async fn fetch_comments(
    client: Client,
    root_kids: Vec<u64>,
    max: usize,
) -> (Vec<Comment>, bool) {
    let remaining = AtomicUsize::new(max);
    let truncated = AtomicBool::new(false);
    let sem = Semaphore::new(MAX_CONCURRENT);
    let limits = Limits {
        remaining: &remaining,
        truncated: &truncated,
        sem: &sem,
    };
    let forest = build_forest(&client, root_kids, 0, limits).await;
    (forest, truncated.load(Ordering::Relaxed))
}

/// Shared bounds for one comment-tree fetch.
#[derive(Clone, Copy)]
struct Limits<'a> {
    /// Comments still allowed to be fetched.
    remaining: &'a AtomicUsize,
    /// Set when a depth or count bound skipped comments.
    truncated: &'a AtomicBool,
    sem: &'a Semaphore,
}

fn build_forest<'a>(
    client: &'a Client,
    ids: Vec<u64>,
    depth: usize,
    limits: Limits<'a>,
) -> BoxFuture<'a, Vec<Comment>> {
    async move {
        if ids.is_empty() {
            return Vec::new();
        }
        if depth >= 12 {
            limits.truncated.store(true, Ordering::Relaxed);
            return Vec::new();
        }
        let remaining = limits.remaining;
        let take = ids.len().min(remaining.load(Ordering::Relaxed));
        if take < ids.len() {
            limits.truncated.store(true, Ordering::Relaxed);
        }
        if take == 0 {
            return Vec::new();
        }
        remaining.fetch_sub(take, Ordering::Relaxed);
        let ids: Vec<u64> = ids.into_iter().take(take).collect();

        let items = join_all(
            ids.iter()
                .map(|&id| fetch_item_limited(client, limits.sem, id)),
        )
        .await;
        let nodes = join_all(items.into_iter().filter_map(|r| r.ok().flatten()).map(
            |item| async move {
                if item.deleted || item.dead {
                    return None;
                }
                let children = build_forest(client, item.kids.clone(), depth + 1, limits).await;
                let html = item.text.as_deref().unwrap_or("");
                Some(Comment::new(item.id, item.by, item.time, html, children))
            },
        ))
        .await;

        nodes.into_iter().flatten().collect()
    }
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_cycles_forward_and_back() {
        assert_eq!(Feed::Top.next(), Feed::New);
        assert_eq!(Feed::Jobs.next(), Feed::Top); // wraps
        assert_eq!(Feed::Top.prev(), Feed::Jobs); // wraps
        assert_eq!(Feed::New.prev(), Feed::Top);
        // A full forward loop returns to the start.
        let mut f = Feed::Top;
        for _ in 0..Feed::ALL.len() {
            f = f.next();
        }
        assert_eq!(f, Feed::Top);
    }

    #[test]
    fn feed_titles_match_order() {
        let titles: Vec<_> = Feed::ALL.iter().map(|f| f.title()).collect();
        assert_eq!(titles, ["Top", "New", "Best", "Ask", "Show", "Jobs"]);
    }

    #[test]
    fn target_url_prefers_url_then_falls_back_to_hn() {
        let with_url = Item {
            id: 42,
            url: Some("https://example.com/x".into()),
            ..Default::default()
        };
        assert_eq!(with_url.target_url(), "https://example.com/x");

        let self_post = Item {
            id: 42,
            url: None,
            ..Default::default()
        };
        assert_eq!(
            self_post.target_url(),
            "https://news.ycombinator.com/item?id=42"
        );
    }

    #[test]
    fn comment_count_defaults_to_zero() {
        assert_eq!(Item::default().comment_count(), 0);
        let it = Item {
            descendants: Some(7),
            ..Default::default()
        };
        assert_eq!(it.comment_count(), 7);
    }

    #[test]
    fn descendant_count_walks_the_whole_tree() {
        let leaf = |id| Comment {
            id,
            by: "x".into(),
            time: 0,
            text: String::new(),
            links: vec![],
            body: vec![],
            children: vec![],
        };
        let tree = Comment {
            children: vec![
                Comment {
                    children: vec![leaf(3), leaf(4)],
                    ..leaf(2)
                },
                leaf(5),
            ],
            ..leaf(1)
        };
        // children: 2,5 ; grandchildren: 3,4 → 4 descendants.
        assert_eq!(tree.descendant_count(), 4);
        assert_eq!(leaf(9).descendant_count(), 0);
    }

    fn node(json: &str) -> AlgoliaNode {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn algolia_thread_becomes_cleaned_comments() {
        let story = node(
            r#"{"id": 1, "type": "story", "author": "op", "children": [
                {"id": 2, "type": "comment", "author": "a", "created_at_i": 100,
                 "text": "<p>see <a href=\"https://x.dev/\">x</a> &amp; more",
                 "children": [
                    {"id": 3, "type": "comment", "author": "b", "text": "reply", "children": []}
                 ]},
                {"id": 4, "type": "comment", "author": null, "text": null, "children": [
                    {"id": 5, "type": "comment", "author": "c", "text": "orphan", "children": []}
                ]}
            ]}"#,
        );
        let comments = algolia_comments(story.children);
        assert_eq!(
            comments.len(),
            1,
            "deleted comments are dropped with their replies"
        );
        let c = &comments[0];
        assert_eq!((c.id, c.by.as_str(), c.time), (2, "a", 100));
        assert_eq!(c.text, "see x & more");
        assert_eq!(c.links, ["https://x.dev/"]);
        assert_eq!(c.children[0].text, "reply");
    }

    #[test]
    fn algolia_drops_non_comment_nodes() {
        let story = node(
            r#"{"id": 1, "children": [
                {"id": 2, "type": "pollopt", "author": "a", "text": "option"},
                {"id": 3, "type": "comment", "author": "b", "text": "hi"}
            ]}"#,
        );
        let ids: Vec<u64> = algolia_comments(story.children)
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, [3]);
    }

    #[test]
    fn top_level_comments_follow_hn_ranking() {
        let leaf = |id| Comment::new(id, "x".into(), 0, "", vec![]);
        // Algolia order is by id; HN ranks 30 first. 50 isn't in `kids`.
        let comments = vec![leaf(10), leaf(20), leaf(30), leaf(50)];
        let ranked = rank_top_level(&[30, 10, 40, 20], comments);
        let ids: Vec<u64> = ranked.iter().map(|c| c.id).collect();
        assert_eq!(ids, [30, 10, 20, 50]);
    }

    #[test]
    fn item_deserializes_with_missing_fields() {
        // The API omits most fields on many items; defaults must fill in.
        let it: Item = serde_json::from_str(r#"{"id": 1, "type": "story"}"#).unwrap();
        assert_eq!(it.id, 1);
        assert_eq!(it.score, 0);
        assert!(it.url.is_none());
        assert!(it.kids.is_empty());
    }
}
