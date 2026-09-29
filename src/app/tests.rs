//! State-machine tests for [`App`](super::App), driven through key and
//! message handling with a no-op spawner so nothing touches the network.

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
        links: vec![],
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
        truncated: false,
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
    app.on_msg(Msg::Comments {
        seq,
        result: tree,
        truncated: false,
    });
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
        truncated: false,
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
    app.on_msg(Msg::Comments {
        seq,
        result: tree,
        truncated: false,
    });

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
    app.on_msg(Msg::Comments {
        seq,
        result: tree,
        truncated: false,
    });
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
        truncated: false,
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

// ── opening links ──────────────────────────────────────────────────────────

/// Replace the browser launcher with one that records the URLs it is given.
fn record_opens(app: &mut App) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = opened.clone();
    app.opener = Box::new(move |url| {
        sink.lock().unwrap().push(url.to_string());
        true
    });
    opened
}

#[test]
fn o_opens_the_article_and_shift_o_the_discussion() {
    let (mut app, _rx) = loaded(40);
    if let Load::Ready(stories) = &mut app.stories {
        stories[0].url = Some("https://example.com/post".into());
    }
    let opened = record_opens(&mut app);
    app.on_key(ch('o'));
    app.on_key(ch('O'));
    assert_eq!(
        *opened.lock().unwrap(),
        [
            "https://example.com/post",
            "https://news.ycombinator.com/item?id=1"
        ]
    );
    assert!(app.visited.contains(&1));
}

#[test]
fn u_steps_through_a_comments_links() {
    let (mut app, _rx) = loaded(40);
    app.on_key(key(KeyCode::Enter));
    let seq = app.comment_gen;
    let mut linked = leaf(2);
    linked.links = vec!["https://a.example".into(), "https://b.example".into()];
    app.on_msg(Msg::Comments {
        seq,
        result: vec![leaf(1), linked],
        truncated: false,
    });
    let opened = record_opens(&mut app);

    app.on_key(ch('u')); // comment 1 has no links
    assert!(opened.lock().unwrap().is_empty());
    assert!(app.toast.as_ref().unwrap().0.contains("no links"));

    app.on_key(ch('j'));
    for _ in 0..3 {
        app.on_key(ch('u'));
    }
    assert_eq!(
        *opened.lock().unwrap(),
        [
            "https://a.example",
            "https://b.example",
            "https://a.example"
        ] // wraps
    );
    assert_eq!(app.visible_comments()[1].links, 2);
}

#[test]
fn truncated_threads_are_reported() {
    let (mut app, _rx) = loaded(40);
    app.on_key(key(KeyCode::Enter));
    let seq = app.comment_gen;
    let tree = vec![Comment {
        children: vec![leaf(2)],
        ..leaf(1)
    }];
    app.on_msg(Msg::Comments {
        seq,
        result: tree,
        truncated: true,
    });
    assert!(app.comments_truncated);
    assert_eq!(app.comments_loaded, 2);

    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Enter)); // reopening resets until loaded
    assert!(!app.comments_truncated);
    assert_eq!(app.comments_loaded, 0);
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
fn refresh_keeps_the_selected_story_selected() {
    let (mut app, _rx) = loaded(40);
    for _ in 0..3 {
        app.on_key(ch('j')); // select story id 4
    }
    app.on_key(ch('r'));
    let seq = app.story_gen;
    // It has moved up the ranking since.
    app.on_msg(Msg::Stories {
        seq,
        result: Ok((vec![9, 4, 7], items(&[9, 4, 7]))),
    });
    assert_eq!(selected(&app), Some(1));
}

#[test]
fn refresh_fetches_ahead_for_a_story_that_dropped() {
    let (mut app, _rx) = loaded(100);
    app.on_key(ch('j')); // story id 2
    app.on_key(ch('r'));
    let seq = app.story_gen;
    let mut ids: Vec<u64> = (3..=60).collect();
    ids.push(2); // dropped to position 58, beyond the first page
    app.on_msg(Msg::Stories {
        seq,
        result: Ok((ids.clone(), items(&ids[..FIRST_PAGE]))),
    });
    assert_eq!(app.pending_jump, Some(58));
    assert!(app.loading_more);
    // One dead story in the batch shifts it up a row; it is found by id.
    let batch: Vec<u64> = ids[FIRST_PAGE..]
        .iter()
        .copied()
        .filter(|&i| i != 30)
        .collect();
    app.on_msg(Msg::MoreStories {
        seq,
        items: items(&batch),
    });
    assert_eq!(selected(&app), Some(57));
    assert_eq!(app.selected_story().unwrap().id, 2);
}

#[test]
fn refresh_falls_back_to_top_when_story_left_the_feed() {
    let (mut app, _rx) = loaded(40);
    app.on_key(ch('j'));
    app.on_key(ch('r'));
    let seq = app.story_gen;
    app.on_msg(Msg::Stories {
        seq,
        result: Ok(((50..=60).collect(), items(&[50, 51]))),
    });
    assert_eq!(selected(&app), Some(0));
    assert_eq!(app.pending_jump, None);
}

#[test]
fn refresh_gives_up_on_a_story_filtered_out_of_the_first_page() {
    let (mut app, _rx) = loaded(40);
    app.on_key(ch('j')); // story id 2
    app.on_key(ch('r'));
    let seq = app.story_gen;
    // Still ranked in the first page, but dead, so not among the items.
    app.on_msg(Msg::Stories {
        seq,
        result: Ok((vec![1, 2, 3], items(&[1, 3]))),
    });
    assert_eq!(selected(&app), Some(0));
    assert_eq!(app.pending_jump, None);
    assert_eq!(app.reselect, None);
}

#[test]
fn overflowing_jump_goes_to_the_end() {
    let (mut app, _rx) = loaded(15);
    app.on_key(ch(':'));
    typed(&mut app, "99999999999999999999999");
    app.on_key(key(KeyCode::Enter));
    assert_eq!(selected(&app), Some(14));
}

#[test]
fn switching_feeds_does_not_reselect() {
    let (mut app, _rx) = loaded(40);
    app.on_key(ch('j'));
    app.on_key(key(KeyCode::Tab));
    let seq = app.story_gen;
    app.on_msg(Msg::Stories {
        seq,
        result: Ok(((1..=40).collect(), items(&[1, 2, 3]))),
    });
    assert_eq!(selected(&app), Some(0));
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
        truncated: false,
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

// ── mouse ──────────────────────────────────────────────────────────────────

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Loaded feed with the mouse enabled and the list drawn at rows 1..29.
fn with_mouse() -> (App, UnboundedReceiver<Msg>) {
    let (mut app, rx) = loaded(40);
    app.settings.mouse = true;
    app.list_area = Rect::new(0, 1, 80, 28);
    (app, rx)
}

#[test]
fn wheel_moves_the_selection() {
    let (mut app, _rx) = with_mouse();
    app.on_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    app.on_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(selected(&app), Some(2));
    app.on_mouse(mouse(MouseEventKind::ScrollUp, 5, 5));
    assert_eq!(selected(&app), Some(1));
}

#[test]
fn click_selects_then_opens() {
    let (mut app, _rx) = with_mouse();
    let left = MouseEventKind::Down(MouseButton::Left);
    app.on_mouse(mouse(left, 10, 1 + 2 * 3 + 1)); // second line of row 3
    assert_eq!(selected(&app), Some(3));
    assert_eq!(app.view, View::List);
    app.on_mouse(mouse(left, 10, 1 + 2 * 3));
    assert_eq!(app.view, View::Comments);
    assert_eq!(app.story.as_ref().unwrap().id, 4);
}

#[test]
fn clicks_outside_the_list_or_past_the_end_do_nothing() {
    let (mut app, _rx) = with_mouse();
    let left = MouseEventKind::Down(MouseButton::Left);
    app.on_mouse(mouse(left, 10, 0)); // header
    app.list_area = Rect::new(0, 1, 80, 100);
    app.on_mouse(mouse(left, 10, 1 + 2 * 50)); // below the last story
    assert_eq!(selected(&app), Some(0));
    assert_eq!(app.view, View::List);
}

#[test]
fn mouse_is_ignored_while_disabled_or_under_an_overlay() {
    let (mut app, _rx) = with_mouse();
    app.settings.mouse = false;
    app.on_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(selected(&app), Some(0));
    app.settings.mouse = true;
    app.on_key(ch('?'));
    app.on_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert_eq!(selected(&app), Some(0));
}

#[test]
fn mouse_setting_toggles_from_the_pane() {
    let (mut app, _rx) = loaded(40);
    app.on_key(ch(','));
    app.on_key(ch('j'));
    app.on_key(ch('j'));
    app.on_key(ch(' '));
    assert!(app.settings.mouse);
    assert!(app.is_dirty());
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
            ..Default::default()
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

// ── shared navigation ──────────────────────────────────────────────────────────

#[test]
fn navigation_keys_behave_the_same_in_every_list() {
    let (mut app, _rx) = loaded(40);
    app.restore(Settings::default(), HashSet::new(), items(&[1, 2, 3]));
    app.on_key(key(KeyCode::Enter));
    let seq = app.comment_gen;
    app.on_msg(Msg::Comments {
        seq,
        result: vec![leaf(1), leaf(2), leaf(3)],
        truncated: false,
    });
    app.on_key(key(KeyCode::Esc));

    for view in [View::Comments, View::Bookmarks] {
        app.view = view;
        app.on_key(ch('G'));
        assert_eq!(app.selected_row(), Some(2), "{view:?}");
        app.on_key(key(KeyCode::PageUp)); // clamps at the top
        assert_eq!(app.selected_row(), Some(0), "{view:?}");
        app.on_key(ch('j'));
        assert_eq!(app.selected_row(), Some(1), "{view:?}");
        app.on_key(key(KeyCode::PageDown)); // clamps at the bottom
        assert_eq!(app.selected_row(), Some(2), "{view:?}");
        app.on_key(ch('g'));
        assert_eq!(app.selected_row(), Some(0), "{view:?}");
    }
}
