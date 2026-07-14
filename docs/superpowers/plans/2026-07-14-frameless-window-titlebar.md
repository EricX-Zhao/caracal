# Frameless window + custom title bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the OS-native window border/titlebar and fold window-move,
double-click-to-maximize, and minimize/maximize/close controls into the
existing `header.rs` row, on all three platforms.

**Architecture:** `main.rs` requests client-side decorations + a transparent
native titlebar. `header.rs`'s existing 40px row grows a draggable region (the
centered title) and — on Linux/Windows only — a window-control button
cluster whose side/order comes from `cx.button_layout()`. macOS keeps native
traffic-light buttons instead of a hand-drawn cluster. The Linux shadow +
8-direction resize-edge hit-testing is *already implemented* in
`gpui_component::Root`/`window_border()` (dormant until client-side
decorations are requested) — no new resize code is needed anywhere in this
plan.

**Tech Stack:** `gpui` (`Window::start_window_move`/`minimize_window`/
`zoom_window`/`remove_window`/`titlebar_double_click`/`is_maximized`/
`window_decorations`, `App::button_layout`, `WindowButtonLayout`/
`WindowButton`, `TitlebarOptions`, `WindowDecorations`), `gpui-component`
(`IconName::WindowMinimize`/`WindowMaximize`/`WindowRestore`/`WindowClose` —
already shipped in `gpui-component-assets`, no new SVGs).

## Global Constraints

- No dependency on `gpui-component`'s `platform_title_bar` crate (GPL-3.0 +
  coupled to Zed-internal crates) — everything here uses only `gpui` and the
  MIT `gpui-component` `ui` crate pieces already in this project's dependency
  tree.
- Windows/macOS code paths are build-verified only (`cargo build`) — this
  environment has no Windows or macOS toolchain. Linux is manually
  click-tested (this is a live Wayland session).
- Only the main window changes. The settings window and new-connection
  window keep native OS decorations.

---

### Task 1: Window-control icons + pure fallback/selection helpers

**Files:**
- Modify: `src/panels/icons.rs`
- Modify: `src/panels/header.rs`
- Test: inline `#[cfg(test)] mod tests` in `src/panels/header.rs`

**Interfaces:**
- Produces: `AppIcon::WindowMinimize`, `AppIcon::WindowMaximize`,
  `AppIcon::WindowRestore`, `AppIcon::WindowClose` (consumed by Task 4).
  `header::or_default_layout(Option<WindowButtonLayout>) -> WindowButtonLayout`
  and `header::maximize_icon(bool) -> AppIcon` (consumed by Task 4).

- [ ] **Step 1: Add the four window-control `AppIcon` variants**

In `src/panels/icons.rs`, add to the `AppIcon` enum (after `SerialPort,`):

```rust
    SerialPort,
    // 标题栏窗口控制按钮（Linux/Windows 专用；macOS 用系统原生交通灯按钮）
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
}
```

And add to the `From<AppIcon> for IconName` match (`icons.rs`'s `impl From<AppIcon> for IconName`), right before the final `Upload | Download | Pencil | Sessions => unreachable!(),` line:

```rust
            SerialPort => IconName::Cpu,
            WindowMinimize => IconName::WindowMinimize,
            WindowMaximize => IconName::WindowMaximize,
            WindowRestore => IconName::WindowRestore,
            WindowClose => IconName::WindowClose,
            // Upload / Download / Pencil / Sessions are handled by custom SVG in
            // `icon()`, not reachable here.
            Upload | Download | Pencil | Sessions => unreachable!(),
```

- [ ] **Step 2: Run a build to confirm the icon variants compile**

Run: `cargo build`
Expected: succeeds (these variants aren't referenced anywhere yet, so no "unused" warnings — enum variants don't warn unused).

- [ ] **Step 3: Write the failing tests for the two pure helpers**

Add to the bottom of `src/panels/header.rs` (new file content, since this file currently has no test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximize_icon_switches_on_maximized_state() {
        assert_eq!(maximize_icon(false), AppIcon::WindowMaximize);
        assert_eq!(maximize_icon(true), AppIcon::WindowRestore);
    }

    #[test]
    fn or_default_layout_passes_through_a_reported_layout() {
        let reported = WindowButtonLayout {
            left: [Some(WindowButton::Close), None, None],
            right: [Some(WindowButton::Minimize), Some(WindowButton::Maximize), None],
        };
        assert_eq!(or_default_layout(Some(reported)), reported);
    }

    #[test]
    fn or_default_layout_falls_back_to_right_aligned_min_max_close() {
        let layout = or_default_layout(None);
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(
            layout.right,
            [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                Some(WindowButton::Close),
            ]
        );
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail (helpers don't exist yet)**

Run: `cargo test --lib panels::header`
Expected: FAIL with "cannot find function `maximize_icon`"/"`or_default_layout`" (or a `use` resolution error for `WindowButtonLayout`/`WindowButton`, added in the next step).

- [ ] **Step 5: Add the two pure helper functions and the new imports**

In `src/panels/header.rs`, change the `use gpui::{...}` line from:

```rust
use gpui::{
    App, IntoElement, ParentElement, SharedString, Styled, WeakEntity, div, px,
};
```

to:

```rust
use gpui::{
    App, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    SharedString, Styled, WeakEntity, Window, WindowButton, WindowButtonLayout, div, px,
};
```

Add these two functions after the `use` block (before `new_local_item`):

```rust
/// Icon for the maximize/restore button: swaps to the "restore" glyph once
/// the window is already maximized, so the button always reads as "the
/// thing this click will do next".
fn maximize_icon(is_maximized: bool) -> AppIcon {
    if is_maximized {
        AppIcon::WindowRestore
    } else {
        AppIcon::WindowMaximize
    }
}

/// Fallback button layout for platforms/desktops that don't report one via
/// `cx.button_layout()` (Windows always; a Linux DE without
/// `gtk-decoration-layout`): minimize/maximize/close, right-aligned. Doesn't
/// reuse `WindowButtonLayout::linux_default()` because that constructor is
/// `#[cfg(any(target_os = "linux", target_os = "freebsd"))]`-gated in gpui —
/// this needs to compile on Windows too.
fn or_default_layout(layout: Option<WindowButtonLayout>) -> WindowButtonLayout {
    layout.unwrap_or(WindowButtonLayout {
        left: [None, None, None],
        right: [
            Some(WindowButton::Minimize),
            Some(WindowButton::Maximize),
            Some(WindowButton::Close),
        ],
    })
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib panels::header`
Expected: 3 passed (`maximize_icon_switches_on_maximized_state`,
`or_default_layout_passes_through_a_reported_layout`,
`or_default_layout_falls_back_to_right_aligned_min_max_close`).

- [ ] **Step 7: Full build check (the new imports include some not yet used by anything — confirm no unused-import warnings slipped through)**

Run: `cargo build`
Expected: clean (the added imports — `InteractiveElement`, `MouseButton`, `MouseDownEvent`, `Window` — aren't used yet until Task 3, so this step will actually show unused-import warnings right now. That's expected and temporary: Task 3 consumes `InteractiveElement`/`MouseButton`/`MouseDownEvent`/`Window`. If you want a clean build at every intermediate step, do Task 1 and Task 3 in the same sitting before running `cargo build` — but each task's own steps above are still correct in isolation.)

- [ ] **Step 8: Commit**

```bash
git add src/panels/icons.rs src/panels/header.rs
git commit -m "$(cat <<'EOF'
feat: add window-control icons and title-bar pure helpers

AppIcon gains WindowMinimize/WindowMaximize/WindowRestore/WindowClose
(mapped to IconName variants already shipped in gpui-component-assets —
no new SVGs). header.rs gains two pure, unit-tested helpers used by the
upcoming title-bar work: maximize_icon (which glyph to show) and
or_default_layout (Windows/DE-less fallback button order, not reusing
WindowButtonLayout::linux_default() since that's Linux/freebsd-only).

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Request client-side decorations + transparent titlebar in `main.rs`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: nothing new from other tasks.
- Produces: a main window opened with `window_decorations: Client` and
  `titlebar.appears_transparent: true` — this is what turns
  `gpui_component::Root`'s already-wired `window_border()` from a no-op into
  real shadow/resize behavior on Linux, and hides the native titlebar on
  Windows/macOS.

- [ ] **Step 1: Update the `gpui` import list**

In `src/main.rs`, change:

```rust
use gpui::{App, AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, actions, px, size};
```

to:

```rust
use gpui::{
    App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowDecorations,
    WindowOptions, actions, point, px, size,
};
```

- [ ] **Step 2: Add `titlebar` and `window_decorations` to the main window's `WindowOptions`**

Change:

```rust
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        let main_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: true,
                    ..Default::default()
                },
```

to:

```rust
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        let main_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: true,
                    // Hides the native titlebar (macOS/Windows) so header.rs's
                    // own 40px row is the only title bar the user sees.
                    // `traffic_light_position` only affects macOS (ignored
                    // elsewhere); it's positioned to land inside that row.
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    // Requests client-side decorations on Linux/Wayland (per
                    // that field's own doc comment: "Wayland only... may be
                    // ignored" elsewhere). This is the sole trigger for
                    // gpui_component::Root's window_border() to switch from a
                    // no-op passthrough to real shadow + 8-direction
                    // resize-edge hit-testing — see header.rs/this plan's
                    // design doc for why no new resize code is needed.
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                },
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 4: Manual check (Linux, this session)**

Run: `cargo run`
Expected: the window opens with no native titlebar/border — Wayland compositor should now be requested to skip its own decoration. (At this point in the plan there's no custom title bar row content yet beyond what already existed, so the window will look "correct minus controls" — that's expected; Tasks 3-4 add the row content back.) Close the app.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "$(cat <<'EOF'
feat: request client-side decorations for the main window

Hides the native titlebar (transparent + no title text) and requests
Wayland/X11 client-side decorations. This alone flips
gpui_component::Root's already-wired window_border() from a no-op into
real shadow + resize-edge behavior — no new resize code needed. The
custom title bar content that replaces the native chrome is added in
the next two commits.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Draggable title region (window-move + double-click-to-maximize)

**Files:**
- Modify: `src/panels/header.rs`

**Interfaces:**
- Consumes: nothing from other tasks (uses only `gpui::Window` primitives
  already imported in Task 1's step 5).
- Produces: the centered title area now moves the window on drag and
  toggles maximize on double-click — no new public interface for other
  tasks to consume.

- [ ] **Step 1: Add the shared mouse-down handler**

In `src/panels/header.rs`, add this function next to the two helpers from
Task 1 (after `or_default_layout`):

```rust
/// Mouse-down handler for the title bar's draggable region (the centered
/// title, and the empty space around it): a single click starts an
/// interactive window move; a double-click toggles maximize instead.
/// `titlebar_double_click` only does anything on macOS — it respects
/// whatever the user's actual System Settings double-click action is there
/// (confirmed: only `gpui_macos` overrides the shared no-op default) — so
/// Linux/Windows get an explicit `zoom_window()` call instead.
fn drag_or_maximize(ev: &MouseDownEvent, window: &mut Window, _cx: &mut App) {
    if ev.click_count == 2 {
        if cfg!(target_os = "macos") {
            window.titlebar_double_click();
        } else {
            window.zoom_window();
        }
    } else {
        window.start_window_move();
    }
}
```

- [ ] **Step 2: Wire it onto the centered-title child**

In `render_header`, change:

```rust
        // Centered active title
        .child(
            div()
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(active_title),
        )
```

to:

```rust
        // Centered active title — also the draggable region: this flex_1
        // area is mostly empty space around the centered text, so a
        // mouse-down anywhere in it reads as "drag the window" (or
        // "double-click to maximize"), matching Zed/VS Code convention. The
        // brand+menu cluster above and the window-controls cluster added in
        // Task 4 are deliberately NOT covered by this handler, so their own
        // clicks/dropdowns are unaffected.
        .child(
            div()
                .id("header-drag-region")
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .on_mouse_down(MouseButton::Left, drag_or_maximize)
                .child(active_title),
        )
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds, no unused-import warnings now (`InteractiveElement`,
`MouseButton`, `MouseDownEvent`, `Window` are all used by this task).

- [ ] **Step 4: Manual check (Linux, this session)**

Run: `cargo run`
Expected:
- Click-and-drag anywhere in the centered title area moves the window.
- Double-click in that area toggles maximize/restore.
- The 文件/视图/终端/帮助 menu buttons still open their dropdowns normally
  (unaffected — they're outside the draggable region).

Close the app.

- [ ] **Step 5: Commit**

```bash
git add src/panels/header.rs
git commit -m "$(cat <<'EOF'
feat: make the header's centered title area drag-to-move the window

Single click-drag starts an interactive window move; double-click
toggles maximize. Only the centered-title flex_1 region (mostly empty
space) gets the handler, so the menu buttons are unaffected.
titlebar_double_click() is a no-op outside macOS (confirmed against the
vendored gpui_linux/gpui_windows backends), so non-macOS explicitly
calls zoom_window() on double-click instead.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Window-control button cluster (Linux/Windows) + macOS traffic-light spacing

**Files:**
- Modify: `src/panels/header.rs`
- Modify: `src/workspace.rs`

**Interfaces:**
- Consumes: `AppIcon::WindowMinimize/WindowMaximize/WindowRestore/WindowClose`
  and `maximize_icon`/`or_default_layout` from Task 1.
- Produces: `render_header` now takes an extra `window: &Window` parameter —
  its only caller (`Workspace::render`) is updated in this same task.

- [ ] **Step 1: Add the per-button and per-side render helpers**

In `src/panels/header.rs`, add after `drag_or_maximize`:

```rust
/// One minimize/maximize/close button. macOS never renders this (native
/// traffic-light buttons replace it — see `render_header`'s `is_macos`
/// branch), so this only needs to look right for Linux/Windows conventions.
fn window_control_button(button: WindowButton, is_maximized: bool, cx: &App) -> impl IntoElement {
    let (icon_name, is_close) = match button {
        WindowButton::Minimize => (AppIcon::WindowMinimize, false),
        WindowButton::Maximize => (maximize_icon(is_maximized), false),
        WindowButton::Close => (AppIcon::WindowClose, true),
    };

    div()
        .id(button.id())
        .flex()
        .items_center()
        .justify_center()
        .w(px(46.0))
        .h_full()
        .text_color(cx.theme().muted_foreground)
        .when(is_close, |d| {
            d.hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
        })
        .when(!is_close, |d| {
            d.hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
        })
        .child(icon(icon_name))
        .on_click(move |_ev, window, _cx| match button {
            WindowButton::Minimize => window.minimize_window(),
            WindowButton::Maximize => window.zoom_window(),
            WindowButton::Close => window.remove_window(),
        })
}

/// Renders one side (`layout.left` or `layout.right`) of the button layout —
/// most desktops only populate one side, but this handles both since
/// `cx.button_layout()` can report either.
fn window_control_side(
    slots: [Option<WindowButton>; gpui::MAX_BUTTONS_PER_SIDE],
    is_maximized: bool,
    cx: &App,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .children(
            slots
                .into_iter()
                .flatten()
                .map(|button| window_control_button(button, is_maximized, cx)),
        )
}
```

- [ ] **Step 2: Add the `prelude::FluentBuilder` and `StatefulInteractiveElement` imports**

`.when(...)` needs `FluentBuilder`; `div().id(...).on_click(...)` in
`window_control_button` needs `StatefulInteractiveElement` (confirmed via
`sessions.rs`'s existing `.on_click` usage on a `div` — it's not covered by
plain `InteractiveElement`). Change the `use gpui::{...}` line (from Task 1's
step 5) to:

```rust
use gpui::{
    App, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window, WindowButton,
    WindowButtonLayout, div, prelude::FluentBuilder, px,
};
```

- [ ] **Step 3: Give `render_header` a `window: &Window` parameter and build the control clusters**

Change the signature:

```rust
pub fn render_header(
    workspace: WeakEntity<Workspace>,
    active_title: SharedString,
    cx: &App,
) -> impl IntoElement + use<> {
```

to:

```rust
pub fn render_header(
    workspace: WeakEntity<Workspace>,
    active_title: SharedString,
    window: &Window,
    cx: &App,
) -> impl IntoElement + use<> {
```

At the top of the function body (right after the `let ws_file = ...` /
`let ws_terminal = ...` / `let ws_settings = ...` lines, before the menu
`Button::new(...)` calls), add:

```rust
    // macOS keeps its native traffic-light buttons (positioned via
    // `TitlebarOptions.traffic_light_position` in `main.rs`) instead of a
    // hand-drawn cluster — that's the platform convention, and gpui_macos
    // already implements the OS-level plumbing for it.
    let is_macos = cfg!(target_os = "macos");
    let window_controls = if is_macos {
        None
    } else {
        let layout = or_default_layout(cx.button_layout());
        let is_maximized = window.is_maximized();
        Some((
            window_control_side(layout.left, is_maximized, cx).into_any_element(),
            window_control_side(layout.right, is_maximized, cx).into_any_element(),
        ))
    };
    let (left_controls, right_controls) = match window_controls {
        Some((l, r)) => (Some(l), Some(r)),
        None => (None, None),
    };
```

- [ ] **Step 4: Attach the clusters (and macOS's reserved padding) to the row**

Change the row's own builder chain from:

```rust
    div()
        .h(px(40.0))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .bg(cx.theme().muted)
        .border_b_1()
        .border_color(cx.theme().border)
        // Brand + menu bar
        .child(
```

to:

```rust
    div()
        .h(px(40.0))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .bg(cx.theme().muted)
        .border_b_1()
        .border_color(cx.theme().border)
        // Reserve space for macOS's native traffic-light buttons (rendered
        // outside this element tree entirely — see `main.rs`'s
        // `traffic_light_position`), so the brand icon doesn't sit under them.
        .when(is_macos, |d| d.pl(px(78.0)))
        .children(left_controls)
        // Brand + menu bar
        .child(
```

And at the very end of the function, after the centered-title `.child(...)`
block from Task 3 (i.e. this becomes the new last line of the builder
chain, replacing the previous closing `)`):

```rust
                .child(active_title),
        )
        .children(right_controls)
}
```

- [ ] **Step 5: Update the call site in `workspace.rs`**

Change:

```rust
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
```

to:

```rust
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), window, cx);
```

(`Workspace::render`'s own `window: &mut Window` parameter is already in
scope here — passing it where `&Window` is expected reborrows implicitly.)

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: succeeds, no warnings.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test`
Expected: all passing, including the 3 new tests from Task 1.

- [ ] **Step 8: Manual check (Linux, this session)**

Run: `cargo run`
Expected:
- Minimize/maximize/close buttons appear on the right (GNOME's actual
  `gtk-decoration-layout`-reported order if this session's DE reports one,
  otherwise the min/max/close fallback).
- Clicking minimize actually minimizes; maximize toggles between maximized
  and restored (and the icon swaps between the maximize/restore glyphs);
  close quits the app.
- Resize from all 4 edges and all 4 corners still works (this is
  `window_border()`'s pre-existing resize-hit-zone logic from Task 2 — confirm
  it wasn't accidentally broken by the row changes).
- Drag the window to a screen edge to trigger a tiling/snap layout, then
  confirm the shadow disappears while tiled (per `window_border()`'s
  tiling-aware behavior) and reappears after un-snapping.
- 文件/视图/终端/帮助 menus still open normally.

Close the app.

- [ ] **Step 9: Commit**

```bash
git add src/panels/header.rs src/workspace.rs
git commit -m "$(cat <<'EOF'
feat: add minimize/maximize/close buttons to the header row

Linux/Windows get a hand-drawn button cluster (order/side from
cx.button_layout() when the desktop reports one, else a right-aligned
minimize/maximize/close fallback); macOS keeps native traffic-light
buttons instead (just reserves left padding so the brand icon doesn't
sit under them). render_header now takes a `window: &Window` parameter
to read is_maximized()/button_layout() while building the row.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Full manual verification pass

**Files:** none (verification only; fixes for anything found go here before
the final commit).

- [ ] **Step 1: Cold-boot check**

Run: `cargo run` from a fresh process (not reusing a stale running instance —
this repo's history has already hit that mistake once this session).
Confirm the window opens with no native border/titlebar from the very first
frame (not just after some interaction).

- [ ] **Step 2: Re-run the full drag/resize/button matrix from Task 4 Step 8**

Repeat all of Task 4 Step 8's checks once more end-to-end in one sitting
(catches any regression introduced between Task 4 and now — there shouldn't
be any, since Task 5 has no code changes, but this is the final gate before
calling the feature done).

- [ ] **Step 3: Multi-window check**

Open Settings (文件 → 设置...) and the new-connection window (会话 panel's
"+"). Confirm both still show native OS decorations (unchanged — only the
main window opts into client-side decorations) and are unaffected by any of
the above.

- [ ] **Step 4: `cargo build` and `cargo test` one last time**

Run: `cargo build && cargo test`
Expected: clean build, all tests passing.

- [ ] **Step 5: Report results to the user**

Summarize what was manually verified on Linux, and reiterate that
Windows/macOS are compile-verified only (no toolchain available here) —
worth a manual smoke test on those platforms before considering this fully
shipped.
