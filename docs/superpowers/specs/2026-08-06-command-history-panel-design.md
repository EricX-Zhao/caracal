# Command History panel (right sidebar)

Date: 2026-08-06
Files under change: new `src/panels/command_history_panel.rs`, `src/terminal/view.rs`,
`src/workspace.rs`, `locales/app.yml`.

Implements the pre-existing, previously-stubbed `PanelId::History` ("命令历史") activity-bar
panel — explicitly out of scope in
[the command-history-suggestions design](2026-08-06-command-history-suggestions-design.md)'s
Non-goals, now brought in as its own follow-up feature. Surfaced during manual testing of that
feature: the user expected the pre-existing "命令历史" sidebar icon to show the recorded history,
since the two features share a name despite being scoped separately.

## Background

`PanelId::History` already exists in [activity_bar.rs](../../../src/panels/activity_bar.rs)
(icon, right-dock placement, locale label) but is wired to the generic
[`StubPanel`](../../../src/panels/stub.rs) placeholder via `workspace.rs`'s `stub_panels`
HashMap, alongside `PanelId::Network`. This spec replaces that stub wiring with a real panel.

`src/command_history.rs` (built for the suggestions feature) already exposes
`load_for(key: &str) -> Vec<String>`, reading `~/.caracal/command_history.toml` for one
connection's history, newest-last. Every `TerminalView` already carries a private
`history_key: String` (SSH: `SshConfig::key()`; Local: `"local"`; Telnet:
`"telnet://{host}:{port}"`; Serial: `"serial://{port}"`) and an existing `title()` accessor.

`MonitorPanel` ([monitor.rs](../../../src/panels/monitor.rs)) is the closest existing
precedent: a per-connection `AnyView` cached in a `Workspace`-level `HashMap<String, AnyView>`,
an `active_*: Option<String>` pointer, a placeholder fallback, focus-following via each
connection-opening method's existing `cx.on_focus` hook, and — critically — it does **not**
force the right dock to switch to it on focus (`SavedConnections` stays the default visible
panel; the user opens it manually via its activity-bar icon). This spec follows that same
non-forcing behavior, not SFTP's forcing one. The one structural difference: Monitor is
SSH-only (`show_monitor(config: SshConfig, ...)`), since remote-host monitoring only makes
sense over an SSH exec channel; History applies to all 4 connection types, so it's triggered
by the connection-agnostic `history_key` string instead.

`Workspace::send_to_focused_terminal(text: &str, execute: bool, cx: &mut App)` already exists
(used by SFTP's "send path to terminal" action) and is exactly the primitive row-click needs.

## Decisions (confirmed with user)

### Row click fills the input line, does not execute

Clicking a history entry sends it to the currently-focused terminal via
`send_to_focused_terminal(text, execute: false, cx)` — the user can still edit or reconsider
before running it. This matches the suggestion dropdown's own accept behavior (also fill-only,
confirmed in the sibling feature's design) — the whole app is now consistent on this point:
nothing from a history/suggestion source ever auto-executes.

### A live substring search box, not a plain scrollable list

A history can hold up to 500 entries (`command_history.rs`'s existing cap), so finding a
specific past command by eye alone is impractical. A search input filters the list live by
substring containment (not prefix — this is manual browsing, a different use case from the
typing feature's live prefix-suggestion), matching `sessions.rs`'s existing search-box
convention for consistency with the rest of the app.

### No live auto-refresh; a manual refresh button instead

The panel loads its connection's history once, when first constructed (matching
Monitor/SFTP's per-connection panel construction timing), and only re-reads the file on an
explicit "刷新" (refresh) button click — mirroring Monitor/SFTP's own existing manual-refresh
convention. No subscription or event channel back into the typing feature's `TerminalView` —
the two features stay decoupled, each only reading the same on-disk file.

## Component structure

- `src/panels/command_history_panel.rs` (new) — `CommandHistoryPanel`: `history_key: String`,
  `entries: Vec<String>` (loaded via `command_history::load_for`), a search `Entity<InputState>`,
  a filtered live-rendered list (reusing the `.id(...).flex().flex_col().flex_1().min_h(px(0.0)).overflow_y_scroll()`
  pattern already established in `quick_commands_panel.rs` and the Settings window), a refresh
  button, and a per-row click handler calling `send_to_focused_terminal`. Header text via the
  `CommandHistory.title` locale key (`"命令历史: %{label}"` / `"Command History: %{label}"`,
  mirroring `Monitor.title`'s existing `"资源监控: %{label}"` pattern), where `%{label}` is the
  connection's `TerminalView::title()`.
- `src/terminal/view.rs` — `TerminalView` gains `pub fn history_key(&self) -> &str` (mirrors
  the existing `pub fn title(&self) -> &str`).
- `src/workspace.rs` — `Workspace` gains `history_panels: HashMap<String, AnyView>`,
  `active_history: Option<String>`, `history_placeholder: AnyView` (mirroring
  `monitor_panels`/`active_monitor`/`monitor_placeholder` field-for-field); `show_history(key:
  String, window, cx)` and `show_history_placeholder(window, cx)` methods (mirroring
  `show_monitor`/`show_monitor_placeholder`, including the same "don't force `right_active`"
  behavior); `resolve(PanelId::History)` gets its own real arm (mirroring
  `resolve(PanelId::Monitor)`'s fallback-to-placeholder shape), removed from the generic
  `stub_panels` construction loop (which keeps building `PanelId::Network`'s stub as before —
  that one stays a stub, this spec only touches History). Each of `open_local_with`/`open_ssh`/
  `open_telnet`/`open_serial`'s existing `cx.on_focus` closure gains a `show_history(...)` call:
  `open_ssh` reuses its own already-computed `key` variable directly; `open_local_with` passes
  the literal `"local".to_string()`; `open_telnet`/`open_serial` read the key back via the new
  `history_key()` accessor on the closure's already-captured `WeakEntity<TerminalView>` (avoids
  duplicating the `telnet://`/`serial://` format strings, which stay solely owned by
  `TerminalView`'s own constructors).
- `locales/app.yml` — new `CommandHistory.*` keys (title, search placeholder, refresh button,
  empty-state message), in both `zh-CN` and `en`.

## Testing

- If the substring-filter logic is written as a standalone pure function (e.g. `fn
  filter_entries(entries: &[String], query: &str) -> Vec<String>`), it gets unit tests
  (case-sensitivity choice, empty-query-returns-all, no-match-returns-empty) — no GPUI
  dependency, same style as `command_history.rs`'s own tests.
- No tests for the panel itself (needs a live gpui window), matching every other panel in this
  codebase's existing zero-test convention.
- Manual smoke test must cover: opening History for a connection that already has recorded
  commands shows them, newest-first; typing in the search box filters live by substring;
  clicking a row fills the focused terminal's input line without executing; the refresh button
  picks up a command recorded after the panel was first shown; switching focus between two
  different connections' terminal tabs shows each one's own distinct history (proving the
  per-connection-key scoping); the panel does not force the right dock to switch to it when a
  terminal gains focus (consistent with Monitor, unlike SFTP).

## Non-goals

- No delete/clear-history action from this panel.
- No export.
- No cross-connection search (strictly scoped to the focused connection's own `history_key`,
  same as the typing feature).
- No live auto-refresh while the panel is open — manual refresh button only.
- No new interaction beyond click-to-fill — no right-click context menu, no multi-select.
