# Terminal input auto-suggestion from command history

Date: 2026-08-06
Files under change: new `src/command_history.rs`, `src/terminal/view.rs`, `src/workspace.rs`,
`src/settings.rs`, `src/panels/settings_window.rs`, `src/main.rs`, locale files.

Not on the README's public Roadmap list — a new feature request. Distinct from the already-
stubbed `PanelId::History` ("命令历史") activity-bar panel ([activity_bar.rs](../../../src/panels/activity_bar.rs)),
which is a separate, unimplemented "browse past commands" panel out of scope here (see
Non-goals) — though it could later read the same data this feature writes.

## Background

Caracal is a terminal *emulator*, not a shell: keystrokes are encoded and written straight to
a PTY ([terminal/view.rs](../../../src/terminal/view.rs)'s `on_key_down` → `encode_key` →
`send_input`), and the remote/local shell running inside that PTY does its own line-editing.
Caracal has no OSC133/shell-integration prompt tracking (confirmed by the existing SFTP cwd-
sync feature's own doc comments — "best-effort... no OSC7, no retry, no shell-specific prompt
detection"), so it cannot reliably read back "what's currently on the input line" from
terminal output. Any command-history feature therefore has to be built by locally tracking
keystrokes as they're typed, independent of what the shell itself does with them.

`TerminalView` ([terminal/view.rs](../../../src/terminal/view.rs)) already caches everything
needed to place a screen-positioned overlay near the cursor: `last_origin_x`/`last_origin_y`
(canvas origin from the last paint, in window coordinates) and `last_cell_w`/`last_cell_h`
(cell metrics), used today by the mouse handlers to convert pixel coordinates to grid
coordinates. `cursor_position(&self) -> (usize, usize)` already returns the cursor's
scrollback-adjusted `(row, col)`, used today by the status bar's `row:col` readout — the same
inputs, used in reverse, place a suggestion popup.

`quick_commands.rs` is this project's existing precedent for a small, `~/.caracal`-persisted,
plain-Rust (no `gpui_component`) data file with a save-on-mutate pattern — the natural template
for this feature's own persistence module.

## Decisions (confirmed with user)

### Interaction: a dropdown candidate list, not inline ghost text

Typing narrows a popup list of matching historical commands (IDE-autocomplete style), not a
fish-shell-style inline suggestion after the cursor. Chosen over ghost text because it surfaces
multiple candidates at once; the trade-off (visually heavier, more surface area to get right)
was made knowingly.

### History source: Caracal records it, not the shell's own history file

On every Enter, whatever the user is confirmed to have typed via local key-tracking (see below)
becomes one history entry — nothing is read from `~/.bash_history`/`~/.zsh_history`/etc. This
avoids per-shell format parsing, avoids needing to read a file on the *remote* host over SSH,
and avoids the staleness problem of shells that only flush history to disk on clean exit. The
trade-off: a freshly-connected host has zero suggestions until the user has typed some commands
in Caracal itself; nothing is retroactively known about commands run before this feature
existed or outside Caracal.

### Persistence: saved to disk, per-connection-key, capped at 500 entries

Survives an app restart — an in-memory-only history would be far less useful. Scoped per
connection (see "Component structure" for exact key derivation per connection type), not global
— different hosts' commands are usually irrelevant to each other, and a global list would mix
unrelated environments' commands together. Each key's list is capped at the most recent 500
entries (oldest dropped) to bound file growth indefinitely.

### Input tracking: track only plain typing; any shell-line-editing key hides suggestions until the next Enter

Because Caracal cannot read back the shell's actual line content, "what's currently typed" is
approximated by locally accumulating printable characters and Backspace since the last Enter.
Any other key — arrows, Ctrl+A/E/U, Home/End, Delete, etc. — means this local approximation can
no longer be trusted to match the real line, so the suggestion dropdown is hidden (a
`tracking_desynced` flag is set) and *stays* hidden — even if the user goes back to plain typing
afterward — until the next Enter resets everything cleanly. This never touches what's actually
sent to the PTY; it's a passive observer sitting alongside the existing `encode_key`/
`send_input` call. The explicit alternative (trying to simulate more shell-side line-editing
keys for better accuracy) was rejected as unbounded complexity for diminishing correctness —
shell history recall via ↑/↓ in particular can never be tracked this way regardless of how many
keys are added.

### Accepting a suggestion fills the line; it does not execute

Selecting a suggestion (via ↑/↓ then Tab or Enter) replaces the typed prefix with the full
suggested command on the input line — the user still has to press Enter again to actually run
it. Chosen for safety: a single stray Enter should never be able to execute a different,
unintended historical command.

### Settings: on by default, with a Settings → Terminal toggle to disable

New `TerminalSettings.command_suggestions_enabled: bool`, `#[serde(default = "default_true")]`
with a small `fn default_true() -> bool { true }` helper (same shape as this file's existing
`default_monitor_interval_secs`/`default_scrollback_lines` functions, just returning `true`
instead of a number), default `true`. Mirrors the existing `monitor_basic_enabled` draft-state
toggle in
`settings_window.rs`'s Terminal tab, just with the opposite default — this feature has no
network/security exposure the way remote monitoring does, so on-by-default is the reasonable
choice, while still giving users who find the popup distracting a way to turn it off.

## Component structure

- `src/command_history.rs` (new) — plain Rust, no `gpui_component` (same CLAUDE.md §1 boundary
  as `quick_commands.rs`). Holds: the persisted shape (`HashMap<String, Vec<String>>`, key →
  that connection's history, newest-last), `load`/`save` (mirroring `quick_commands.rs`'s
  `load()`/`save()` exactly), `record(history: &mut HashMap<...>, key: &str, line: &str)`
  (skips empty/duplicate-of-last-entry, appends, truncates to the most recent 500), and
  `suggestions(history: &HashMap<...>, key: &str, prefix: &str) -> Vec<String>` (prefix match,
  deduped, most-recent-first, capped at 8).
- `src/terminal/view.rs` — `TerminalView` gains `history_key: String` (set at construction),
  `input_buffer: String`, `tracking_desynced: bool`, `suggestions: Vec<String>`,
  `selected_index: Option<usize>`. `on_key_down` gains the tracking/matching logic described
  above, inserted alongside (not replacing) the existing `encode_key`/`send_input` call. New
  render logic adds the popup as an extra positioned child when `suggestions` is non-empty,
  using `cursor_position()` + the existing cached cell-metrics/origin fields to place it.
- `src/workspace.rs` — each of `open_ssh`/`open_local`/`open_telnet`/`open_serial` computes its
  own history key from the config it already has in scope (SSH reuses `SshConfig::key()`;
  Local is the fixed string `"local"`; Telnet is `"telnet://{host}:{port}"`; Serial is
  `"serial://{port_name}"`) and passes it into `TerminalView::new(...)` as a new parameter,
  alongside the existing `font_config` argument.
- `src/settings.rs` — `TerminalSettings` gains `command_suggestions_enabled: bool` (default
  `true`).
- `src/panels/settings_window.rs` — Terminal tab gains a toggle switch, following
  `monitor_enabled_switch`'s exact pattern.
- `src/main.rs` — `mod command_history;` registered alongside the other top-level modules.
- Locale files — new `Terminal.command_suggestions` (or similar) label for the new Settings
  toggle, in both `zh-CN` and `en`.

## Testing

- `command_history.rs`: pure-function unit tests, no GPUI/network dependency — load/save
  round-trip, the empty-input and duplicate-of-last-entry skip rules on `record`, the 500-entry
  cap (oldest dropped, newest kept), and `suggestions`'s prefix-match + dedup + recency-order +
  cap-at-8 behavior (fixture `Vec<String>` history, various prefixes including no-match and
  multiple-match cases).
- `terminal/view.rs`'s new key-handling branches: no unit tests, matching this file's existing
  zero-test convention for input handling (exercised manually, like every other
  `on_key_down` branch already in this file). Manual smoke test must cover: suggestions appear
  and narrow while typing, disappear when nothing matches, arrow-key/Ctrl+A/Ctrl+U/Home/End all
  hide the dropdown and keep it hidden until the next Enter even if the user resumes plain
  typing first, Tab/Enter accept the ↑/↓-selected suggestion by sending only the unmatched
  suffix (never re-sending or backspacing the already-typed prefix), Escape dismisses without
  side effects, and a plain Enter with nothing arrowed-into still submits the typed line
  normally rather than substituting anything.
- `settings.rs`: a backward-compat test confirming an old `settings.toml` without
  `command_suggestions_enabled` still deserializes with it defaulting to `true`, matching this
  file's existing convention for every other `TerminalSettings` field.

## Non-goals

- No reading of shell history files (`~/.bash_history`, `~/.zsh_history`, etc.) — Caracal-
  recorded history only.
- No fuzzy/substring matching — prefix-only.
- No simulating shell-side line editing beyond the desync-and-hide rule — arrow-key cursor
  movement, Ctrl+A/E/U, and shell-native history recall are never tracked or reproduced.
- No cross-connection history sharing — strictly scoped per connection key, never global.
- No configurable match algorithm, suggestion count, or history cap — fixed at prefix-match/
  top-8/500-entries for v1.
- No history browsing/management UI (search all history, delete an entry, export, etc.) — that
  belongs to the separate, already-stubbed `PanelId::History` panel, which is out of scope for
  this spec entirely, not merely deferred.
- No accept-and-execute in one step — accepting a suggestion always fills the line only.
