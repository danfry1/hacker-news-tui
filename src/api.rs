//! Hacker News Firebase API client.

use futures::future::{BoxFuture, FutureExt, join_all};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

pub const BASE: &str = "https://hacker-news.firebaseio.com/v0";

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
    pub children: Vec<Comment>,
}

impl Comment {
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

/// Fetch a comment forest under `root_kids`, bounded to `max` total comments so
/// even huge threads stay responsive, and to [`MAX_CONCURRENT`] in-flight requests.
pub async fn fetch_comments(client: Client, root_kids: Vec<u64>, max: usize) -> Vec<Comment> {
    let remaining = AtomicUsize::new(max);
    let sem = Semaphore::new(MAX_CONCURRENT);
    build_forest(&client, root_kids, 0, &remaining, &sem).await
}

fn build_forest<'a>(
    client: &'a Client,
    ids: Vec<u64>,
    depth: usize,
    remaining: &'a AtomicUsize,
    sem: &'a Semaphore,
) -> BoxFuture<'a, Vec<Comment>> {
    async move {
        if depth >= 12 || ids.is_empty() {
            return Vec::new();
        }
        let take = ids.len().min(remaining.load(Ordering::Relaxed));
        if take == 0 {
            return Vec::new();
        }
        remaining.fetch_sub(take, Ordering::Relaxed);
        let ids: Vec<u64> = ids.into_iter().take(take).collect();

        let items = join_all(ids.iter().map(|&id| fetch_item_limited(client, sem, id))).await;
        let nodes = join_all(items.into_iter().filter_map(|r| r.ok().flatten()).map(
            |item| async move {
                if item.deleted || item.dead {
                    return None;
                }
                let children =
                    build_forest(client, item.kids.clone(), depth + 1, remaining, sem).await;
                Some(Comment {
                    id: item.id,
                    by: if item.by.is_empty() {
                        "[unknown]".to_string()
                    } else {
                        item.by
                    },
                    time: item.time,
                    text: crate::util::clean_html(item.text.as_deref().unwrap_or("")),
                    children,
                })
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
