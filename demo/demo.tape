# hacker-news-tui demo. Regenerate with:  demo/record.sh   (requires: vhs)
# Run via record.sh, not directly: it builds --release and exports
# $HACKER_NEWS_TUI_BIN. The app fetches live Hacker News data, so sleeps are
# generous enough to absorb network latency before each interaction.
Output hacker-news-tui.gif

Set Shell "bash"
Set FontSize 15
Set Width 1180
Set Height 760
Set Padding 18
Set TypingSpeed 60ms
Set WindowBar Colorful

# --- off-camera: launch and wait for the Top feed to load ---
Hide
Type "$HACKER_NEWS_TUI_BIN"
Enter
Sleep 3.5s
Show

# settle on the populated story list
Sleep 1800ms

# browse the feed
Down
Sleep 500ms
Down
Sleep 500ms
Down
Sleep 800ms

# open the discussion, let comments load, scroll, collapse a thread
Enter
Sleep 3s
Down
Sleep 700ms
Down
Sleep 700ms
Space@1s
Sleep 1500ms
Space@1s
Sleep 1200ms
Escape
Sleep 1200ms

# bookmark a couple of stories, then view the Saved list
Type "s"
Sleep 1200ms
Down
Sleep 500ms
Type "s"
Sleep 1200ms
Type "b"
Sleep 2200ms
Escape
Sleep 1200ms

# switch feeds: Ask HN
Type "4"
Sleep 2600ms

# the settings pane — opt in to remembering bookmarks
Type ","
Sleep 1500ms
Down
Sleep 700ms
Space@1s
Sleep 1600ms
Escape
Sleep 1000ms

# the help overlay
Type "?"
Sleep 2500ms
Escape
Sleep 800ms

# back to the top of the feed
Type "1"
Sleep 2200ms
