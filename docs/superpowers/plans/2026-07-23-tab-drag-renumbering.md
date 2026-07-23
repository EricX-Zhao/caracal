# Tab Drag Renumbering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the previously-accepted "drag-reorder doesn't update tab numbers" gap. `gpui-component` always leaves a dropped tab as the active panel in its `TabPanel` (confirmed by reading every drag-drop code path), so `TerminalPanel::on_added_to` — already fired on every (re)attach, including drags — can read that `TabPanel`'s public `active_ix()` and tell `Workspace` where to reposition this tab in its own tracked order, then renumber everyone.

**Architecture:** `TerminalPanel` gains a `workspace: WeakEntity<Workspace>` back-reference (same pattern `SftpPanel`/`MonitorPanel` already use) and, in `on_added_to`, calls a new `Workspace::reposition_tab_panel` method with its freshly-read `active_ix`. `reposition_tab_panel` removes the tab from wherever it sits in `tab_panels` and reinserts it at the reported index, then calls the existing `renumber_tabs`.

**Tech Stack:** Rust, gpui / gpui-component (dock `Panel` trait), no new dependencies.

## Global Constraints

- Scoped to the single-tab-strip case this whole feature already assumes (no support for tabs split across multiple panes — a pre-existing boundary of `Workspace::active_tabs_item`, not something this plan changes).
- No change to `secondary-1..9` shortcut behavior, `TerminalPanelEvent`'s variants, or the drag-reorder false-close fix (`attached`/deferred-close mechanism) from the prior plan — this builds alongside it, not instead of it.
- `register_tab_panel`/`unregister_tab_panel`/`renumber_tabs` (from the live-renumbering plan) are unchanged and still called exactly as before.

---

### Task 1: `TerminalPanel` repositions itself via `active_ix` on every (re)attach

**Files:**
- Modify: `src/panels/terminal.rs` — imports, struct + `new` (around line 95-131), `on_added_to` (around line 289-297)
- Modify: `src/workspace.rs` — add `reposition_tab_panel` after `renumber_tabs` (around line 706-710); update all four `TerminalPanel::new` call sites (`open_local_with`, `open_ssh`, `open_telnet`, `open_serial`)

**Interfaces:**
- Produces: `Workspace::reposition_tab_panel(&mut self, panel: Entity<TerminalPanel>, new_ix: usize, cx: &mut Context<Self>)` (private, called only from `TerminalPanel::on_added_to`).
- Changes: `TerminalPanel::new`'s signature gains a fourth parameter, `workspace: WeakEntity<Workspace>` — every call site is updated in this same task (a signature change requires it; a reviewer can't approve one call site without the others).

This is one task — the constructor signature change and the new `Workspace` method are tightly coupled (the method has no caller until the constructor threads the handle through, and the constructor change breaks the build until all four call sites pass the new argument), matching the same reasoning used for previous single-task-vs-split decisions in this feature's earlier rounds.

No new automated test (see the design spec's "Testing" section): this is live GPUI lifecycle-callback behavior (`on_added_to` timing, reading a sibling entity's `active_ix()`) that can't be exercised without a live `Window`/`DockArea` driving real drag input, matching this codebase's established precedent. Verified by a clean build/test run plus the manual checklist in Step 6.

- [ ] **Step 1: Import `Workspace` in `src/panels/terminal.rs`**

Add, alongside the existing `use crate::...` imports (after `use crate::terminal::view::TerminalView;`):

```rust
use crate::workspace::Workspace;
```

- [ ] **Step 2: Add the `workspace` field and thread it through `new`**

Find this exact current text:

```rust
    /// Whether this panel is currently attached to a `TabPanel`. Flipped to
    /// `false` by `on_removed`, back to `true` by `on_added_to` — the signal
    /// `on_removed` uses to tell a genuine close apart from gpui-component's
    /// internal detach-then-reinsert dance for a drag-reorder (both call
    /// `on_removed` identically; only a real close never calls `on_added_to`
    /// again afterward). See
    /// docs/superpowers/specs/2026-07-22-drag-reorder-false-close-design.md.
    attached: bool,
    /// Lazily built on first render (`TerminalPanel::new` takes no `cx`, and
    /// the handle needs `self.terminal.read(cx)` to get the shared `Term`).
    scrollbar_handle: Option<TerminalScrollbarHandle>,
}

impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>, tab_number: u32) -> Self {
        Self {
            terminal,
            tab_panel: None,
            tab_number,
            attached: true,
            scrollbar_handle: None,
        }
    }
```

Replace with:

```rust
    /// Whether this panel is currently attached to a `TabPanel`. Flipped to
    /// `false` by `on_removed`, back to `true` by `on_added_to` — the signal
    /// `on_removed` uses to tell a genuine close apart from gpui-component's
    /// internal detach-then-reinsert dance for a drag-reorder (both call
    /// `on_removed` identically; only a real close never calls `on_added_to`
    /// again afterward). See
    /// docs/superpowers/specs/2026-07-22-drag-reorder-false-close-design.md.
    attached: bool,
    /// Back-reference so `on_added_to` can tell `Workspace` where this tab
    /// landed after every (re)attach, including a drag-reorder — mirrors
    /// the same pattern `SftpPanel`/`MonitorPanel` already use to call back
    /// into `Workspace`. See
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md.
    workspace: WeakEntity<Workspace>,
    /// Lazily built on first render (`TerminalPanel::new` takes no `cx`, and
    /// the handle needs `self.terminal.read(cx)` to get the shared `Term`).
    scrollbar_handle: Option<TerminalScrollbarHandle>,
}

impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>, tab_number: u32, workspace: WeakEntity<Workspace>) -> Self {
        Self {
            terminal,
            tab_panel: None,
            tab_number,
            attached: true,
            workspace,
            scrollbar_handle: None,
        }
    }
```

- [ ] **Step 3: Update `on_added_to`**

Find this exact current text:

```rust
    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
        self.attached = true;
    }
```

Replace with:

```rust
    /// gpui-component calls `on_added_to` on every (re)attach — including a
    /// drag-reorder's reinsert — but crucially calls it *before* it sets the
    /// panel's new `active_ix` (confirmed by reading `TabPanel::insert_panel_at`/
    /// `add_panel_with_active`: both call `on_added_to` first, `set_active_ix`
    /// after). Reading `active_ix()` synchronously right here would see the
    /// *previous* value, not this panel's real new position. Deferring the
    /// read (via `window.defer`, until after the current update cycle — the
    /// same reentrancy-avoidance pattern used elsewhere in this file) fixes
    /// both that staleness *and* a second problem: this callback typically
    /// fires while a `Workspace`-entity update is already on the call stack
    /// (e.g. from `Workspace::open_local_with`'s own dispatch), and calling
    /// `workspace.update(...)` synchronously from in here would be exactly
    /// the nested-self-update GPUI panics on. See
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md.
    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel.clone());
        self.attached = true;
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let this = cx.entity();
        window.defer(cx, move |_window, cx| {
            let Some(active_ix) = tab_panel.upgrade().map(|tp| tp.read(cx).active_ix()) else {
                return;
            };
            workspace.update(cx, |workspace, cx| {
                workspace.reposition_tab_panel(this, active_ix, cx);
            });
        });
    }
```

- [ ] **Step 4: Add `Workspace::reposition_tab_panel` in `src/workspace.rs`**

Find this exact current text (right after `renumber_tabs`):

```rust
    /// Set every open tab's displayed `"N-"` prefix to its 1-indexed
    /// position in `tab_panels`.
    fn renumber_tabs(&mut self, cx: &mut Context<Self>) {
        for (i, panel) in self.tab_panels.iter().enumerate() {
            panel.update(cx, |panel, cx| panel.set_tab_number((i + 1) as u32, cx));
        }
    }
```

Add right after it (before the following `ssh_keys_snapshot` method):

```rust

    /// Move `panel` to `new_ix` in the open-tab list (removing it from
    /// wherever it currently sits first) and recompute every open tab's
    /// displayed sequence number. Called by `TerminalPanel::on_added_to`
    /// every time gpui-component (re)attaches a panel — including a manual
    /// drag-reorder, whose new position this reads via the dropped panel's
    /// own `TabPanel::active_ix()` (always left pointing at it — see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    fn reposition_tab_panel(&mut self, panel: Entity<TerminalPanel>, new_ix: usize, cx: &mut Context<Self>) {
        self.tab_panels.retain(|p| p.entity_id() != panel.entity_id());
        let ix = new_ix.min(self.tab_panels.len());
        self.tab_panels.insert(ix, panel);
        self.renumber_tabs(cx);
    }
```

- [ ] **Step 5: Update all four `TerminalPanel::new` call sites**

Each of `open_local_with`, `open_ssh`, `open_telnet`, `open_serial` currently has this exact line (identical text at all four sites):

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0));
```

Immediately before that line at each of the four sites, add:

```rust
        let workspace_handle = cx.entity().downgrade();
```

And change the line itself to:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0, workspace_handle));
```

(This mirrors the exact idiom `Workspace::new` already uses for `QuickCommandsPanel` at line 448: `let workspace_handle = cx.entity().downgrade();` then passed into the child's constructor.)

- [ ] **Step 6: Build, test, and manually verify**

Run: `cargo build`
Expected: clean build, no warnings about unused `workspace` field or `reposition_tab_panel`.

Run: `cargo test`
Expected: all tests pass (no test constructs `TerminalPanel` directly or calls any `open_*` method).

Run the app (`cargo run`) and check by hand (no automated/screenshot check, per this project's convention):
- Open 4 tabs: confirm they read `1-`, `2-`, `3-`, `4-`.
- Drag the 4th tab to the 3rd visual position: confirm the numbers immediately update to reflect the new order (the tab you dragged now reads `3-`, and the tab it displaced now reads `4-`) — no more frozen/stale numbers after a drag.
- Confirm `Ctrl+1`/`Ctrl+2`/`Ctrl+3`/`Ctrl+4` each jump to the tab currently printing that number, including right after the drag.
- Open a brand-new tab afterward: confirm it still gets the correct next number (the reposition-on-first-attach path staying a no-op, not breaking the append case).

- [ ] **Step 7: Commit**

```bash
git add src/panels/terminal.rs src/workspace.rs
git commit -m "$(cat <<'EOF'
feat: keep tab numbers in sync through drag-reorder

EOF
)"
```
