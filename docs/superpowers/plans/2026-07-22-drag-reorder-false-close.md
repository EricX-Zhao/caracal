# Drag-Reorder False-Close Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `TerminalPanel::on_removed` from treating gpui-component's internal drag-reorder detach (which it can't distinguish from a real close) as a genuine tab close — self-heal by deferring the "is this really gone" check by one update cycle and rechecking whether `on_added_to` fired again in the meantime.

**Architecture:** `TerminalPanel` gains an `attached: bool` field, flipped `false` in `on_removed` and back to `true` in `on_added_to`. `on_removed` defers (`window.defer`, an existing pattern in this same file) the actual `TerminalPanelEvent::Closed` emission, checking `attached` when the deferred callback runs. `Workspace`'s four `open_*` methods and their close-subscription closures are completely unchanged — they still just react to `Closed` whenever it legitimately fires.

**Tech Stack:** Rust, gpui / gpui-component (dock `Panel` trait), no new dependencies.

## Global Constraints

- Single-file fix: `src/panels/terminal.rs` only. `src/workspace.rs` is not touched.
- No change to the already-accepted "printed tab number goes stale after a drag" limitation (see `docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md`) — that's a separate, already-documented gap. This fix is about the *false close cleanup*, not about making drag-reorder renumber-aware.
- A genuine tab close (the "X" button or `Ctrl+Shift+W`) must still eventually decrement `tab_count`, remove the tab from `Workspace`'s renumbering list, and (for SSH) tear down the shared session/SFTP/Monitor panel — just one update-cycle tick later than before, which must remain imperceptible.

---

### Task 1: Self-healing close detection in `TerminalPanel`

**Files:**
- Modify: `src/panels/terminal.rs:95-122` (struct + `new`)
- Modify: `src/panels/terminal.rs:280-291` (`on_added_to` + `on_removed`)

**Interfaces:**
- No new public/`pub(crate)` interfaces — `on_removed`/`on_added_to` are existing `Panel` trait method overrides; this task only changes their bodies and adds a private field.
- Consumes: `cx.entity().downgrade()` and `window.defer(cx, ...)`, both already used elsewhere in this same file (`TerminalPanel::close`, a few lines below `on_removed`) — same pattern, same reentrancy rationale.

This is one task (not split further) — it's a single, small, tightly-coupled change to one file; splitting it would leave an intermediate state where either the field exists unused or `on_removed`'s behavior half-changes, neither independently meaningful.

No new automated test (see the design spec's "Testing" section for why: this is live GPUI lifecycle-callback behavior — `on_removed`/`on_added_to`/`window.defer` — that can't be exercised without a live `Window`/`DockArea` driving real drag input, matching this codebase's established precedent throughout its keyboard-shortcut and tab-lifecycle features). Verified by a clean build/test run plus the manual checklist in Step 5.

- [ ] **Step 1: Add the `attached` field and initialize it in `new`**

Find this exact current text:

```rust
pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix. Kept in sync by `Workspace::renumber_tabs` (via
    /// `set_tab_number` below) every time the open-tab set changes — not
    /// a value this panel manages itself. Only goes stale after a manual
    /// drag-reorder, which `Workspace` has no way to observe (see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    tab_number: u32,
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
            scrollbar_handle: None,
        }
    }
```

Replace with:

```rust
pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix. Kept in sync by `Workspace::renumber_tabs` (via
    /// `set_tab_number` below) every time the open-tab set changes — not
    /// a value this panel manages itself. Only goes stale after a manual
    /// drag-reorder, which `Workspace` has no way to observe (see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    tab_number: u32,
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

- [ ] **Step 2: Update `on_added_to` and `on_removed`**

Find this exact current text:

```rust
    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
    }

    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TerminalPanelEvent::Closed);
    }
}
```

Replace with:

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

    /// gpui-component's `TabPanel::detach_panel` calls this identically for
    /// a genuine close AND for every drag-driven reposition (detach, then
    /// immediately reinsert elsewhere) — it has no way to tell us which.
    /// Deferring the actual "is this really gone" check by one update cycle
    /// and rechecking `attached` lets a drag's synchronous reinsert (which
    /// flips `attached` back to `true` via `on_added_to`, before this
    /// deferred closure runs) self-heal away a false close. See
    /// docs/superpowers/specs/2026-07-22-drag-reorder-false-close-design.md.
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attached = false;
        let weak = cx.entity().downgrade();
        window.defer(cx, move |_window, cx| {
            let _ = weak.update(cx, |this, cx| {
                if !this.attached {
                    cx.emit(TerminalPanelEvent::Closed);
                }
            });
        });
    }
}
```

(The deferred closure's own `window` parameter is prefixed `_window` — unlike `TerminalPanel::close`'s deferred closure a few lines above, which needs `window` to call `panel.focus_handle(cx).focus(window, cx)`-style methods, this one only needs `cx` to update the weak entity.)

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: clean build. If the closure's `window` parameter produces an unused-variable warning, prefix it `_window` instead of adding the `let _ = window;` line (whichever reads cleaner — pick one, don't do both).

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all existing tests pass unchanged (no test constructs `TerminalPanel` directly or exercises `on_removed`/`on_added_to`).

- [ ] **Step 5: Manual verification**

Run the app (`cargo run`) and check by hand (no automated/screenshot check, per this project's convention):
- Open 3 tabs, drag one to a different position within the strip: confirm the app doesn't misbehave (no tab silently vanishes from the count, no visible glitch).
- After the drag, close a *different*, unrelated tab (or open a new one): confirm every remaining tab's `"N-"` number is still correctly recomputed — including the one you dragged (it should now participate in renumbering again, not stay frozen).
- If you have an SSH connection available: open exactly one tab for a host, confirm its SFTP/Monitor panel shows; drag that tab to reorder it relative to other tabs; confirm the SFTP/Monitor panel for that host is *not* reset/disconnected by the drag.
- Confirm a genuine close (the "X" button) still works exactly as before: the tab disappears, the count updates, and (for SSH) the shared session tears down once no tabs reference that host — with no perceptible delay.

- [ ] **Step 6: Commit**

```bash
git add src/panels/terminal.rs
git commit -m "$(cat <<'EOF'
fix: don't treat a drag-reorder's internal detach as a real tab close

EOF
)"
```
