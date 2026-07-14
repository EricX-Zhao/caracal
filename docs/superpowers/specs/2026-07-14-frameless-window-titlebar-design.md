# Frameless window + custom title bar

## Background

`header.rs` already builds a 40px in-app row (brand icon, File/View/Terminal/Help
dropdown menus, centered active-tab title) sitting *below* the OS's native window
chrome. That file's own doc comment already flagged the gap: "The OS window
decorations are kept for now; a frameless window + custom min/max/close controls
... are a follow-up phase." This spec is that follow-up: remove the native
titlebar/border and fold window-move/minimize/maximize/close into the existing row,
matching how Zed (and most modern GPUI-style apps) look.

Two library-level findings shape this design:

1. **`gpui-component`'s `platform_title_bar` crate (what Zed itself uses for this)
   is GPL-3.0-or-later and depends on Zed-internal crates** (`project`, `settings`,
   `theme`, `workspace`, `zed_actions`, ...) that aren't meant for external
   consumption. It cannot be a dependency here (this project is MIT/Apache — see
   the "no GPL terminal crates" boundary already documented in project memory).
   Everything below uses only `gpui` (Apache-2.0, already a dependency) and
   `gpui-component`'s MIT-licensed `ui` crate pieces we already depend on.
2. **The hard part — Linux shadow rendering + 8-direction resize-edge
   hit-testing — is already wired up and just dormant.** `gpui_component::Root`
   (used for every window in this app) always wraps its content in
   `window_border()` (`Root::bordered` defaults to `true` — confirmed in that
   crate's own tests). `window_border()` queries `window.window_decorations()`
   each frame: if it reports `Decorations::Server` (the default — what we get
   today), it's a zero-cost passthrough; if `Decorations::Client`, it draws the
   shadow, rounded corners, tiling-aware insets, and all 8 resize-edge/corner hit
   zones automatically. So requesting client-side decorations is the *entire*
   trigger for Linux's resize/shadow behavior — no new resize code needed.

Verified directly against the vendored sources (not assumed):
`~/.cargo/git/checkouts/zed-.../crates/gpui/src/platform.rs` (`WindowOptions`,
`TitlebarOptions`, `WindowDecorations`, `Decorations`, `WindowButtonLayout`),
`.../crates/gpui/src/window.rs` (`Window::start_window_move`, `minimize_window`,
`zoom_window`, `remove_window`, `titlebar_double_click`, `is_maximized`,
`window_decorations`), `~/.cargo/git/checkouts/gpui-component-.../crates/ui/src/
window_border.rs` (the dormant shadow/resize machinery), and that same repo's
icon assets (`window-minimize.svg`, `window-maximize.svg`, `window-restore.svg`,
`window-close.svg` already ship in `gpui-component-assets`, so `IconName` already
has matching variants — no new SVGs to add).

## Decisions (confirmed with user)

- **All three platforms**, not Linux-only — even though only Linux (this dev
  session, Wayland) can actually be click-tested. Windows/macOS get a
  best-effort, compile-verified implementation using the same gpui APIs Zed
  itself relies on for those platforms, but cannot be manually verified here.
- **The centered active-tab title text is part of the draggable region** — click
  and drag on it moves the window, matching Zed/VS Code convention, not reserved
  for a future click-to-do-something-else interaction.
- **macOS gets native traffic-light buttons** (`TitlebarOptions.
  traffic_light_position`), not hand-drawn ones — that's the platform convention,
  and `gpui_macos` already implements the OS-level plumbing for it. Only
  Linux/Windows get our own drawn minimize/maximize/close buttons.
- **Linux button order comes from the desktop environment when available**
  (`cx.button_layout()` reads e.g. GNOME's `gtk-decoration-layout`), falling back
  to `WindowButtonLayout::linux_default()` (minimize, maximize, close, right-
  aligned) if the platform doesn't report one.

## Architecture

### `main.rs` — window construction

The main window's `WindowOptions` gains:

```rust
WindowOptions {
    window_bounds: Some(WindowBounds::Windowed(bounds)),
    focus: true,
    titlebar: Some(TitlebarOptions {
        title: None,
        appears_transparent: true,
        traffic_light_position: Some(point(px(12.0), px(12.0))),
    }),
    window_decorations: Some(WindowDecorations::Client),
    ..Default::default()
}
```

- `appears_transparent: true` hides the native titlebar text/background on
  macOS and Windows (each platform's own gpui backend already implements this;
  confirmed present in both `gpui_macos` and `gpui_windows`).
- `traffic_light_position` only affects macOS; ignored elsewhere.
- `window_decorations: Some(WindowDecorations::Client)` is the Linux/Wayland
  request for client-side decorations (per that field's own doc comment, "Wayland
  only... may be ignored" elsewhere) — this is what flips `window_border()`'s
  passthrough into real shadow+resize behavior.

The settings window and new-connection window (`workspace.rs`'s `open_settings`,
`sessions.rs`'s new-connection window) are small utility dialogs, not the main
work surface — they keep native decorations (no changes there). Only the main
window gets the custom title bar.

### `header.rs` — the row itself

Extend the existing `render_header` rather than introducing a new module — this
*is* the module that row already lives in, and its own doc comment already
anticipated this change.

- **New parameter**: `render_header` gains a `window: &mut Window` parameter (its
  caller, `Workspace::render`, already has one in scope) so it can read
  `window.is_maximized()` (to choose the maximize-vs-restore icon) and
  `cx.button_layout()` while building the tree. (The interactive handlers below
  don't need this — `on_mouse_down`/`on_click` closures receive their own fresh
  `&mut Window` at dispatch time regardless of the enclosing function's
  signature.)
- **Draggable background**: the existing "brand + menus" child and the existing
  centered-title child both currently sit in the row with plain flex layout and
  no mouse handlers. Add `.on_mouse_down(MouseButton::Left, |ev, window, _cx| {
  if ev.click_count() == 2 { window.titlebar_double_click() } else {
  window.start_window_move() } })` to the row's own background *and* to the
  centered-title child specifically (so dragging the title text also moves the
  window, per the confirmed decision) — but *not* to the four menu buttons
  themselves, so their existing click/dropdown behavior is untouched. gpui
  dispatches mouse-down to the most specific (topmost) hit first; putting the
  move/double-click handler only on the row background and the title text
  (never on the button elements) means a click that lands on a button never
  reaches the drag handler at all — no propagation fighting needed.
- **Window-controls cluster** (`#[cfg(not(target_os = "macos"))]`, i.e. compiled
  out entirely on macOS since native traffic lights replace it there): three
  small icon buttons appended to the right of the row —
  - Minimize: `IconName::WindowMinimize`, `on_click` → `window.minimize_window()`.
  - Maximize/Restore: icon switches between `IconName::WindowMaximize` and
    `IconName::WindowRestore` based on `window.is_maximized()`; `on_click` →
    `window.zoom_window()`.
  - Close: `IconName::WindowClose`, `on_click` → `window.remove_window()`.
  - Order/side placement: `cx.button_layout()` if `Some`, else
    `WindowButtonLayout::linux_default()`. (On Windows this API returns `None`
    in practice — Windows doesn't have a user-configurable button-layout concept
    — so it always falls back to the same right-aligned min/max/close order,
    which matches Windows convention anyway.)
  - Hover styling matches the existing menu buttons' ghost/xsmall look, with a
    close-button hover tinted `cx.theme().danger` (standard convention — red on
    hover for close, neutral for the other two).
- **macOS left padding**: when `#[cfg(target_os = "macos")]`, the brand+menu
  cluster gets extra left padding (`pl(px(78.0))`, roughly matching traffic-light
  cluster width + margin) so the brand icon doesn't sit under the native
  traffic lights.

### What needs no changes at all

- `gpui_component::Root` / `window_border()` — already active, already correct,
  triggered purely by the `WindowOptions` change above.
- Resize-from-edge on Windows — handled by `gpui_windows`'s own
  `WM_NCHITTEST`/`WM_NCCALCSIZE` handling once the native titlebar is hidden
  (confirmed present in that crate); no app-level resize code needed there
  either.
- Icons — `window-minimize`/`window-maximize`/`window-restore`/`window-close`
  already ship in `gpui-component-assets`; `IconName`'s macro-generated enum
  already has matching variants.

## Testing

- No unit-testable logic here beyond icon selection (maximize vs. restore based
  on a `bool`) — if that's factored into a small pure function
  (`fn maximize_icon(is_maximized: bool) -> IconName`), it gets a couple of
  trivial unit tests, matching this codebase's convention of only unit-testing
  pure logic and verifying `render()`-level UI by hand.
- Manual verification (Linux, this dev session — a live Wayland compositor):
  drag-move via row background and via the centered title, double-click-to-
  maximize/restore, minimize/maximize/close buttons, resize from all 4 edges +
  4 corners, snap/tile against a screen edge (confirms `window_border()`'s
  tiling-aware inset logic), confirm menu buttons (文件/视图/终端/帮助) still
  open their dropdowns normally (not swallowed by the new drag handler).
- Windows/macOS: compile-only (`cargo build`/`cargo check`) — no toolchain for
  either available in this environment, so those code paths are verified by
  reading the vendored `gpui_windows`/`gpui_macos` sources against what we call,
  not by running the app.

## Non-goals

- No changes to the settings window or new-connection window's chrome — they
  keep native OS decorations.
- No Linux `.desktop` file / installed-icon work (separate, already-known gap
  from the earlier icon-design work — not in scope here).
- No attempt to replicate macOS's native traffic-light *hover* animations or
  Windows 11's Snap Layouts flyout — those are OS-drawn even in a frameless
  window and Just Work once the platform's own decoration is hidden; nothing
  to build.
