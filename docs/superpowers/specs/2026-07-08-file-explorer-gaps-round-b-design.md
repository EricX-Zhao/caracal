# File Explorer gaps, round B: hidden files, directory history, cwd sync

Date: 2026-07-08
Files under change: `src/panels/sftp.rs`, `src/terminal/view.rs`, `src/workspace.rs`.

Item 5 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md), round B (round A —
context menu/rename/properties — is done). Three remaining gaps: hidden-files toggle,
directory history, and cwd sync between the SFTP browser and the terminal. Bookmarks (also
originally part of item 5) were dropped from scope per user decision during brainstorming.

## Background

`SftpPanel` (`src/panels/sftp.rs`) already has a toolbar, a path bar with a copy-path button,
and a `DataTable` fed by `FileTableDelegate.entries: Vec<SftpEntry>`. SFTP `read_dir`
(`terminal/ssh.rs`'s `sftp_read_dir`) returns *all* entries including dotfiles — no
server-side or client-side hidden-file filtering exists today.

There is no directory-navigation history at all — `enter_dir`/`go_up`/typing-a-path-and-
pressing-Enter each just overwrite `self.path` and call `refresh()`.

caracal's terminal has **no OSC7/shell-integration cwd tracking** — `TerminalView` only knows
the working directory it was *spawned* with (`terminal/view.rs`), never learns about `cd`
commands the user runs afterward. `Workspace::send_to_focused_terminal(&self, text: &str,
execute: bool, cx: &App)` (`workspace.rs:412`) already exists (built for quick commands) and
can inject text into whichever terminal last had focus — this is exactly what "send path to
terminal" needs, no new plumbing. `Workspace::focused_terminal: Option<WeakEntity<TerminalView>>`
(`workspace.rs:72`) is already tracked and kept current by an existing focus-observation
subscription — the reverse direction ("sync from terminal") has no equivalent capability
today and needs new investigation.

`TerminalView` wraps an `alacritty_terminal::Term` (`self.term: Mutex<Term<EventProxy>>`).
`terminal/selection.rs` already builds `alacritty_terminal::selection::Selection` values
programmatically (`selection::start`, driven by mouse events, not user typing) and reads them
back via `term.selection_to_string()` (`selection.rs:129`, wrapped as
`selection::selected_text`). `SelectionType::Lines` (confirmed in the pinned
`alacritty_terminal` 0.26.0 source, `src/selection.rs:93-98`) selects a whole line from a
single point — this is directly reusable to read arbitrary line text out of the terminal's
grid without needing to learn a separate cell-iteration API: construct a `Lines`-type
`Selection` at a target row, call `selection_to_string()`, then restore whatever selection
was there before (so this doesn't clobber a real user selection in progress).

## Decisions (confirmed with user)

### Scope: bookmarks dropped

Round B now covers only hidden files, directory history, and cwd sync. Bookmarks (originally
also part of item 5) are out of scope for this round entirely — not deferred to a future
round, just removed from the item's plan.

### Hidden files: client-side filter over an always-full fetch

`FileTableDelegate` keeps the full server response in a new `all_entries: Vec<SftpEntry>`
field; the existing `entries: Vec<SftpEntry>` field becomes the *filtered display list*
derived from it. Toggling hidden files re-derives `entries` from `all_entries` in place (no
new SFTP round-trip) via a new `apply_hidden_filter(&mut self, show_hidden: bool)` method —
`show_hidden: bool` lives on `SftpPanel` (user-facing toggle state, alongside
`download_dir`/`pending_op`), read by `refresh()`'s success callback to filter freshly-fetched
data too. A file/dir is "hidden" if its name starts with `.` (matches every other file
browser's convention, including nyaterm's). New toolbar button, right side (matching
nyaterm's placement), reusing `gpui_component`'s built-in `IconName::Eye`/`IconName::EyeOff`
(both already exist as bundled icon assets — confirmed via
`crates/assets/assets/icons/eye.svg` / `eye-off.svg` in the pinned `gpui-component` checkout,
the same way the path bar's existing copy-path button already uses `IconName::Copy` directly
without going through caracal's own `AppIcon` enum). Default off (hidden files hidden),
matching nyaterm. Not persisted — session/panel-instance state only, matching this round's
other two features' non-persistence (see below).

### Directory history: session-only, dropdown button (not focus-triggered)

A `history: Vec<String>` field on `SftpPanel`, appended to (skipping consecutive duplicates)
on every successful navigation (`enter_dir`, `go_up`, and committing a typed path in the path
bar) — capped at 20 entries, oldest dropped first. Not persisted (matches nyaterm's own
ephemeral per-session cache, which is memory-only and cleared on app restart — this round
doesn't even persist across a panel being closed and reopened, since `SftpPanel` itself isn't
long-lived-cached across dock-panel destroy/recreate cycles today).

**Deviation from nyaterm's exact UX**: nyaterm shows history as a popup while the path bar is
*being edited* (focus-triggered). This spec uses a dedicated small dropdown button next to
the path input instead (the same `DropdownButton` + `PopupMenuItem` list pattern already used
three times in this codebase — icon picker, serial-port picker, saved-connections' More-menu)
showing the most recent 5 directories, click-to-navigate. This is a deliberate de-risking
adaptation: it reuses an already-proven component instead of wiring new focus-in/focus-out
event handling onto `InputState` (unconfirmed whether that's cleanly exposed), and matches
this codebase's established pattern for "small navigable list triggered by a button" more
closely than nyaterm's own focus-triggered popup would.

### cwd sync: "send to terminal" (reliable) + "sync from terminal" (best-effort)

**Send to terminal** (browser → terminal): a new path-bar button
(`IconName::ArrowRight`) calls `self.workspace.read_with(cx, |ws, cx|
ws.send_to_focused_terminal(&format!("cd '{}'", self.path), true, cx))` — reuses
`send_to_focused_terminal` exactly as-is, no changes needed to `Workspace`. Requires
`SftpPanel` to gain a `workspace: WeakEntity<Workspace>` field (new constructor parameter),
set from `Workspace::show_sftp` the same way `NewConnectionWindow` already holds a
`WeakEntity<SavedConnectionsPanel>` back-reference.

**Sync from terminal** (terminal → browser, best-effort, explicitly accepted as fragile by
the user during brainstorming): a new path-bar button (`IconName::ArrowLeft`) triggers:
1. `Workspace` gains `pub fn guess_focused_terminal_cwd(&self, cx: &mut Context<Self>) ->
   Task<Option<String>>` — returns `Task::ready(None)` immediately if there's no focused
   terminal; otherwise captures the terminal's current cursor row (`TerminalView::
   cursor_position().0`, already exists), calls the terminal's existing `send_text("pwd",
   true)`, then (inside `cx.spawn`) waits a fixed ~400ms via `cx.background_executor()
   .timer(...)` (no shell-integration signal exists to wait on properly — this fixed delay
   *is* the "best-effort" part), then reads the line immediately below the captured cursor
   row (new `TerminalView::line_text`, see below) and returns it if it looks like an absolute
   path (starts with `/`), `None` otherwise.
2. `TerminalView` gains `pub fn line_text(&self, row: usize) -> String` — locks `self.term`,
   saves the current `term.selection` (so a real in-progress user selection isn't clobbered),
   temporarily sets `term.selection = Some(Selection::new(SelectionType::Lines,
   Point::new(Line(row), Column(0)), Side::Left))`, reads `term.selection_to_string()`,
   restores the saved selection, returns the trimmed text (or empty string if the row had no
   selectable content).
3. `SftpPanel`'s click handler awaits the `Task<Option<String>>`; on `Some(path)`, treats it
   exactly like a successfully-committed path-bar navigation (sets `self.path`, pushes to
   `history`, calls `refresh()`); on `None`, sets `self.status` to a message like "无法从终端
   获取当前目录" rather than navigating anywhere.

**Known limitations, accepted as out of scope for this round** (the user explicitly chose the
best-effort approach knowing this): multi-line shell prompts break the "read the row right
below the captured cursor row" assumption; a shell slower than ~400ms to echo+respond produces
a false negative (reads a not-yet-updated line, most likely rejected by the `/`-prefix check
rather than silently navigating somewhere wrong, but not guaranteed); a command already
running in the terminal when "sync from terminal" is triggered will have its own output
interleaved with the injected `pwd`, likely also caught by the `/`-prefix validation but not
guaranteed. No retry, no OSC7, no shell-specific prompt detection.

## Component structure

- `src/terminal/view.rs` — new `TerminalView::line_text(&self, row: usize) -> String`.
- `src/workspace.rs` — new `Workspace::guess_focused_terminal_cwd(&self, cx: &mut
  Context<Self>) -> Task<Option<String>>`; `show_sftp` passes `cx.entity().downgrade()` into
  `SftpPanel::new`'s new `workspace` parameter.
- `src/panels/sftp.rs`:
  - `SftpPanel` gains `workspace: WeakEntity<Workspace>`, `show_hidden: bool`, `history:
    Vec<String>` fields; `SftpPanel::new`'s signature gains the `workspace` parameter.
  - `FileTableDelegate` gains `all_entries: Vec<SftpEntry>` and `apply_hidden_filter(&mut
    self, show_hidden: bool)`; the existing `entries` field becomes the filtered view.
  - `refresh()`'s success callback sets `all_entries` then calls `apply_hidden_filter`
    instead of setting `entries` directly.
  - `enter_dir`, `go_up`, and the path-bar's Enter-to-navigate handler each push the
    navigated-to path onto `history` (dedup consecutive, cap at 20).
  - `render_toolbar` gains the hidden-files toggle button.
  - `render_path_bar` gains the history dropdown button, "send to terminal" button, and
    "sync from terminal" button.
  - New methods: `toggle_hidden_files(&mut self, cx: &mut Context<Self>)`,
    `send_path_to_terminal(&self, cx: &Context<Self>)`, `sync_cwd_from_terminal(&mut self,
    window: &mut Window, cx: &mut Context<Self>)`, and a shared `navigate_to(&mut self, path:
    String, window: &mut Window, cx: &mut Context<Self>)` helper that does "set `self.path`,
    push to `history` (dedup consecutive, cap at 20), `sync_path_input`, `refresh`" in one
    place. All navigation call sites route through it: the three pre-existing ones
    (`enter_dir`, `go_up`, the path-bar's Enter-to-navigate handler) are refactored to call
    it instead of duplicating those same four steps inline, and the two new ones (the history
    dropdown's click handler, `sync_cwd_from_terminal`'s success path) call it too — so
    history-pushing has exactly one implementation, not five.

## Testing

- No unit tests for the new GPUI UI wiring (toolbar button, path-bar buttons, dropdown) —
  matches the existing zero-test convention across `panels/*.rs`.
- No unit tests for `TerminalView::line_text` or `Workspace::guess_focused_terminal_cwd` —
  both need a live `alacritty_terminal::Term`/window context that this codebase's existing
  terminal tests (`terminal/view.rs`'s `#[cfg(test)] mod tests`, which test pure functions
  like font-config resolution) aren't set up to construct; matches the fact that no other
  terminal-grid-reading code in this file has test coverage either (`cursor_position`,
  `copy_selection_to_clipboard` are also untested).
- `apply_hidden_filter` is a pure function over `Vec<SftpEntry>` and *is* straightforward to
  unit test if the implementer finds it natural to extract as a standalone free function
  (`fn filter_hidden(entries: &[SftpEntry], show_hidden: bool) -> Vec<SftpEntry>`) rather than
  an inherent method — not a hard requirement, matching this file's existing style of not
  extracting every small helper purely for testability (see the previous round's plan).
- Manual smoke test must cover: toggle hidden files on/off, confirm dotfiles appear/disappear
  without a visible refresh flicker; navigate through several directories, open the history
  dropdown, confirm it shows the last 5 in most-recent-first order, click one and confirm it
  navigates there; click "send to terminal" with a terminal focused, confirm a `cd` command
  appears in that terminal; click "sync from terminal" after manually `cd`-ing in the
  terminal, confirm the browser follows (accepting some flakiness per the Decisions section);
  click "sync from terminal" with no terminal ever focused, confirm a graceful status message
  rather than a panic; click "sync from terminal" while the terminal is mid-command (e.g. a
  long-running `sleep 5`), confirm no crash and a status message rather than a wrong
  navigation.

## Non-goals

- No bookmarks (dropped from this round's scope entirely, not deferred).
- No true bidirectional auto-sync (nyaterm's "auto-sync-cwd, highlighted when on" toggle) —
  would need OSC7/shell-integration support that doesn't exist anywhere in caracal's terminal
  emulator; a much larger cross-cutting change, out of scope here.
- No shell-specific prompt detection or retry logic for "sync from terminal" — a single fixed
  delay, single read, single validation pass; failures surface as a status message, not a
  retry loop.
- No persistence for hidden-files preference or directory history — both are per-panel-
  instance session state, matching nyaterm's own ephemeral behavior for these two features.
- No "6-column" list header expansion (owner/group columns) — unrelated to this round's three
  features, not part of item 5's remaining scope per the original roadmap breakdown.
