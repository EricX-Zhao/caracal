# Workspace-owned center tabs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Workspace.tab_panels` the only strong owner of center terminal tabs, replacing `DockArea` with a Workspace-rendered strip so close actually drops the PTY/serial handle.

**Architecture:** Pure index helpers and the strip UI live in `src/panels/center_tabs.rs`. `Workspace` keeps `tab_panels` plus `active_tab: Option<usize>`, renders the strip + the active `TerminalPanel`, and closes via `close_tab` (no `TabPanel::remove_panel`). `TerminalPanel` stays the gpui-component adapter for scrollbar and copy/paste menu, but stops implementing `dock::Panel`. Split-by-edge-drop is deleted, not reimplemented.

**Tech Stack:** Rust, GPUI (`div`, `on_drag`/`on_drop`, `actions!`), gpui-component (`ActiveTheme`, `Scrollbar`, `ContextMenuExt` — not `DockArea`), rust-i18n.

**Spec:** `docs/superpowers/specs/2026-08-24-workspace-center-tabs-design.md`

## Global Constraints

- CLAUDE.md §1: `src/terminal/view.rs` stays free of `gpui_component` imports. Copy/paste menu and scrollbar stay on `TerminalPanel`.
- CLAUDE.md §2: one SSH host = one `SshSession`. This plan does not change `ssh_sessions` / reconnect.
- Do not remove the `gpui-component` crate. Do not reimplement drag-to-split.
- `"N-"` titles stay live position in `tab_panels` (`renumber_tabs`), matching `2026-07-22-tab-sequence-numbers-design.md`.
- `secondary-shift-w` remains the close-tab shortcut; only the Action type changes (`CloseTab`, not `ClosePanel`).
- Empty center shows `Terminal.empty_tabs`; opening a tab from Sessions / New Tab still works.
- Serial port lock must drop on that tab's close, not on the next tab open.

## File map

| File | Responsibility |
|------|----------------|
| Create: `src/panels/center_tabs.rs` | `active_index_after_close`, `reorder_indices`, `DragTab`, `render_center` |
| Modify: `src/panels/mod.rs` | `pub mod center_tabs` |
| Modify: `src/workspace.rs` | Own `active_tab`; `close_tab`; render via `center_tabs`; delete dock |
| Modify: `src/panels/terminal.rs` | Drop `Panel`/`TabPanel`/false-close; close via workspace |
| Modify: `src/workspace.rs` `gpui::actions!` | Add `CloseTab` |
| Modify: `src/panels/keybindings.rs` | Bind `"close_tab"` to `CloseTab` |
| Modify: `locales/app.yml` | `Terminal.empty_tabs` |

---

### Task 1: Tab-list index helpers

**Files:**
- Create: `src/panels/center_tabs.rs`
- Modify: `src/panels/mod.rs`
- Test: `src/panels/center_tabs.rs` (`#[cfg(test)]` module in the same file)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn active_index_after_close(closed: usize, active: usize, len: usize) -> Option<usize>`
  - `pub fn reorder_indices(from: usize, to: usize, len: usize) -> Vec<usize>`
  - `pub mod center_tabs` from `src/panels/mod.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/panels/center_tabs.rs` with only the test module and the two function signatures if needed so the file parses — do **not** implement the real logic yet. Put this at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::{active_index_after_close, reorder_indices};

    #[test]
    fn close_left_of_active_decrements() {
        assert_eq!(active_index_after_close(0, 2, 3), Some(1));
    }

    #[test]
    fn close_active_middle_keeps_slot() {
        // tabs [0,1,2], active 1, close 1 → new list [0,2], active stays index 1
        assert_eq!(active_index_after_close(1, 1, 3), Some(1));
    }

    #[test]
    fn close_last_while_active_lands_on_new_last() {
        assert_eq!(active_index_after_close(2, 2, 3), Some(1));
    }

    #[test]
    fn close_right_of_active_leaves_active() {
        assert_eq!(active_index_after_close(2, 0, 3), Some(0));
    }

    #[test]
    fn close_only_tab_yields_none() {
        assert_eq!(active_index_after_close(0, 0, 1), None);
    }

    #[test]
    fn close_out_of_range_or_empty_yields_none() {
        assert_eq!(active_index_after_close(0, 0, 0), None);
        assert_eq!(active_index_after_close(5, 0, 2), None);
    }

    #[test]
    fn reorder_move_right() {
        assert_eq!(reorder_indices(0, 2, 3), vec![1, 2, 0]);
    }

    #[test]
    fn reorder_move_left() {
        assert_eq!(reorder_indices(2, 0, 3), vec![2, 0, 1]);
    }

    #[test]
    fn reorder_no_op_same_index() {
        assert_eq!(reorder_indices(1, 1, 3), vec![0, 1, 2]);
    }

    #[test]
    fn reorder_out_of_range_is_identity() {
        assert_eq!(reorder_indices(9, 0, 3), vec![0, 1, 2]);
        assert_eq!(reorder_indices(0, 9, 3), vec![0, 1, 2]);
    }
}
```

At the top of the same file, declare empty stubs so the tests compile and fail on the assertions (or do not compile until the signatures exist — prefer signatures that `unimplemented!()`):

```rust
//! Center terminal tab strip: the list math and the GPUI strip.
//! `Workspace` is the only strong owner of `TerminalPanel`s; this module
//! does not hold entities.

pub fn active_index_after_close(_closed: usize, _active: usize, _len: usize) -> Option<usize> {
    unimplemented!("Task 1")
}

pub fn reorder_indices(_from: usize, _to: usize, _len: usize) -> Vec<usize> {
    unimplemented!("Task 1")
}
```

Add `pub mod center_tabs;` to `src/panels/mod.rs` next to `pub mod terminal;`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --offline --bin caracal panels::center_tabs::tests -- --nocapture`

Expected: FAIL (panic on `unimplemented!`, or assertion failure if you return dummy values). Not a compile error about a missing module.

- [ ] **Step 3: Implement the helpers**

Replace the stubs with:

```rust
/// After closing the tab at `closed` in a list of `len` tabs whose current
/// active index is `active`, the new active index — or `None` if the list
/// is now empty or the close is invalid.
pub fn active_index_after_close(closed: usize, active: usize, len: usize) -> Option<usize> {
    if len == 0 || closed >= len {
        return None;
    }
    let new_len = len - 1;
    if new_len == 0 {
        return None;
    }
    let mut a = active.min(len - 1);
    if closed < a {
        a -= 1;
    } else if closed == a && a >= new_len {
        a = new_len - 1;
    }
    Some(a)
}

/// Permutation of `0..len` after moving the item at `from` to index `to`
/// (the index in the list *after* removal). Identity if either index is
/// out of range or `from == to`.
pub fn reorder_indices(from: usize, to: usize, len: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..len).collect();
    if from >= len || to >= len || from == to {
        return v;
    }
    let item = v.remove(from);
    let insert_at = to.min(v.len());
    v.insert(insert_at, item);
    v
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --offline --bin caracal panels::center_tabs::tests`

Expected: 11 passed.

- [ ] **Step 5: Commit**

```bash
git add src/panels/center_tabs.rs src/panels/mod.rs
git commit -m "$(cat <<'EOF'
feat: add center-tab index helpers

Workspace will own the terminal tab list; these pure functions decide
the new active index on close and the permutation on drag-reorder.
EOF
)"
```

---

### Task 2: `CloseTab` action and keymap (still no UI change)

**Files:**
- Modify: `src/workspace.rs` (the `gpui::actions!` list near line 66)
- Modify: `src/panels/keybindings.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `workspace::CloseTab` action. `keybinding_for("close_tab", key)` returns `KeyBinding::new(key, CloseTab, None)`.

- [ ] **Step 1: Add the action**

In `src/workspace.rs`, the existing list is:

```rust
gpui::actions!(caracal_workspace, [
    NewTab,
    NextTab,
    PrevTab,
    ...
    ZoomOut,
]);
```

Insert `CloseTab,` immediately after `PrevTab,`.

- [ ] **Step 2: Point the shortcut at it**

In `src/panels/keybindings.rs`:

- Change `use gpui_component::dock::ClosePanel;` to `use crate::workspace::CloseTab;`
- In `keybinding_for`, change `"close_tab" => KeyBinding::new(key, ClosePanel, None)` to `"close_tab" => KeyBinding::new(key, CloseTab, None)`.

Until Task 4, `CloseTab` is unbound on the Workspace render (no `on_action`). The old `ClosePanel` dock binding is gone, so **close-tab by keyboard will not work until Task 4**. That is expected and called out in Task 4's handler. Do not leave `ClosePanel` as a fallback.

- [ ] **Step 3: Compile**

Run: `cargo test --offline --bin caracal panels::keybindings -- --nocapture`

If that module has no tests, run: `cargo check --offline --bin caracal`

Expected: success. `ClosePanel` must not appear anywhere under `src/`.

- [ ] **Step 4: Commit**

```bash
git add src/workspace.rs src/panels/keybindings.rs
git commit -m "$(cat <<'EOF'
feat: bind close-tab to Workspace CloseTab

Stop using gpui-component's dock ClosePanel so the shortcut can close
a Workspace-owned tab in the next cutover.
EOF
)"
```

---

### Task 3: Strip UI + empty placeholder (render-only)

**Files:**
- Modify: `src/panels/center_tabs.rs`
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `active_index_after_close` / `reorder_indices` from Task 1 (not called from render yet).
- Produces:
  - `pub struct DragTab { pub ix: usize }` (`#[derive(Clone)]`)
  - `pub fn tab_label(tab_number: u32, title: &str) -> String` — `format!("{tab_number}-{title}")`
  - Locale key `Terminal.empty_tabs` (`zh-CN`: `"没有打开的终端"`, `en`: `"No open terminals"`)

This task does **not** wire the strip into `Workspace::render_body`. It only adds render helpers and a label helper with tests so Task 4 can call them.

- [ ] **Step 1: Write the failing label test**

In `src/panels/center_tabs.rs` tests, add:

```rust
    #[test]
    fn tab_label_prefixes_live_number() {
        assert_eq!(tab_label(1, "本地终端"), "1-本地终端");
        assert_eq!(tab_label(3, "prod:2"), "3-prod:2");
    }
```

Do not implement `tab_label` yet.

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo test --offline --bin caracal panels::center_tabs::tests::tab_label_prefixes_live_number`

Expected: compile error, `cannot find function tab_label`.

- [ ] **Step 3: Implement `tab_label` and `DragTab`**

```rust
#[derive(Clone)]
pub struct DragTab {
    pub ix: usize,
}

pub fn tab_label(tab_number: u32, title: &str) -> String {
    format!("{tab_number}-{title}")
}
```

Add to `locales/app.yml` under `Terminal:`, after `paste:`:

```yaml
  empty_tabs:
    zh-CN: "没有打开的终端"
    en: "No open terminals"
```

- [ ] **Step 4: Run tests**

Run: `cargo test --offline --bin caracal panels::center_tabs::tests`

Expected: all previous tests plus `tab_label_prefixes_live_number` pass.

- [ ] **Step 5: Commit**

```bash
git add src/panels/center_tabs.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: add center-tab label helper and empty-state copy

The strip UI in the next task renders tab_label and Terminal.empty_tabs.
EOF
)"
```

---

### Task 4: Cut over Workspace off `DockArea`

This is the behavior change. After this task, `cargo check` must succeed and there must be no `DockArea` in `workspace.rs`.

**Files:**
- Modify: `src/workspace.rs`
- Modify: `src/panels/center_tabs.rs` (add `render_tab_strip` / `render_empty_center`)
- Modify: `src/panels/terminal.rs`

**Interfaces:**
- Consumes: Task 1 helpers, Task 2 `CloseTab`, Task 3 `tab_label` / `DragTab` / `Terminal.empty_tabs`.
- Produces:
  - `Workspace.active_tab: Option<usize>`
  - `Workspace::close_tab(&mut self, panel: Entity<TerminalPanel>, window: &mut Window, cx: &mut Context<Self>)`
  - `Workspace::set_active_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>)`
  - Center of `render_body` is the strip + active panel, not `self.dock_area`

- [ ] **Step 1: Add strip render functions**

In `src/panels/center_tabs.rs` add the GPUI render helpers. They take data, not a `Workspace` borrow, so `workspace.rs` stays the owner.

Required signatures:

```rust
use gpui::{
    Div, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::ActiveTheme;

use crate::panels::terminal::TerminalPanel;

/// Horizontal chip strip. `on_select` / `on_close` / drop handling are
/// wired by the caller via the returned element's listeners — this helper
/// only builds one chip so Workspace can attach `cx.listener`s.
pub fn render_tab_chip(
    ix: usize,
    label: String,
    is_active: bool,
    cx: &mut gpui::Context<crate::workspace::Workspace>,
) -> Div {
    let bg = if is_active {
        cx.theme().list_active
    } else {
        gpui::transparent_black()
    };
    div()
        .id(("center-tab", ix))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(bg)
        .text_sm()
        .hover(|s| s.bg(cx.theme().list_hover))
        .child(div().child(label))
}

pub fn render_empty_center(cx: &gpui::App) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .child(rust_i18n::t!("Terminal.empty_tabs").to_string())
}
```

Use `cx.theme().list_active` when active, `gpui::transparent_black()` when not (same pair as `security_auth.rs`), and `.hover(|s| s.bg(cx.theme().list_hover))` (same as `sessions.rs`). Do not invent theme fields.

`render_tab_chip` returns a `Div` **without** click/drag handlers so `Workspace` can attach `cx.listener` with the right `this`.

- [ ] **Step 2: Replace Workspace fields**

In `pub struct Workspace`:

- Delete `dock_area: Entity<DockArea>`.
- Delete `tab_count: usize`.
- Add `active_tab: Option<usize>,` next to `tab_panels`.
- Add `tab_drag_target: Option<(usize, bool)>,` — `Some((hover_ix, insert_before))` while a `DragTab` is over a chip, same role as `SessionsPanel.drag_reorder_target`.

In `Workspace::new`, delete the `DockArea::new(...)` block. Initialize `active_tab: None`, `tab_drag_target: None`. Remove unused imports: `DockArea`, `DockItem`, `DockPlacement`, `PanelStyle`, `Axis` if it becomes unused.

- [ ] **Step 3: Replace `add_center` / tab switching**

Delete `add_center`, `prune_stale_center`, `center_tab_group_is_stale`, `active_tabs_item`.

Replace `set_active_tab_index` with:

```rust
fn set_active_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if ix >= self.tab_panels.len() {
        return;
    }
    self.active_tab = Some(ix);
    let panel = self.tab_panels[ix].clone();
    panel.read(cx).focus_handle(cx).focus(window, cx);
    let term = panel.read(cx).terminal(); // add a pub(crate) getter if needed
    self.set_active_title_from(&term.downgrade(), cx);
    cx.notify();
}
```

`TerminalPanel` does not currently expose `terminal`. Add:

```rust
pub(crate) fn terminal(&self) -> Entity<TerminalView> {
    self.terminal.clone()
}
```

in `src/panels/terminal.rs` next to `set_tab_number`.

`on_next_tab` / `on_prev_tab` / `goto_tab` use `self.tab_panels.len()` instead of `self.tab_count`, and `self.active_tab.unwrap_or(0)` as `current`.

Every `open_local*` / `open_ssh` / `open_telnet` / `open_serial` currently does:

```rust
self._subscriptions.push(tab_count_sub);
self.register_tab_panel(panel.clone(), cx);
self.add_center(Arc::new(panel), window, cx);
```

Change each to:

```rust
self.register_tab_panel(panel.clone(), cx);
self.set_active_tab(self.tab_panels.len() - 1, window, cx);
```

Delete the `subscribe_in(..., TerminalPanelEvent::Closed)` blocks that only did `tab_count -= 1` + `unregister_tab_panel`. SSH-specific close cleanup moves into `close_tab` (next step). For SSH opens, `handle_ssh_tab_closed` must still run on close — do **not** drop that logic, just stop going through the event.

- [ ] **Step 4: Implement `close_tab`**

```rust
pub(crate) fn close_tab(
    &mut self,
    panel: Entity<TerminalPanel>,
    window: &mut Window,
    cx: &mut Context<Self>,
) {
    let Some(closed) = self
        .tab_panels
        .iter()
        .position(|p| p.entity_id() == panel.entity_id())
    else {
        return;
    };
    let term = panel.read(cx).terminal();
    // Today's SSH `Closed` subscriber also calls `release_ssh_tab_number`.
    // Keep that pairing: `open_ssh` already does
    // `ssh_reconnect_configs.insert(terminal.entity_id(), config)` and
    // `allocate_ssh_tab_number`. Add `ssh_tab_n_by_term: HashMap<EntityId, (String, u32)>`
    // populated next to that insert (`key`, allocated n) and:
    if let Some(config) = self.ssh_reconnect_configs.get(&term.entity_id()).cloned() {
        self.handle_ssh_tab_closed(config, &term.downgrade(), window, cx);
    }
    if let Some((key, n)) = self.ssh_tab_n_by_term.remove(&term.entity_id()) {
        self.release_ssh_tab_number(&key, n);
    }
    let prev_active = self.active_tab.unwrap_or(0);
    let prev_len = self.tab_panels.len();
    self.unregister_tab_panel(&panel, window, cx);
    self.active_tab = crate::panels::center_tabs::active_index_after_close(
        closed,
        prev_active,
        prev_len,
    );
    if let Some(ix) = self.active_tab {
        self.set_active_tab(ix, window, cx);
    } else {
        self.active_title = "Caracal".into();
        self.focused_terminal = None;
        cx.notify();
    }
}
```

Adjust `handle_ssh_tab_closed`'s current signature (it likely takes `Entity<TerminalPanel>` or `WeakEntity<TerminalView>` — read the existing function and pass what it already expects; do not invent a new helper). `unregister_tab_panel` must **stop calling** `prune_stale_center` (that function is gone). After this, `unregister_tab_panel` only retains/removes from `tab_panels` and `renumber_tabs`.

`on_close_tab` handler:

```rust
fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
    let Some(ix) = self.active_tab else {
        return;
    };
    let panel = self.tab_panels[ix].clone();
    self.close_tab(panel, window, cx);
}
```

On `impl Render for Workspace`, add `.on_action(cx.listener(Self::on_close_tab))` next to the other tab actions.

`TerminalPanel::close` — replace the `TabPanel::remove_panel` body with:

```rust
fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(workspace) = self.workspace.upgrade() else {
        return;
    };
    let panel = cx.entity();
    window.defer(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.close_tab(panel, window, cx);
        });
    });
}
```

- [ ] **Step 5: Delete dock `Panel` machinery on `TerminalPanel`**

In `src/panels/terminal.rs` remove:

- fields `tab_panel`, `attached`
- `impl Panel for TerminalPanel` (`panel_name`, `zoomable`, `title`, `on_added_to`, `on_removed`)
- imports `Panel`, `PanelControl`, `PanelEvent`, `PanelView`, `TabPanel`
- `impl EventEmitter<PanelEvent>` if nothing else needs it
- `impl EventEmitter<TerminalPanelEvent>` **if** no remaining subscriber; if `Closed` is unused after Step 4, delete the enum too

Keep `impl Render`, `impl Focusable`, scrollbar, context menu, `tab_number` / `set_tab_number`.

The `"N-"` prefix + X move to the strip (Workspace render). `TerminalPanel::render` is only the terminal + scrollbar. The X on the chip calls `close` via Workspace listener, not via `Panel::title`.

- [ ] **Step 6: Render the center**

In `render_body`, replace `.child(self.dock_area.clone())` with a call that builds:

```rust
let strip = {
    let mut row = div().flex().flex_row().items_center().gap_1().px_1().py_1().w_full();
    for (ix, panel) in self.tab_panels.iter().enumerate() {
        let is_active = self.active_tab == Some(ix);
        let number = panel.read(cx).tab_number(); // add pub(crate) getter
        let title = panel.read(cx).terminal().read(cx).title().to_string();
        let label = crate::panels::center_tabs::tab_label(number, &title);
        let chip = crate::panels::center_tabs::render_tab_chip(ix, label, is_active, cx)
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.set_active_tab(ix, window, cx);
            }))
            .on_drag(crate::panels::center_tabs::DragTab { ix }, |drag, _, _, cx| {
                cx.new(|_| drag.clone())
            })
            .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<crate::panels::center_tabs::DragTab>, _, _cx| {
                if event.bounds.contains(&event.event.position) {
                    let insert_before = event.event.position.x < event.bounds.center().x;
                    this.tab_drag_target = Some((ix, insert_before));
                } else if this.tab_drag_target.is_some_and(|(t, _)| t == ix) {
                    this.tab_drag_target = None;
                }
            }))
            .on_drop(cx.listener(move |this, drag: &crate::panels::center_tabs::DragTab, window, cx| {
                let insert_before = this
                    .tab_drag_target
                    .filter(|(t, _)| *t == ix)
                    .map(|(_, b)| b)
                    .unwrap_or(true);
                this.tab_drag_target = None;
                let mut to = ix;
                if !insert_before {
                    to += 1;
                }
                // After removal of `drag.ix`, indices >= drag.ix shift down.
                let from = drag.ix;
                if to > from {
                    to -= 1;
                }
                to = to.min(this.tab_panels.len().saturating_sub(1));
                let panel = this.tab_panels[from].clone();
                this.reposition_tab_panel(panel, to, cx);
                this.set_active_tab(to, window, cx);
            }))
            .child(
                div()
                    .id(("close-center-tab", ix))
                    .rounded_sm()
                    .hover(|s| s.bg(cx.theme().danger))
                    .child(crate::panels::icons::icon(crate::panels::icons::AppIcon::Delete))
                    .on_click(cx.listener(move |this, ev, window, cx| {
                        cx.stop_propagation();
                        let panel = this.tab_panels[ix].clone();
                        this.close_tab(panel, window, cx);
                    })),
            );
        row = row.child(chip);
    }
    row
};

let body = if let Some(ix) = self.active_tab {
    div().size_full().child(self.tab_panels[ix].clone())
} else {
    crate::panels::center_tabs::render_empty_center(cx)
};
```

Add `pub(crate) fn tab_number(&self) -> u32 { self.tab_number }` on `TerminalPanel`.

`reposition_tab_panel` already reorders `tab_panels` and `renumber_tabs`. Keep it.

- [ ] **Step 7: Compile and unit-test**

Run:

```
cargo test --offline --bin caracal panels::center_tabs::tests
cargo test --offline --bin caracal workspace::tests
cargo check --offline --bin caracal
```

Expected: all pass; `src/workspace.rs` contains no `DockArea` / `prune_stale_center` / `tab_count`. `rg DockArea src/` is empty (side panels never used it). `rg ClosePanel src/` is empty.

If `handle_ssh_tab_closed` still takes a `window` and used dock APIs, strip those calls — it should only drop `ssh_sessions` / SFTP / monitor when the last tab for that host closes, same as today.

- [ ] **Step 8: Commit**

```bash
git add src/workspace.rs src/panels/terminal.rs src/panels/center_tabs.rs
git commit -m "$(cat <<'EOF'
feat: own center tabs in Workspace, drop DockArea

tab_panels is the only strong owner. Close drops the terminal backend
immediately; drag-reorder no longer goes through TabPanel::on_removed.
EOF
)"
```

---

### Task 5: Manual verification

No code. Native GPUI app (no browser). Run `cargo run --offline` and walk these. If any fail, fix in this task and amend or add a follow-up commit — do not mark the plan done with a known close/PTY leak.

- [ ] **Step 1: Open/close numbering**

Open three local tabs. Titles are `1-…`, `2-…`, `3-…`. `secondary-2` focuses the middle one. Close tab 1; remaining titles are `1-…`, `2-…` and `secondary-1` focuses the new first.

- [ ] **Step 2: Serial reopen (the leak this plan exists to fix)**

Open a serial tab first, then a local tab. Close the serial tab (local remains). Reopen the same serial port. Expected: connects. Must **not** be "device busy".

- [ ] **Step 3: Drag reorder**

Two or more tabs. Drag tab 3 to the left of tab 1. Numbers recompute to match visual order. For an SSH tab: SFTP still talks to that host after the drag (session was not torn down).

- [ ] **Step 4: Last tab and shortcut**

Close the last tab. Center shows "没有打开的终端" (or English if locale is en). `secondary-shift-t` opens a new local tab. `secondary-shift-w` on a tab closes it; with no tabs it does nothing.

- [ ] **Step 5: Split is gone**

Drag a tab to the far edge of the center. Expected: reorder only, never a second pane.

- [ ] **Step 6: Commit only if Step 1–5 required code fixes**

If verification found a bug and you fixed it:

```bash
git add -u
git commit -m "$(cat <<'EOF'
fix: center-tab close/reorder leftover from DockArea cutover
EOF
)"
```

If verification passed with no extra diffs, do not make an empty commit.
