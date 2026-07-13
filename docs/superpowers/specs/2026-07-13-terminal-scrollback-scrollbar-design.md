# Configurable scrollback length + right-side scrollbar for the terminal

Date: 2026-07-13

Files under change: `src/settings.rs`, `src/panels/settings_window.rs`,
`src/terminal/model.rs`, `src/terminal/view.rs`, `src/terminal/grid_snapshot.rs`,
`src/panels/terminal.rs`.

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

### Scrollbar UI — corrected placement: `panels/terminal.rs`, not `terminal/`

**This codebase enforces a hard architectural boundary** (historically documented
in a since-removed `CLAUDE.md §1`, still governing the code today — verified via
`grep -rn gpui_component src/terminal/*.rs`, which returns no `use` statements):
`src/terminal/` must never import `gpui_component`; that crate is only allowed in
`src/panels/*.rs` adapters. gpui-component's `Scrollbar`/`ScrollbarHandle`
therefore **cannot** be used or referenced from `terminal/view.rs` or
`terminal/render.rs` — the original draft of this section was wrong to place
the scrollbar there. The correct owner is `TerminalPanel` in
`panels/terminal.rs:56-61`, the existing thin adapter that already does nothing
but `div().size_full().child(self.terminal.clone())`.

- **Two small pure-gpui accessors added to `TerminalView`** (`terminal/view.rs`,
  no `gpui_component` involved, so the boundary holds):
  - `pub fn shared_term(&self) -> SharedTerm` — clones `self.term` (an `Arc`,
    cheap).
  - `pub fn last_cell_height(&self) -> f32` — returns `self.last_cell_h`
    (already tracked, `view.rs:175`, populated each paint,
    `view.rs:658-661`; `0.0` before the first paint, `view.rs:406-409`).
- **Display mode: hover/scroll-activity only, auto-fades otherwise**
  (confirmed with user) — use gpui-component's `ScrollbarShow::Scrolling`
  explicitly (not `Hover`, not `Always`, and not left to inherit
  `cx.theme().scrollbar_show`, so this stays correct even if some other panel's
  theme configuration changes later). Reading the actual visibility logic in
  `gpui-component/crates/ui/src/scroll/scrollbar.rs:604-662`: `Hover` only shows
  the bar while the pointer is directly over the thin bar/thumb region, so it
  would *not* react to mouse-wheel scrolling (the cursor is normally over the
  terminal content, not the bar). `Scrolling` refreshes on every offset change
  *and* stays visible on hover, fading out ~2–3s after the last change
  (`FADE_OUT_DELAY`/`FADE_OUT_DURATION`, `scrollbar.rs:28-29`) — the mode that
  actually matches "shows on hover or while scrolling, auto-hides otherwise."
- New adapter type `TerminalScrollbarHandle` (private to `panels/terminal.rs`),
  implementing gpui-component's `ScrollbarHandle` trait
  (`gpui-component/crates/ui/src/scroll/scrollbar.rs:54-65`, `Clone`-bounded —
  `Scrollbar::vertical<H: ScrollbarHandle + Clone>(&H)`). It holds a
  `SharedTerm` (via `TerminalView::shared_term()`) plus an `Rc<Cell<f32>>` cell
  height that `TerminalPanel::render` refreshes every frame from
  `TerminalView::last_cell_height()` (no `cx` reaches `ScrollbarHandle` methods,
  so the cell height must already be cached outside the entity system by the
  time they're called).
  - **Sign convention (verified against `scrollbar.rs:826,861-863,937-946`,
    same convention gpui's own `ScrollHandle` uses):** `offset().y` is `0` at
    the *top* of content and increasingly **negative** toward the bottom,
    down to `-(content_height - viewport_height)`. This is the opposite of a
    naive "distance scrolled down" reading — get the sign wrong and the thumb
    drags backwards.
  - `content_size() -> Size<Pixels>`: `size(px(0.0), total_lines as f32 * cell_h)`.
    Width is a dummy `0px` — confirmed unused for a vertical-only scrollbar
    (`scrollbar.rs:541-559` only reads `.height`/`offset().y` when
    `axis.is_vertical()`).
  - `offset() -> Point<Pixels>`: let `hidden_above = total_lines - screen_lines
    - display_offset` (lines of history above the current viewport); return
    `point(px(0.0), -(hidden_above as f32 * cell_h))`.
  - `set_offset(Point<Pixels>)`: inverse — `hidden_above = (-offset.y / cell_h).round()`,
    `target_display_offset = total_lines - screen_lines - hidden_above`, diff
    against the current `display_offset`, and call
    `scrollback::apply(&mut term, Scroll::Delta(delta))` (reusing the existing
    scroll entry point in `terminal/scrollback.rs`, `pub fn`, rather than
    adding a second way to move the viewport).
  - Both directions guard `cell_h <= 0.0` (before first paint) by returning/
    treating the offset and content size as zero rather than dividing by zero.
  - `total_lines()`/`screen_lines()` come from alacritty's `Dimensions` trait
    (`use alacritty_terminal::grid::Dimensions;`), callable directly on `Term`
    (`Term<T>: Dimensions`, delegates to the grid internally); `display_offset()`
    is inherent on `Grid`, reached via `term.grid().display_offset()` (not
    part of `Dimensions`).
- `TerminalPanel` gains a field `scrollbar_handle: Option<TerminalScrollbarHandle>`
  (`None` initially — `TerminalPanel::new` takes no `cx`/`Window` today and
  isn't changed; the handle is lazily built on first render, when `cx` is
  available). `TerminalPanel::render` (`panels/terminal.rs:56-61`) becomes:
  initialize `scrollbar_handle` if `None` (using `terminal.read(cx).shared_term()`),
  refresh its cached cell height from `terminal.read(cx).last_cell_height()`,
  then render `div().relative().size_full().child(self.terminal.clone())` with
  an added child: `div().absolute().inset_0().child(Scrollbar::vertical(handle)
  .id("terminal-scrollbar").scrollbar_show(ScrollbarShow::Scrolling))` — the
  explicit `ScrollbarShow::Scrolling` override is why the lower-level
  `Scrollbar::vertical` builder is used directly instead of the
  `ScrollableElement::vertical_scrollbar` convenience trait (which has no way
  to override the show mode away from the ambient theme default).
- No app-level visibility gate is added for "no scrollback" — this is left to
  the `Scrollbar` element's own standard behavior of not rendering a
  meaningfully-draggable/visible thumb when `content_size` for its axis does not
  exceed the viewport size (`scrollbar.rs:568-572`, "hide scrollbar if the
  scroll area is smaller than the container"), consistent with how every other
  scrollbar in this codebase (e.g. `overflow_y_scrollbar()` in
  `saved_connections.rs:1505`) behaves without bespoke conditionals.

## Testing

- Unit tests for `parse_scrollback_lines`: valid mid-range value, boundary values
  (1,000 and 50,000 accepted; 999 and 50,001 rejected), non-numeric input
  rejected — placed alongside the existing `parse_font_size`/`parse_monitor_interval`
  tests in `settings_window.rs`.
- Unit tests for `TerminalSettings` default/round-trip: a `settings.toml` missing
  `scrollback_lines` still loads (via `#[serde(default)]`) with the `10_000`
  default, matching the existing forward-compat tests in `settings.rs:128-210`.
- Unit tests for `TerminalScrollbarHandle`'s offset/content_size math (in
  `panels/terminal.rs`), against a `grid_snapshot`-style fixed-size term with
  known `total_lines()`/`screen_lines()`/`display_offset()`, checking both
  directions (line-position → pixel offset, and a requested pixel offset → the
  resulting `display_offset` after `set_offset`).
- Manual verification in the running app (via the `run` skill), since
  hover/fade behavior and visual placement are not exercised by unit tests:
  run a long-output command to build up scrollback, confirm the thumb appears
  on hover/scroll and fades when idle, drag the thumb to jump position, resize
  the window, and confirm the bar is effectively absent when a tab has no more
  history than fits on screen.
