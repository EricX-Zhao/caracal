# Configurable scrollback length + right-side scrollbar for the terminal

Date: 2026-07-13

Files under change: `src/settings.rs`, `src/panels/settings_window.rs`,
`src/terminal/model.rs`, `src/terminal/view.rs`, `src/terminal/render.rs`,
`src/terminal/scrollback.rs`.

## Background

Today the alacritty `Term`'s scrollback capacity is hardcoded:
`scrolling_history: 10_000` in `new_term()` (`terminal/model.rs:69`). There is no
UI to change it, and no visual scrollbar — the only way to know "how far back" you
are is the existing wheel/keyboard scroll behavior in `scrollback.rs`
(`delta_from_pixels`/`delta_from_lines`, consumed by `TerminalView::on_scroll_wheel`,
`view.rs:859-873`) and the Shift+PageUp/PageDown/Home/End bindings
(`view.rs:700-714`), both of which call `term.scroll_display(Scroll::...)`.
`Term::grid().display_offset()` (lines scrolled back from the live bottom) is the
sole source of truth for scroll position; there is no separate app-level
scrollback buffer to keep in sync (`scrollback.rs` module docs).

This spec adds:
1. A "scrollback lines" setting on the Terminal settings tab.
2. A right-side scrollbar overlay on the terminal view, driven by that same
   `display_offset`/line-count model, reusing gpui-component's existing
   `Scrollbar` element (`gpui-component/crates/ui/src/scroll/scrollbar.rs`)
   rather than building a new widget.

## Decisions (confirmed with user)

### Setting: scope, range, and when it takes effect

- New field `scrollback_lines: u32` on `TerminalSettings` (`settings.rs:33-52`),
  default `10_000` via a `default_scrollback_lines()` fn following the existing
  `default_monitor_basic_interval_secs()`-style pattern (`settings.rs:54-60`),
  `#[serde(default = "default_scrollback_lines")]` so old `settings.toml` files
  without the key keep working.
- Valid range **1,000–50,000**. A `parse_scrollback_lines(s: &str) -> Option<u32>`
  function (same shape as `parse_font_size`/`parse_monitor_interval`,
  `settings_window.rs:10-31`) rejects non-numeric input and clamps/rejects
  out-of-range values the same way the existing parsers do (reject, don't
  silently clamp — matches current `parse_monitor_interval` behavior).
- **Confirmed with user: this setting only affects newly opened terminal tabs.**
  Changing it in Settings and clicking Apply does **not** resize or recreate the
  `Term` grid of any terminal tab that is already open — those keep whatever
  scrollback capacity they were created with. Only the next tab opened after the
  change picks up the new value. This avoids needing to reconcile alacritty's
  `Term`/`Grid` to a different `scrolling_history` capacity mid-session (which is
  not something `Term` supports doing in place without rebuilding the grid and
  losing/truncating existing history).

### Settings UI (`panels/settings_window.rs`)

- Add a fourth labeled group to `render_terminal_tab` (`settings_window.rs:281-344`),
  after "轮询间隔 (秒)", titled "回滚行数" with an `Input` bound to a new
  `scrollback_input: Entity<InputState>`-equivalent field (mirroring
  `font_size_input`/`monitor_interval_input`'s construction and lifecycle).
  Placeholder/current-value text uses `AppSettings.terminal.scrollback_lines`.
- `apply()` (`settings_window.rs:125`) gains a branch parsing this input via
  `parse_scrollback_lines` and writing the result into `self.draft.terminal.scrollback_lines`,
  same as the existing font/interval fields.

### Threading the value into new `Term`s (`terminal/model.rs`, `terminal/view.rs`)

- `new_term()` (`model.rs:67-75`) gains a parameter `scrollback_lines: usize`,
  replacing the hardcoded `10_000` at line 69.
- Font settings (`font_family`/`font_size`) reach `TerminalView` *after*
  construction: `Workspace::seed_font_from_settings` (`workspace.rs:600-607`)
  calls `settings::load()` fresh and then calls the `set_font_family`/`set_font_size`
  setters on the already-constructed entity. That pattern does not work here,
  because `new_term()` runs *inside* `base_setup` (`view.rs:373`), before a
  `TerminalView` entity exists for `Workspace` to reach — and unlike font,
  scrollback capacity is baked into the `Term`/`Config` at construction and (per
  the decision above) is never changed post-construction.
- Instead, `base_setup` calls `settings::load()` itself, right at the point it
  currently builds the `Term` (`view.rs:373`), and reads
  `.terminal.scrollback_lines as usize` to pass into `new_term()`. This is a
  self-contained fresh load — the same cheap TOML read `workspace.rs` already
  performs in several places (`workspace.rs:230, 266, 294, 513, 533`) — rather
  than adding a `scrollback_lines` parameter to every `TerminalView` constructor
  (`with_backend`, `new_ssh_connecting`, `new_ssh_shell`, telnet/serial
  constructors, etc.).
- The test/snapshot call site, `grid_snapshot.rs:168`, keeps a fixed literal
  (e.g. `10_000` or a small test-friendly constant) — snapshot tests are not
  expected to exercise the settings path.

### Scrollbar UI (`terminal/view.rs`, `terminal/render.rs`)

- **Display mode: hover/scroll-activity only, auto-fades otherwise**
  (confirmed with user) — use gpui-component's `ScrollbarShow::Scrolling` (its
  default), not `Hover` or `Always`. Reading the actual visibility logic in
  `scrollbar.rs:604-662`: `Hover` only shows the bar while the pointer is
  directly over the thin bar/thumb region, so it would *not* react to
  mouse-wheel scrolling (the cursor is normally over the terminal content, not
  the bar). `Scrolling` refreshes on every offset change *and* stays visible on
  hover, fading out ~2–3s after the last change (`FADE_OUT_DELAY`/
  `FADE_OUT_DURATION`, `scrollbar.rs:28-29`) — this is the mode that actually
  matches "shows on hover or while scrolling, auto-hides otherwise." This
  overlays on top of the grid rather than reserving permanent width, so
  terminal column/row sizing (`terminal_canvas`'s prepaint measurement,
  `render.rs:60+`) is unaffected.
- New adapter type, e.g. `TerminalScrollbarHandle`, implementing
  gpui-component's `ScrollbarHandle` trait
  (`gpui-component/crates/ui/src/scroll/scrollbar.rs:54-65`):
  - `content_size() -> Size<Pixels>`: height = `term.grid().total_lines() as f32 * cell_h`;
    width = the current viewport width (only the vertical axis is used, but the
    trait is axis-agnostic so both dimensions must be populated consistently
    with the viewport's own bounds).
  - `offset() -> Point<Pixels>`: converts `term.grid().display_offset()` (lines
    scrolled back from live bottom, `0` = bottom) into a **top-relative** pixel
    offset: `y = (total_lines - screen_lines - display_offset) * cell_h`.
  - `set_offset(Point<Pixels>)`: inverse conversion — compute the target
    `display_offset` from the requested `y`, diff it against the current
    `display_offset`, and call `term.scroll_display(Scroll::Delta(delta))`
    (reusing the existing scroll entry point in `scrollback.rs` rather than
    adding a second way to move the viewport).
  - Both directions read the cell height from the same value already tracked as
    `last_cell_h: f32` on `TerminalView` (`view.rs:175`, populated each paint at
    `view.rs:658-661`). Before the first paint this is `0.0`
    (`view.rs:406-409`); `offset()`/`content_size()` must guard `cell_h == 0.0`
    and report a degenerate/identity state (e.g. offset `0`, content size equal
    to viewport size) rather than dividing by zero.
  - The adapter holds the same shared `Term` handle (`SharedTerm`) the view
    already owns, plus a shared handle to the cell-height value (e.g. an
    `Rc<Cell<f32>>` populated alongside `last_cell_h`, since `ScrollbarHandle`
    methods take `&self` with no access to the view struct), so it stays valid
    across renders.
- Rendered as an absolutely-positioned child of `TerminalView::render`'s outer
  `div` (already `.relative()`, `view.rs:946`), pinned to the right edge — same
  overlay technique already used there for the connection banner
  (`view.rs:972-975`).
- No app-level visibility gate is added for "no scrollback" — this is left to
  the `Scrollbar` element's own standard behavior of not rendering a
  meaningfully-draggable/visible thumb when `content_size` for its axis does not
  exceed the viewport size, consistent with how every other scrollbar in this
  codebase (e.g. `overflow_y_scrollbar()` in `saved_connections.rs:1505`) behaves
  without bespoke conditionals.

## Testing

- Unit tests for `parse_scrollback_lines`: valid mid-range value, boundary values
  (1,000 and 50,000 accepted; 999 and 50,001 rejected), non-numeric input
  rejected — placed alongside the existing `parse_font_size`/`parse_monitor_interval`
  tests in `settings_window.rs`.
- Unit tests for `TerminalSettings` default/round-trip: a `settings.toml` missing
  `scrollback_lines` still loads (via `#[serde(default)]`) with the `10_000`
  default, matching the existing forward-compat tests in `settings.rs:128-210`.
- Unit tests for `TerminalScrollbarHandle`'s offset/content_size math against a
  `grid_snapshot`-style fixed-size term with known `total_lines()`/`screen_lines()`/
  `display_offset()`, checking both directions (line-position → pixel offset, and
  a requested pixel offset → the resulting `display_offset` after `set_offset`).
- Manual verification in the running app (via the `run` skill), since
  hover/fade behavior and visual placement are not exercised by unit tests:
  run a long-output command to build up scrollback, confirm the thumb appears
  on hover/scroll and fades when idle, drag the thumb to jump position, resize
  the window, and confirm the bar is effectively absent when a tab has no more
  history than fits on screen.
