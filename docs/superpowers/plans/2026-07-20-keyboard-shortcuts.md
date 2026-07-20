# Keyboard Shortcuts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the keyboard shortcuts from `docs/superpowers/specs/2026-07-20-keyboard-shortcuts-design.md` — tab management, new-connection, sidebar/panel toggling, and terminal font-zoom/clear-screen.

**Architecture:** New `gpui::actions!` types bound globally (`None` context, so they fire regardless of which view has focus — see the design spec's dispatch-order note) via `cx.bind_keys` in `main.rs`, handled by `on_action` listeners on `Workspace`'s root render element. Tab-close reuses gpui-component's existing `dock::ClosePanel` action. Tab next/prev/goto-N mutate the live `TabPanel` entity through `DockItem::active_index()` (a public gpui-component method) plus an explicit `cx.notify()` (that method doesn't call it itself — confirmed by reading its source). Clear-screen is terminal-local, bound only in `TERMINAL_KEY_CONTEXT`.

**Tech Stack:** Rust, `gpui`, `gpui-component` (dock module), `alacritty_terminal`.

## Global Constraints

- Every new binding uses gpui's `"secondary-x"` modifier alias (Cmd on macOS, Ctrl on Windows/Linux) — never hardcode `ctrl-`.
- No screenshot-driven GUI verification — manual keyboard testing only, described per task (this project's standing convention).
- No new user-visible strings are introduced by this feature (shortcuts have no new UI text), so no i18n/locale work is needed, with one exception: the new-tab fallback title reuses the existing `NewConnectionWindow.type_local` locale key rather than a new one.
- Match existing code style: pure/testable logic as small associated functions with `#[cfg(test)] mod tests` unit tests (see `Workspace::lowest_free_number` in `workspace.rs` for the precedent this plan follows) — don't unit-test trivial one-line field toggles that the existing codebase doesn't test either.

---

### Task 1: Tab-index navigation math

Pure, gpui-free logic for computing the next/previous/target tab index — no UI wiring yet. This is what Task 2's action handlers will call.

**Files:**
- Modify: `src/workspace.rs` (add near `lowest_free_number`, around line 570)
- Test: `src/workspace.rs`'s existing `#[cfg(test)] mod tests` block (around line 1633)

**Interfaces:**
- Produces: `Workspace::next_tab_index(current: usize, len: usize) -> usize`, `Workspace::prev_tab_index(current: usize, len: usize) -> usize`, `Workspace::goto_tab_index(target_one_indexed: usize, len: usize) -> Option<usize>` — all `pub(self)`/private associated functions on `Workspace`, consumed by Task 2's action handlers.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/workspace.rs` (after `lowest_free_number_reuses_a_gap`):

```rust
    #[test]
    fn next_tab_index_advances_by_one() {
        assert_eq!(Workspace::next_tab_index(0, 3), 1);
        assert_eq!(Workspace::next_tab_index(1, 3), 2);
    }

    #[test]
    fn next_tab_index_wraps_at_the_end() {
        assert_eq!(Workspace::next_tab_index(2, 3), 0);
    }

    #[test]
    fn next_tab_index_is_a_safe_no_op_with_no_tabs() {
        assert_eq!(Workspace::next_tab_index(0, 0), 0);
    }

    #[test]
    fn next_tab_index_stays_put_with_a_single_tab() {
        assert_eq!(Workspace::next_tab_index(0, 1), 0);
    }

    #[test]
    fn prev_tab_index_retreats_by_one() {
        assert_eq!(Workspace::prev_tab_index(2, 3), 1);
        assert_eq!(Workspace::prev_tab_index(1, 3), 0);
    }

    #[test]
    fn prev_tab_index_wraps_at_the_start() {
        assert_eq!(Workspace::prev_tab_index(0, 3), 2);
    }

    #[test]
    fn prev_tab_index_is_a_safe_no_op_with_no_tabs() {
        assert_eq!(Workspace::prev_tab_index(0, 0), 0);
    }

    #[test]
    fn goto_tab_index_converts_one_indexed_to_zero_indexed() {
        assert_eq!(Workspace::goto_tab_index(1, 3), Some(0));
        assert_eq!(Workspace::goto_tab_index(3, 3), Some(2));
    }

    #[test]
    fn goto_tab_index_rejects_out_of_range() {
        assert_eq!(Workspace::goto_tab_index(4, 3), None);
        assert_eq!(Workspace::goto_tab_index(0, 3), None);
        assert_eq!(Workspace::goto_tab_index(1, 0), None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib workspace::tests -- next_tab_index prev_tab_index goto_tab_index`
Expected: FAIL to compile — `next_tab_index`/`prev_tab_index`/`goto_tab_index` not found on `Workspace`.

- [ ] **Step 3: Implement the pure functions**

Add near `Workspace::lowest_free_number` (around line 570 of `src/workspace.rs`):

```rust
    /// The tab index one to the right of `current`, wrapping to `0` past the
    /// last tab. Returns `0` when `len == 0` (nothing to wrap over) — a safe
    /// no-op for the "no tabs open" case, same convention as
    /// `prev_tab_index`/`goto_tab_index` below.
    fn next_tab_index(current: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        (current + 1) % len
    }

    /// The tab index one to the left of `current`, wrapping to the last tab
    /// index past the first. Mirrors `next_tab_index`.
    fn prev_tab_index(current: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        if current == 0 { len - 1 } else { current - 1 }
    }

    /// Converts a 1-indexed `secondary-N` target (as pressed by the user)
    /// into a 0-indexed tab index, or `None` if `target_one_indexed` is `0`
    /// or beyond the current tab count.
    fn goto_tab_index(target_one_indexed: usize, len: usize) -> Option<usize> {
        if target_one_indexed == 0 || target_one_indexed > len {
            return None;
        }
        Some(target_one_indexed - 1)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib workspace::tests -- next_tab_index prev_tab_index goto_tab_index`
Expected: PASS (10 tests).

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: add pure tab-index navigation math"
```

---

### Task 2: Tab management shortcuts (new/close/next/prev/goto)

**Files:**
- Modify: `src/workspace.rs` — struct fields, `Workspace::new`, `add_center`, `open_local_with`/`open_telnet`/`open_serial`/`open_ssh`, new action handlers, `Render::render`'s `.on_action(...)` chain.
- Modify: `src/main.rs` — new `KeyBinding`s, new imports.

**Interfaces:**
- Consumes: `Workspace::next_tab_index`/`prev_tab_index`/`goto_tab_index` from Task 1. `gpui_component::dock::{ClosePanel, DockItem, TabPanel}` (already imported: `DockItem` at `workspace.rs:38`; `ClosePanel`/`TabPanel` are new imports for this task). `TerminalPanelEvent` from `src/panels/terminal.rs` (already imported at `workspace.rs:56`).
- Produces: new action types `NewTab, NextTab, PrevTab, GotoTab1..GotoTab9` (declared via `gpui::actions!` in `workspace.rs`) — Task 3 adds one more action to this same macro invocation. New `Workspace` fields `tab_count: usize`. New private methods `Workspace::active_tabs_item`, `Workspace::set_active_tab_index` — reused by no other task, but follow this exact naming if extending later.

**Why a `tab_count` field instead of reading gpui-component's own `DockArea.center()` tree for the count:** `DockArea.center()`'s cached `DockItem::Tabs { items, .. }` list is only ever appended to, never pruned when a tab closes (this is the exact bug `Workspace::center_tab_group_is_stale` already works around for the "every tab closed" case) — reading `items.len()` from it would silently over-count after any tab close. `TabPanel` has no public panel-count getter either (`panels: Vec<Arc<dyn PanelView>>` is `pub(crate)` inside gpui-component, `active_ix()` is public but a count isn't). So `Workspace` tracks its own live count instead, incremented at `add_center` (the single choke point every `open_*` method already funnels through) and decremented via `TerminalPanelEvent::Closed`, which fires for every tab type (confirmed in `panels/terminal.rs`'s `Panel::on_removed` — the doc comment there says local/Telnet/Serial tabs already emit this event today, just unobserved).

- [ ] **Step 1: Add the `tab_count` field and increment it in `add_center`**

In `src/workspace.rs`, add a field to the `Workspace` struct (near `terminal_views`, around line 244):

```rust
    /// Number of tabs currently open in the center dock's (single) tab
    /// group. Tracked here rather than read from gpui-component's own
    /// `DockArea.center()` tree, whose cached panel list is stale after any
    /// tab close (see `center_tab_group_is_stale`'s doc comment) — and
    /// `TabPanel` has no public panel-count getter either. Incremented in
    /// `add_center` (the single entry point every `open_*` method uses),
    /// decremented wherever a `TerminalPanelEvent::Closed` is observed
    /// below.
    tab_count: usize,
```

Initialize it in `Workspace::new`'s `Self { ... }` (near `terminal_views: Vec::new(),`, around line 472):

```rust
            tab_count: 0,
```

In `add_center` (around line 1150), add the increment as the first line of the method body:

```rust
    fn add_center(
        &mut self,
        panel: Arc<dyn gpui_component::dock::PanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab_count += 1;
        self.dock_area.update(cx, |dock_area, cx| {
```

- [ ] **Step 2: Decrement `tab_count` on every tab close**

In `open_ssh`'s existing `closed_sub` closure (around line 683), add the decrement alongside the existing cleanup:

```rust
        let closed_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
            this.release_ssh_tab_number(&closed_key, tab_number);
        });
```

In `open_local_with` (around line 559), `open_telnet` (around line 876), and `open_serial` (around line 896) — each currently reads:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
```

Change each of these three occurrences to:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, _panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
        });
        self._subscriptions.push(tab_count_sub);
        self.add_center(Arc::new(panel), window, cx);
```

- [ ] **Step 3: Declare the new action types**

Near the top of `src/workspace.rs`, after the `use` statements (around line 64), add:

```rust
gpui::actions!(caracal_workspace, [
    NewTab,
    NextTab,
    PrevTab,
    GotoTab1,
    GotoTab2,
    GotoTab3,
    GotoTab4,
    GotoTab5,
    GotoTab6,
    GotoTab7,
    GotoTab8,
    GotoTab9,
]);
```

Add the new imports this task needs, in `workspace.rs`'s existing `use gpui_component::dock::{...}` line (around line 38):

```rust
use gpui_component::dock::{DockArea, DockItem, DockPlacement, PanelStyle, TabPanel};
```

- [ ] **Step 4: Implement the tab-group lookup + active-index setter helpers**

Add these private methods to `Workspace` (near `add_center`, around line 1189, right after `center_tab_group_is_stale`):

```rust
    /// Find the (currently only ever one) `Tabs` group inside the center
    /// dock's tree — a `Split` wrapping a single `Tabs` item today (see
    /// `add_center`'s doc comment); this walk also handles a future nested
    /// `Split` without changes.
    fn active_tabs_item(&self, cx: &App) -> Option<DockItem> {
        fn find(item: &DockItem) -> Option<DockItem> {
            match item {
                DockItem::Tabs { .. } => Some(item.clone()),
                DockItem::Split { items, .. } => items.iter().find_map(find),
                _ => None,
            }
        }
        find(self.dock_area.read(cx).center())
    }

    /// Switch the center dock's active tab to `ix` (0-indexed). Uses
    /// `DockItem::active_index`, gpui-component's own public method for
    /// mutating the live `TabPanel` entity's active index — but that method
    /// doesn't call `cx.notify()` itself (confirmed by reading its body), so
    /// this does that explicitly afterward, or the tab switch wouldn't
    /// repaint until something unrelated triggered a redraw.
    fn set_active_tab_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(tabs_item) = self.active_tabs_item(cx) else {
            return;
        };
        let DockItem::Tabs { ref view, .. } = tabs_item else {
            return;
        };
        let view = view.clone();
        tabs_item.active_index(ix, cx);
        view.update(cx, |_, cx| cx.notify());
    }
```

- [ ] **Step 5: Implement the action handlers**

Add these to `Workspace` (near `set_active_tab_index`):

```rust
    /// `secondary-shift-t`: duplicate the focused tab's connection — a new
    /// shell on the same host if it's SSH, otherwise (or with nothing
    /// focused) a new local shell, same fallback `open_local_with` already
    /// uses elsewhere.
    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let fallback_title = rust_i18n::t!("NewConnectionWindow.type_local").to_string();
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            self.open_local_with(String::new(), String::new(), fallback_title, window, cx);
            return;
        };
        let Some(config) = self.ssh_reconnect_configs.get(&terminal.entity_id()).cloned() else {
            self.open_local_with(String::new(), String::new(), fallback_title, window, cx);
            return;
        };
        // SSH tab titles are always "{display_name}:{n}" (see `open_ssh`) —
        // recover the original display name by dropping the numeric suffix.
        let title = terminal.read(cx).title().to_string();
        let display_name = title.rsplit_once(':').map_or(title.clone(), |(name, _)| name.to_string());
        self.open_ssh(config, display_name, window, cx);
    }

    fn on_next_tab(&mut self, _: &NextTab, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(tabs_item) = self.active_tabs_item(cx) else {
            return;
        };
        let DockItem::Tabs { ref view, .. } = tabs_item else {
            return;
        };
        let current = view.read(cx).active_ix();
        let new_ix = Self::next_tab_index(current, self.tab_count);
        self.set_active_tab_index(new_ix, cx);
    }

    fn on_prev_tab(&mut self, _: &PrevTab, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(tabs_item) = self.active_tabs_item(cx) else {
            return;
        };
        let DockItem::Tabs { ref view, .. } = tabs_item else {
            return;
        };
        let current = view.read(cx).active_ix();
        let new_ix = Self::prev_tab_index(current, self.tab_count);
        self.set_active_tab_index(new_ix, cx);
    }

    /// Shared by the nine `on_goto_tab_N` handlers below.
    fn goto_tab(&mut self, one_indexed: usize, cx: &mut Context<Self>) {
        let Some(ix) = Self::goto_tab_index(one_indexed, self.tab_count) else {
            return;
        };
        self.set_active_tab_index(ix, cx);
    }

    fn on_goto_tab_1(&mut self, _: &GotoTab1, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(1, cx);
    }
    fn on_goto_tab_2(&mut self, _: &GotoTab2, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(2, cx);
    }
    fn on_goto_tab_3(&mut self, _: &GotoTab3, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(3, cx);
    }
    fn on_goto_tab_4(&mut self, _: &GotoTab4, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(4, cx);
    }
    fn on_goto_tab_5(&mut self, _: &GotoTab5, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(5, cx);
    }
    fn on_goto_tab_6(&mut self, _: &GotoTab6, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(6, cx);
    }
    fn on_goto_tab_7(&mut self, _: &GotoTab7, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(7, cx);
    }
    fn on_goto_tab_8(&mut self, _: &GotoTab8, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(8, cx);
    }
    fn on_goto_tab_9(&mut self, _: &GotoTab9, _window: &mut Window, cx: &mut Context<Self>) {
        self.goto_tab(9, cx);
    }
```

- [ ] **Step 6: Wire the handlers into `Render for Workspace`**

In `src/workspace.rs`'s `Render for Workspace` (around line 1601), extend the chained calls on the outer `div()`, right after `.font(chrome_font)`:

```rust
            .font(chrome_font)
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_goto_tab_1))
            .on_action(cx.listener(Self::on_goto_tab_2))
            .on_action(cx.listener(Self::on_goto_tab_3))
            .on_action(cx.listener(Self::on_goto_tab_4))
            .on_action(cx.listener(Self::on_goto_tab_5))
            .on_action(cx.listener(Self::on_goto_tab_6))
            .on_action(cx.listener(Self::on_goto_tab_7))
            .on_action(cx.listener(Self::on_goto_tab_8))
            .on_action(cx.listener(Self::on_goto_tab_9))
```

- [ ] **Step 7: Bind the keys in `main.rs`**

In `src/main.rs`, add to the imports (near line 39):

```rust
use gpui_component::dock::ClosePanel;
use workspace::{
    GotoTab1, GotoTab2, GotoTab3, GotoTab4, GotoTab5, GotoTab6, GotoTab7, GotoTab8, GotoTab9,
    NewTab, NextTab, PrevTab, Workspace,
};
```

(`Workspace` itself is already imported via `use workspace::Workspace;` at line 40 — replace that whole line with the block above so `Workspace` isn't imported twice.)

Extend the existing `cx.bind_keys([...])` array (around line 153) with the new entries:

```rust
        cx.bind_keys([
            KeyBinding::new("ctrl-c", Interrupt, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", SendBackTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("secondary-shift-t", NewTab, None),
            KeyBinding::new("secondary-shift-w", ClosePanel, None),
            KeyBinding::new("secondary-tab", NextTab, None),
            KeyBinding::new("secondary-shift-tab", PrevTab, None),
            KeyBinding::new("secondary-1", GotoTab1, None),
            KeyBinding::new("secondary-2", GotoTab2, None),
            KeyBinding::new("secondary-3", GotoTab3, None),
            KeyBinding::new("secondary-4", GotoTab4, None),
            KeyBinding::new("secondary-5", GotoTab5, None),
            KeyBinding::new("secondary-6", GotoTab6, None),
            KeyBinding::new("secondary-7", GotoTab7, None),
            KeyBinding::new("secondary-8", GotoTab8, None),
            KeyBinding::new("secondary-9", GotoTab9, None),
        ]);
```

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: builds cleanly (warnings only if any, no errors).

- [ ] **Step 9: Manual test**

Run: `cargo run`. With at least 3 tabs open (mix of local + SSH if you have a reachable host, otherwise all local — open a couple via the Sessions panel, or via `secondary-shift-t` itself once the first exists):
- `secondary-shift-t` on a focused local tab opens a new local shell tab.
- `secondary-shift-t` on a focused SSH tab opens another shell on the same host (tab title becomes `name:2`, `name:3`, etc.).
- `secondary-tab` / `secondary-shift-tab` cycle forward/backward through tabs, wrapping at both ends.
- `secondary-1`..`secondary-9` jump directly to that tab (no-op if the app has fewer tabs than the number pressed).
- `secondary-shift-w` closes the focused tab.

- [ ] **Step 10: Commit**

```bash
git add src/workspace.rs src/main.rs
git commit -m "feat: add tab management keyboard shortcuts"
```

---

### Task 3: New Connection shortcut

**Files:**
- Modify: `src/workspace.rs` — one more action in the `actions!` block from Task 2, one handler, one `.on_action(...)` wiring line.
- Modify: `src/main.rs` — one import, one `KeyBinding`.

**Interfaces:**
- Consumes: the `actions!(caracal_workspace, [...])` block from Task 2 (add `NewConnectionAction` to that same list). `SessionsPanel::open_new_connection_window(&mut self, group_id: Option<String>, edit_ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>)` (already exists, `pub(crate)`, at `src/panels/sessions.rs:418`). `Workspace::saved_sessions: Entity<SessionsPanel>` (already exists, `workspace.rs:266`).

- [ ] **Step 1: Add the action**

In `src/workspace.rs`, add `NewConnectionAction` to the `actions!` list from Task 2:

```rust
gpui::actions!(caracal_workspace, [
    NewTab,
    NextTab,
    PrevTab,
    GotoTab1,
    GotoTab2,
    GotoTab3,
    GotoTab4,
    GotoTab5,
    GotoTab6,
    GotoTab7,
    GotoTab8,
    GotoTab9,
    NewConnectionAction,
]);
```

- [ ] **Step 2: Implement the handler**

Add to `Workspace` (near `on_new_tab`):

```rust
    /// `secondary-shift-n`: open the "New Connection" window (focuses it
    /// instead of opening a duplicate if one is already open — see
    /// `SessionsPanel::open_new_connection_window`).
    fn on_new_connection(&mut self, _: &NewConnectionAction, window: &mut Window, cx: &mut Context<Self>) {
        self.saved_sessions.update(cx, |panel, cx| {
            panel.open_new_connection_window(None, None, window, cx);
        });
    }
```

- [ ] **Step 3: Wire it into `Render for Workspace`**

Add one more line to the chain from Task 2's Step 6:

```rust
            .on_action(cx.listener(Self::on_new_connection))
```

- [ ] **Step 4: Bind the key in `main.rs`**

Add `NewConnectionAction` to the `use workspace::{...}` import from Task 2, and one entry to `cx.bind_keys([...])`:

```rust
            KeyBinding::new("secondary-shift-n", NewConnectionAction, None),
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 6: Manual test**

Run: `cargo run`, press `secondary-shift-n`. Expected: the "New Connection" window opens. Press it again while that window is still open: expected the existing window is focused, not a second one opened.

- [ ] **Step 7: Commit**

```bash
git add src/workspace.rs src/main.rs
git commit -m "feat: add new-connection keyboard shortcut"
```

---

### Task 4: Sidebar, quick-commands, and settings toggles

**Files:**
- Modify: `src/workspace.rs` — two new fields, `Workspace::new`, `toggle_panel`, `open_security_auth_panel`, new actions/handlers, `Render::render` wiring.
- Modify: `src/main.rs` — imports, `KeyBinding`s.

**Interfaces:**
- Consumes: `PanelId`, `Side` (already imported in `workspace.rs`). The `actions!` block from Task 2/3 (add four more entries).
- Produces: `Workspace` fields `left_last_panel: PanelId`, `right_last_panel: PanelId` — not consumed elsewhere in this plan, but any future code that opens a left/right panel programmatically should keep them in sync the same way `toggle_panel`/`open_security_auth_panel` do below.

- [ ] **Step 1: Add the remembered-panel fields**

In the `Workspace` struct (near `left_active`/`right_active`, around line 291):

```rust
    left_active: Option<PanelId>,
    right_active: Option<PanelId>,
    /// Which panel to reopen when `secondary-b` toggles the left sidebar
    /// back on after it was closed — closing it (mouse click on the active
    /// icon, or the keyboard toggle) sets `left_active` to `None`, which
    /// loses track of what was showing, so this remembers it separately.
    /// Defaults to `Sftp` (`side_items(Side::Left)`'s first entry).
    left_last_panel: PanelId,
    /// Mirrors `left_last_panel` for the right sidebar. Defaults to
    /// `Sessions`, matching today's actual startup default
    /// (`right_active: Some(PanelId::Sessions)` below).
    right_last_panel: PanelId,
```

In `Workspace::new`'s `Self { ... }` (near `right_active: Some(PanelId::Sessions),`, around line 493):

```rust
            left_active: None,
            right_active: Some(PanelId::Sessions),
            left_last_panel: PanelId::Sftp,
            right_last_panel: PanelId::Sessions,
```

- [ ] **Step 2: Keep the remembered panel in sync on every open**

Replace `toggle_panel` (around line 1215) with:

```rust
    /// Toggle `id` in its side's single slot: open it if it isn't the active
    /// panel, otherwise close the slot. Also updates that side's
    /// remembered panel (`left_last_panel`/`right_last_panel`) whenever the
    /// slot opens, so the keyboard sidebar toggle (`toggle_side`, below)
    /// reopens the same panel a mouse click last chose.
    fn toggle_panel(&mut self, id: PanelId, _window: &mut Window, cx: &mut Context<Self>) {
        let is_active = match id.side() {
            Side::Left => self.left_active == Some(id),
            Side::Right => self.right_active == Some(id),
        };
        match id.side() {
            Side::Left => self.left_active = if is_active { None } else { Some(id) },
            Side::Right => self.right_active = if is_active { None } else { Some(id) },
        }
        if !is_active {
            match id.side() {
                Side::Left => self.left_last_panel = id,
                Side::Right => self.right_last_panel = id,
            }
        }
        cx.notify();
    }
```

In `open_security_auth_panel` (around line 1234), add the remembered-panel update:

```rust
    pub(crate) fn open_security_auth_panel(&mut self, cx: &mut Context<Self>) {
        self.left_active = Some(PanelId::Security);
        self.left_last_panel = PanelId::Security;
        cx.notify();
        let _ = self.own_window.update(cx, |_view, window, _cx| window.activate_window());
    }
```

- [ ] **Step 3: Add the actions**

Extend the `actions!` block from Tasks 2/3:

```rust
gpui::actions!(caracal_workspace, [
    NewTab,
    NextTab,
    PrevTab,
    GotoTab1,
    GotoTab2,
    GotoTab3,
    GotoTab4,
    GotoTab5,
    GotoTab6,
    GotoTab7,
    GotoTab8,
    GotoTab9,
    NewConnectionAction,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    ToggleQuickCommands,
    OpenSettingsAction,
]);
```

- [ ] **Step 4: Implement the handlers**

Add to `Workspace`:

```rust
    /// Shared by `on_toggle_left_sidebar`/`on_toggle_right_sidebar`: flips
    /// the side's slot between `None` and its remembered panel.
    fn toggle_side(&mut self, side: Side, cx: &mut Context<Self>) {
        let (currently_open, remembered) = match side {
            Side::Left => (self.left_active.is_some(), self.left_last_panel),
            Side::Right => (self.right_active.is_some(), self.right_last_panel),
        };
        let new_value = if currently_open { None } else { Some(remembered) };
        match side {
            Side::Left => self.left_active = new_value,
            Side::Right => self.right_active = new_value,
        }
        cx.notify();
    }

    fn on_toggle_left_sidebar(
        &mut self,
        _: &ToggleLeftSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_side(Side::Left, cx);
    }

    fn on_toggle_right_sidebar(
        &mut self,
        _: &ToggleRightSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_side(Side::Right, cx);
    }

    fn on_toggle_quick_commands(
        &mut self,
        _: &ToggleQuickCommands,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_quick_commands = !self.show_quick_commands;
        cx.notify();
    }

    fn on_open_settings_action(
        &mut self,
        _: &OpenSettingsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
    }
```

- [ ] **Step 5: Wire the handlers into `Render for Workspace`**

Add four more lines to the chain from Task 2/3:

```rust
            .on_action(cx.listener(Self::on_toggle_left_sidebar))
            .on_action(cx.listener(Self::on_toggle_right_sidebar))
            .on_action(cx.listener(Self::on_toggle_quick_commands))
            .on_action(cx.listener(Self::on_open_settings_action))
```

- [ ] **Step 6: Bind the keys in `main.rs`**

Add the four new types to the `use workspace::{...}` import, and four entries to `cx.bind_keys([...])`:

```rust
            KeyBinding::new("secondary-b", ToggleLeftSidebar, None),
            KeyBinding::new("secondary-shift-b", ToggleRightSidebar, None),
            KeyBinding::new("secondary-j", ToggleQuickCommands, None),
            KeyBinding::new("secondary-,", OpenSettingsAction, None),
```

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 8: Manual test**

Run: `cargo run`.
- Click the SFTP icon to open the left sidebar on SFTP, then press `secondary-b` twice: it closes, then reopens back on SFTP (not reset to Network).
- `secondary-shift-b` toggles the right sidebar the same way (Sessions by default).
- `secondary-j` toggles the bottom quick-commands drawer.
- `secondary-,` opens Settings; pressing it again focuses the same window instead of opening a second one.

- [ ] **Step 9: Commit**

```bash
git add src/workspace.rs src/main.rs
git commit -m "feat: add sidebar/quick-commands/settings keyboard shortcuts"
```

---

### Task 5: Terminal font zoom

**Files:**
- Modify: `src/workspace.rs` — pure clamp function + tests, zoom actions/handlers, wiring.
- Modify: `src/main.rs` — imports, `KeyBinding`s.

**Interfaces:**
- Consumes: `Workspace::apply_font_settings(&mut self, font_family: String, font_size: Pixels, font_fallback1: String, font_fallback2: String, cx: &mut Context<Self>)` (already exists, `workspace.rs:1019`). `crate::settings::{load, save}` (already imported as `crate::settings` in `workspace.rs:57`; `settings::load() -> AppSettings`, `settings::save(&AppSettings) -> std::io::Result<()>` per existing call sites in `settings_window.rs`).
- Produces: `Workspace::clamped_font_size(current: f32, delta: f32) -> f32` (pure, private, tested here — not consumed elsewhere in this plan).

- [ ] **Step 1: Write the failing tests**

Add to `workspace.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn clamped_font_size_applies_the_step() {
        assert_eq!(Workspace::clamped_font_size(14.0, 1.0), 15.0);
        assert_eq!(Workspace::clamped_font_size(14.0, -1.0), 13.0);
    }

    #[test]
    fn clamped_font_size_does_not_exceed_the_max() {
        assert_eq!(Workspace::clamped_font_size(96.0, 1.0), 96.0);
    }

    #[test]
    fn clamped_font_size_does_not_go_below_the_min() {
        assert_eq!(Workspace::clamped_font_size(6.0, -1.0), 6.0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib workspace::tests -- clamped_font_size`
Expected: FAIL to compile — `clamped_font_size` not found.

- [ ] **Step 3: Implement the clamp function and the actions**

Add near `apply_font_settings` (around line 1019):

```rust
    /// Bounds match `settings_window.rs`'s `parse_font_size` validation
    /// range (6..=96px) — zooming can't push the terminal font outside what
    /// Settings would already reject if typed by hand.
    const MIN_FONT_SIZE: f32 = 6.0;
    const MAX_FONT_SIZE: f32 = 96.0;
    const ZOOM_STEP: f32 = 1.0;

    fn clamped_font_size(current: f32, delta: f32) -> f32 {
        (current + delta).clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE)
    }

    /// Adjust the persisted terminal font size by `delta`px (clamped) and
    /// broadcast it to every open tab — the same effect as changing the
    /// font-size field in Settings and clicking Apply, just without opening
    /// the dialog. No-ops (does not touch disk) if the delta is fully
    /// absorbed by the clamp, i.e. already at the min/max.
    fn zoom_font(&mut self, delta: f32, cx: &mut Context<Self>) {
        let mut loaded = settings::load();
        let new_size = Self::clamped_font_size(loaded.terminal.font_size, delta);
        if new_size == loaded.terminal.font_size {
            return;
        }
        loaded.terminal.font_size = new_size;
        if let Err(e) = settings::save(&loaded) {
            log::error!("failed to save zoomed font size: {e}");
            return;
        }
        self.apply_font_settings(
            loaded.terminal.font_family.clone(),
            px(new_size),
            loaded.terminal.font_fallback1.clone(),
            loaded.terminal.font_fallback2.clone(),
            cx,
        );
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_font(Self::ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_font(-Self::ZOOM_STEP, cx);
    }
```

Extend the `actions!` block from Tasks 2-4 with `ZoomIn, ZoomOut`:

```rust
gpui::actions!(caracal_workspace, [
    NewTab,
    NextTab,
    PrevTab,
    GotoTab1,
    GotoTab2,
    GotoTab3,
    GotoTab4,
    GotoTab5,
    GotoTab6,
    GotoTab7,
    GotoTab8,
    GotoTab9,
    NewConnectionAction,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    ToggleQuickCommands,
    OpenSettingsAction,
    ZoomIn,
    ZoomOut,
]);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib workspace::tests -- clamped_font_size`
Expected: PASS (3 tests).

- [ ] **Step 5: Wire the handlers into `Render for Workspace`**

Add two more lines to the chain from the earlier tasks:

```rust
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
```

- [ ] **Step 6: Bind the keys in `main.rs`**

Add `ZoomIn, ZoomOut` to the `use workspace::{...}` import, and these entries to `cx.bind_keys([...])` (both plain `=`/`-` and the shifted `+` variant, since `+` requires Shift on most keyboards and users will reach for either):

```rust
            KeyBinding::new("secondary-=", ZoomIn, None),
            KeyBinding::new("secondary-shift-=", ZoomIn, None),
            KeyBinding::new("secondary--", ZoomOut, None),
```

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 8: Manual test**

Run: `cargo run`. Press `secondary-=` a few times: every open tab's font grows 1px each press. Press `secondary--`: shrinks back. Hold either past the 6px/96px bound: it stops changing instead of erroring. Restart the app: the last zoomed size is still applied (persisted).

- [ ] **Step 9: Commit**

```bash
git add src/workspace.rs src/main.rs
git commit -m "feat: add terminal font zoom keyboard shortcuts"
```

---

### Task 6: Clear screen

**Files:**
- Modify: `src/terminal/view.rs` — new action in the existing `caracal_terminal` actions block, new handler, `.on_action(...)` wiring.
- Modify: `src/main.rs` — import, `KeyBinding`.

**Interfaces:**
- Consumes: `self.term: SharedTerm` (`Arc<FairMutex<Term<EventProxy>>>`, already a field on `TerminalView`, used throughout `terminal/view.rs`, e.g. at line 793 `self.term.lock().scroll_display(...)`).

- [ ] **Step 1: Add the action**

In `src/terminal/view.rs`, extend the existing actions block (line 45):

```rust
gpui::actions!(caracal_terminal, [Interrupt, SendTab, SendBackTab, ClearScreen]);
```

- [ ] **Step 2: Implement the handler**

Add near `on_send_back_tab` (around line 828):

```rust
    /// `secondary-shift-l`: erase the visible viewport only (not
    /// scrollback history) — a client-side force-clear, distinct from
    /// plain `Ctrl+L` (still passed through as a raw byte to the
    /// shell/remote program, which already binds it to clear-screen via
    /// readline in almost every shell).
    fn on_clear_screen(&mut self, _: &ClearScreen, _window: &mut Window, cx: &mut Context<Self>) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        self.term.lock().clear_screen(ClearMode::All);
        cx.notify();
    }
```

- [ ] **Step 3: Wire it into `Render for TerminalView`**

In the `.on_action(...)` chain (around line 1045), add:

```rust
            .on_action(cx.listener(Self::on_interrupt))
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_back_tab))
            .on_action(cx.listener(Self::on_clear_screen))
```

- [ ] **Step 4: Bind the key in `main.rs`**

Add `ClearScreen` to the existing `use terminal::view::{Interrupt, SendBackTab, SendTab, TERMINAL_KEY_CONTEXT};` import (line 39):

```rust
use terminal::view::{ClearScreen, Interrupt, SendBackTab, SendTab, TERMINAL_KEY_CONTEXT};
```

Add one entry to `cx.bind_keys([...])`:

```rust
            KeyBinding::new("secondary-shift-l", ClearScreen, Some(TERMINAL_KEY_CONTEXT)),
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 6: Manual test**

Run: `cargo run`, open a terminal tab, type a few commands so there's visible output, then press `secondary-shift-l`. Expected: the visible screen clears (cursor lands at the top), but scrolling up still shows the prior output in scrollback.

- [ ] **Step 7: Commit**

```bash
git add src/terminal/view.rs src/main.rs
git commit -m "feat: add clear-screen keyboard shortcut"
```

---

## Self-Review Notes

- **Spec coverage:** every row of the spec's keybinding table maps to a task — tab mgmt (Task 2), new connection (Task 3), sidebars/quick-commands/settings (Task 4), zoom (Task 5), clear screen (Task 6). Task 1 is the pure-logic prerequisite for Task 2.
- **Placeholder scan:** no TBDs; every step has literal code, not descriptions.
- **Type consistency:** action names (`NewTab`, `NextTab`, `PrevTab`, `GotoTab1..9`, `NewConnectionAction`, `ToggleLeftSidebar`, `ToggleRightSidebar`, `ToggleQuickCommands`, `OpenSettingsAction`, `ZoomIn`, `ZoomOut`, `ClearScreen`) and handler names (`on_new_tab`, `on_next_tab`, etc.) are used identically across every task that references them and in `main.rs`'s imports.
