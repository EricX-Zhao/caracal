# Quick Commands (bottom drawer + status bar)

Date: 2026-07-07
Files under change: new `src/quick_commands.rs`, new `src/panels/quick_commands_panel.rs`,
`src/terminal/view.rs`, `src/workspace.rs`.

Item 2 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md) — chosen as the
second sub-project because it's a fully independent new feature, low-risk place to validate
a new panel + new persisted-config pattern end to end (per the roadmap's own reasoning).

## Background

caracal has no quick-commands feature at all (only an unused `AppIcon::QuickCmd` enum value
and an unrelated "quick SSH" TODO button). nyaterm's reference version
([nyaterm-ui-layout-analysis.md §5](../../reference/nyaterm-ui-layout-analysis.md)) has a
docked panel with 3 view modes, categories, sort, pin, color/icon tags, `{{variable}}`
templating, and import — this spec deliberately builds a much smaller first pass.

Two supporting gaps surfaced while designing this:
- `TerminalView` (`src/terminal/view.rs`) has no public way to inject text into the
  terminal — `send_input`/`paste_from_clipboard` are both private. A quick command needs to
  send its saved text to whichever terminal is currently focused.
- `Workspace` (`src/workspace.rs`) tracks `terminal_views: Vec<WeakEntity<TerminalView>>`
  (every terminal, for the settings-page font broadcast) but not *which one is currently
  focused* — needed both to know where to send a quick command and, per this spec's second
  half, to show that terminal's cursor position in the status bar.

## Decisions (confirmed with user)

### Placement: bottom drawer + enhanced status bar, not a side-panel slot

nyaterm's own quick-commands panel lives in a "底部辅助区域" (bottom auxiliary area), not a
side dock — the user chose to build the equivalent for caracal rather than reusing the
existing left/right activity-bar single-slot pattern, even though that would have needed no
new UI infrastructure.

- **Status bar becomes the toggle surface.** `Workspace::render_status_bar` (currently an
  empty 22px bar, its own doc comment already says "reserved for future info") gets a left
  icon cluster (holding, for now, a single quick-commands toggle icon — structured so a
  second icon can be added later without redesign) and a right-aligned info slot showing the
  focused terminal's cursor position as `row:col` (1-indexed for display), blank when no
  terminal is focused.
- **The drawer itself** is a new fixed-height (~220px, not resizable — resizing is a later
  nice-to-have, not needed for v1) full-width panel inserted between the existing body row
  (`[left activity bar][body][right activity bar]`) and the status bar, toggled by the
  status-bar icon. Closed by default at startup. No enum/registry abstraction like
  `PanelId`'s (that pattern was justified by 7 different panels across 2 sides; a single
  bottom consumer doesn't need the same machinery) — just a `show_quick_commands: bool` on
  `Workspace`.

### Cursor position display

Bundled into this same task (not a separate sub-project) because it shares the status-bar
work. `TerminalView` gains `pub fn cursor_position(&self) -> (usize, usize)` reading
`self.term.lock().renderable_content().cursor.point` — the same computation
`terminal/grid_snapshot.rs`'s `snapshot_content` already does for painting (0-indexed
row/col, row adjusted by `display_offset` for scrollback), so this introduces no new
locking pattern. The status bar reads it from `Workspace.focused_terminal` on every render
and displays `row+1:col+1`; displays nothing when `focused_terminal` is `None` or the weak
ref has died.

### Sending text to a terminal

New `TerminalView::pub fn send_text(&self, text: &str, execute: bool)`, reusing the existing
`encode_paste` (bracketed-paste-aware) encoder that `paste_from_clipboard` already uses
internally, so quick-command text goes through the same normalization (line-ending
handling, bracket-wrapping when the terminal is in `BRACKETED_PASTE` mode) as a real paste.
When `execute` is `true`, a trailing `\r` is appended after the encoded payload. Returns
without writing anything if `text` is empty (matching `encode_paste`'s existing
empty-string `None` behavior).

### Focused-terminal tracking

`Workspace` gains `focused_terminal: Option<WeakEntity<TerminalView>>`, set inside the same
`cx.on_focus` closure that already updates `active_title` at all 5 terminal-creation call
sites (`open_local`, `open_local_with`, `open_ssh`, `open_telnet`, `open_serial`) — no new
subscription needed, just one more field write alongside the existing
`set_active_title_from` call.

### Data model and persistence

New `src/quick_commands.rs`, mirroring `config.rs`/`settings.rs`'s established
load/save/path shape exactly, persisted to a **new**, separate
`~/.config/caracal/quick_commands.toml` (not folded into `connections.toml` or
`settings.toml` — matches the project's one-file-per-concern convention).

```rust
pub struct QuickCommand {
    pub id: String,
    pub label: String,
    pub command: String,
    pub execution_mode: ExecutionMode,
}

pub enum ExecutionMode {
    Execute,
    Append,
}
```

No `description`, `category`, `pinned`, `color_tag`, or `icon_tag` fields — deliberately
excluded per the confirmed minimal-viable-set scope; add them when a later pass actually
needs them, not speculatively now.

### v1 feature scope (confirmed minimal)

- Flat list, no categories, no search, no sort modes, no view-mode switcher (tile/list/
  compact — just one list layout), no pin, no color/icon tags, no import, no
  `{{variable}}` templating.
- **Add/Edit is an inline form**, matching `saved_connections.rs`'s `ConnForm` idiom
  (toggle-visibility, embedded in the panel, not a standalone window) — three fields:
  label, command (textarea), execution mode (2-way pill: 执行 / 追加, matching the
  `Self::pill` idiom already used for the serial-config pills in `saved_connections.rs`).
- Row shows label + a truncated command preview + an execution-mode indicator; hover
  reveals edit/delete icon buttons (same convention as `ConnectionItem`'s hover actions).
  Clicking a row (outside the hover-action icons) sends it to `Workspace.focused_terminal`
  per its `execution_mode`; if no terminal is focused, the click is a no-op (row could be
  visually disabled/dimmed, or a toast — implementation's call, not a hard requirement here).

## Component structure

- `src/quick_commands.rs` — `QuickCommand`, `ExecutionMode`, `quick_commands_path()`,
  `load() -> Vec<QuickCommand>`, `save(&[QuickCommand]) -> anyhow::Result<()>`. Pure data +
  I/O, no `gpui`/`gpui_component` imports (same layering rule `config.rs`/`settings.rs`
  follow).
- `src/panels/quick_commands_panel.rs` — `QuickCommandsPanel`: owns the loaded
  `Vec<QuickCommand>`, the inline-form toggle state, a `workspace: WeakEntity<Workspace>`
  callback handle (to reach `focused_terminal` and call `send_text`). Persists on every
  add/edit/delete (no debounce needed at this scale — `saved_connections.rs` also persists
  synchronously on every mutation).
- `src/workspace.rs` — `focused_terminal` field (write side, as described above),
  `show_quick_commands: bool` field + toggle method, owns the `Entity<QuickCommandsPanel>`
  (created once in `Workspace::new`, like `saved_panel`), `render_status_bar` rewritten to
  show the icon cluster + cursor position, and the drawer inserted into `render()`'s body
  between the existing body row and the status bar.
- `src/terminal/view.rs` — `send_text`, `cursor_position`, both additive (no changes to
  existing methods).

## Testing

- `src/quick_commands.rs`: unit tests mirroring `config.rs`/`settings.rs`'s existing shape
  — default/round-trip serde tests, a backward-compat-shaped test (though there's no prior
  format to be backward-compatible with yet, the test still locks in that
  `#[serde(default)]` coverage behaves as expected for a partially-specified TOML entry).
- `TerminalView::cursor_position`: a unit test is possible following the existing pattern in
  `terminal/grid_snapshot.rs`'s own test module (construct a `Term`, feed it some input via
  the test harness already used there, assert the reported position) — include one if the
  implementer finds it straightforward with the existing test scaffolding; not a hard
  requirement if it turns out to need substantial new test infrastructure, since
  `grid_snapshot.rs`'s existing tests already exercise the same underlying
  `renderable_content().cursor` read path.
- `TerminalView::send_text`: no unit test — like `send_input`/`paste_from_clipboard`, this
  writes to a live backend and isn't meaningfully unit-testable in isolation; consistent
  with those methods having no existing tests either.
- The panel UI itself (`quick_commands_panel.rs`, the status-bar changes in `workspace.rs`)
  isn't unit-tested, consistent with the rest of the codebase's `panels/*.rs`/render code.
  Verification is a manual smoke test (add a command, send it in both modes, toggle the
  drawer, confirm cursor position updates and clears correctly across tab switches).

## Non-goals

- No resizable drawer height.
- No categories, search, sort, pin, tags, import, or variable templating (see v1 scope
  above) — explicitly deferred.
- No "send to all sessions" broadcast (nyaterm's sync-groups feature) — caracal has no
  concept of session groups at all yet.
- No changes to `header.rs` — the toggle icon lives in the status bar, not the menu bar.
