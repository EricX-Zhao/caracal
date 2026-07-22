# Tab Live Renumbering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sticky lowest-free-number tab-numbering mechanism (shipped earlier today) with live renumbering: every open tab's displayed `"N-"` prefix is recomputed from its real open-order position every time a tab opens or closes, so the number only ever goes stale after a manual drag-reorder (not after closing a middle tab, which was the bug the user actually hit).

**Architecture:** `Workspace` keeps an ordered `Vec<Entity<TerminalPanel>>` of currently-open tabs (append on open, remove on close) instead of a `HashSet<u32>` of allocated numbers. A `renumber_tabs` helper walks that list after every change and pushes each tab's 1-indexed position into a new `TerminalPanel::set_tab_number` setter. `TerminalPanel::new`'s signature and `title()`'s rendering are unchanged from the currently-shipped feature.

**Tech Stack:** Rust, gpui / gpui-component (dock `Panel` trait), no new dependencies.

## Global Constraints

- `secondary-1..9` shortcut behavior is unchanged and out of scope — it was already proven correct (via live debug-log evidence gathered during this bug's investigation) and must not be touched.
- No changes to `TerminalPanelEvent`'s variants.
- Manual drag-reorder remains an accepted, unfixable gap (no `gpui-component` fork) — this plan only fixes the open/close case.
- This plan **removes** the previous `tab_numbers: HashSet<u32>` / `allocate_tab_number` / `release_tab_number` mechanism entirely; it is superseded, not layered under.

---

### Task 1: `TerminalPanel::set_tab_number` + `Workspace`'s ordered tab list

**Files:**
- Modify: `src/panels/terminal.rs:102-107` (field doc comment) and add a new method after `new` (around `src/panels/terminal.rs:113-121`)
- Modify: `src/workspace.rs:263-271` (remove `tab_numbers` field, add `tab_panels` field)
- Modify: `src/workspace.rs` — remove `allocate_tab_number`/`release_tab_number` (currently right after `release_ssh_tab_number`, search for `fn allocate_tab_number`), add `register_tab_panel`/`unregister_tab_panel`/`renumber_tabs` in their place
- Modify: `src/workspace.rs:512` (`Workspace::new`'s struct literal — replace `tab_numbers: HashSet::new(),` with `tab_panels: Vec::new(),`)

**Interfaces:**
- Produces: `TerminalPanel::set_tab_number(&mut self, n: u32, cx: &mut Context<Self>)` (`pub(crate)`) — used by Task 2.
- Produces: `Workspace::register_tab_panel(&mut self, panel: Entity<TerminalPanel>, cx: &mut Context<Self>)` and `Workspace::unregister_tab_panel(&mut self, panel: &Entity<TerminalPanel>, cx: &mut Context<Self>)` — used by Task 2.
- Consumes: nothing new from elsewhere. `TerminalPanel::new`'s existing signature (`terminal: Entity<TerminalView>, tab_number: u32`) is untouched by this task.

This task doesn't wire anything into the four `open_*` methods yet — that's Task 2. After this task, `register_tab_panel`/`unregister_tab_panel` will show as `dead_code` (nothing calls them yet) — expected, matches this codebase's established foundation-task pattern.

- [ ] **Step 1: Update the `tab_number` field's doc comment in `src/panels/terminal.rs`**

Change (around line 102-107):

```rust
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix — assigned once by `Workspace::allocate_tab_number` when the
    /// tab was opened. Not recomputed if the tab is later drag-reordered
    /// or if an earlier tab is closed, shifting this one's visual slot
    /// (see docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    tab_number: u32,
```

to:

```rust
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix. Kept in sync by `Workspace::renumber_tabs` (via
    /// `set_tab_number` below) every time the open-tab set changes — not
    /// a value this panel manages itself. Only goes stale after a manual
    /// drag-reorder, which `Workspace` has no way to observe (see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    tab_number: u32,
```

- [ ] **Step 2: Add the setter in `src/panels/terminal.rs`**

Right after `TerminalPanel::new`'s closing brace (the `new` function currently ends around line 114-121, look for the `}` that closes it, before `fn close`), add:

```rust
    /// Overwrite this tab's displayed `"N-"` prefix and repaint. Called by
    /// `Workspace::renumber_tabs` whenever the open-tab set changes.
    pub(crate) fn set_tab_number(&mut self, n: u32, cx: &mut Context<Self>) {
        self.tab_number = n;
        cx.notify();
    }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: clean build. `set_tab_number` will show a `dead_code` warning (unused) — expected, resolved in Task 2.

- [ ] **Step 4: Replace `tab_numbers`/`allocate_tab_number`/`release_tab_number` in `src/workspace.rs`**

Find the `tab_numbers` field (currently right after `ssh_tab_numbers` in the `Workspace` struct, around line 262-271):

```rust
    ssh_tab_numbers: HashMap<String, HashSet<u32>>,
    /// Sequence numbers (the `"N-"` tab-title prefix) currently in use
    /// across every open terminal tab, workspace-wide — unlike
    /// `ssh_tab_numbers` above, this single pool is shared by every tab
    /// kind (local/SSH/Telnet/Serial), not scoped per SSH host. Populated
    /// in each `open_*` method via `allocate_tab_number`, released
    /// wherever a `TerminalPanelEvent::Closed` is observed. Not
    /// recomputed on drag-reorder or on closing a non-last tab — see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md.
    tab_numbers: HashSet<u32>,
```

Replace with:

```rust
    ssh_tab_numbers: HashMap<String, HashSet<u32>>,
    /// Every currently open `TerminalPanel`, in open order — the source
    /// of truth for the workspace-wide `"N-"` tab-title sequence number,
    /// recomputed by live position (not a sticky per-tab id) every time a
    /// tab opens or closes via `register_tab_panel`/`unregister_tab_panel`.
    /// Only goes stale after a manual drag-reorder — see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md.
    tab_panels: Vec<Entity<TerminalPanel>>,
```

Find `Workspace::new`'s struct literal line `ssh_tab_numbers: HashMap::new(),` (around line 512) — it's currently followed by `tab_numbers: HashSet::new(),`. Change that second line to:

```rust
            tab_panels: Vec::new(),
```

Find `allocate_tab_number`/`release_tab_number` (added right after `release_ssh_tab_number`, search `fn allocate_tab_number`):

```rust
    fn allocate_tab_number(&mut self) -> u32 {
        let n = Self::lowest_free_number(&self.tab_numbers);
        self.tab_numbers.insert(n);
        n
    }

    fn release_tab_number(&mut self, n: u32) {
        self.tab_numbers.remove(&n);
    }
```

Delete those two methods entirely and replace with:

```rust
    /// Append `panel` to the end of the open-tab list and recompute every
    /// open tab's displayed sequence number from its new position.
    fn register_tab_panel(&mut self, panel: Entity<TerminalPanel>, cx: &mut Context<Self>) {
        self.tab_panels.push(panel);
        self.renumber_tabs(cx);
    }

    /// Remove `panel` from the open-tab list (by entity identity) and
    /// recompute every remaining tab's displayed sequence number.
    fn unregister_tab_panel(&mut self, panel: &Entity<TerminalPanel>, cx: &mut Context<Self>) {
        self.tab_panels.retain(|p| p.entity_id() != panel.entity_id());
        self.renumber_tabs(cx);
    }

    /// Set every open tab's displayed `"N-"` prefix to its 1-indexed
    /// position in `tab_panels`.
    fn renumber_tabs(&mut self, cx: &mut Context<Self>) {
        for (i, panel) in self.tab_panels.iter().enumerate() {
            panel.update(cx, |panel, cx| panel.set_tab_number((i + 1) as u32, cx));
        }
    }
```

`TerminalPanel` is already imported in `src/workspace.rs` (used throughout for the four `open_*` methods) — no new `use` needed.

- [ ] **Step 5: Verify it compiles and existing tests still pass**

Run: `cargo build`
Expected: clean build. `register_tab_panel`/`unregister_tab_panel` show as unused (`dead_code`) — expected, resolved in Task 2. `renumber_tabs` is called by both, so it won't itself warn once those two exist, even though nothing calls *them* yet.

Run: `cargo test --lib workspace::tests`
Expected: all existing tests pass unchanged (this task touches no tested pure function).

- [ ] **Step 6: Commit**

```bash
git add src/panels/terminal.rs src/workspace.rs
git commit -m "$(cat <<'EOF'
feat: replace sticky tab-number pool with an ordered open-tab list

EOF
)"
```

---

### Task 2: Wire live renumbering into all four tab-opening methods

**Files:**
- Modify: `src/workspace.rs:582-624` (`open_local_with`)
- Modify: `src/workspace.rs:725-799` (`open_ssh`)
- Modify: `src/workspace.rs:974-999` (`open_telnet`)
- Modify: `src/workspace.rs:1001-1026` (`open_serial`)

(Line numbers are from before Task 1's edits shift them slightly — find each method by name; the shapes below are exact regardless of the final line numbers.)

**Interfaces:**
- Consumes: `Workspace::register_tab_panel`/`unregister_tab_panel` from Task 1.
- No new interfaces produced — this is the last task for this rework.

This is one task, not four, for the same reason as the original feature's Task 2: all four call sites change together in one coherent step (removing the now-dead `tab_seq`/`allocate_tab_number` calls and adding the new register/unregister calls), and a reviewer can't sensibly approve one call site's rewiring without the others changing identically.

No automated test for this step either (same "can't unit-test live Entity manipulation" rationale as Task 1) — verified by a clean build/test run plus the manual checklist in Step 6.

- [ ] **Step 1: Wire `open_local_with`**

Change:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let tab_count_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(tab_count_sub);
        self.add_center(Arc::new(panel), window, cx);
```

to:

```rust
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(tab_count_sub);
        self.register_tab_panel(panel.clone(), cx);
        self.add_center(Arc::new(panel), window, cx);
```

- [ ] **Step 2: Wire `open_ssh`**

Change:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let closed_config = config.clone();
        let closed_term = term_weak.clone();
        let closed_key = key.clone();
        let closed_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
            this.release_ssh_tab_number(&closed_key, tab_number);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(closed_sub);
        self.add_center(Arc::new(panel), window, cx);
```

to:

```rust
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        // (Unrelated to the SSH per-host `tab_number` variable above, which
        // still feeds the "{display_name}:{n}" title suffix untouched.)
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0));
        let closed_config = config.clone();
        let closed_term = term_weak.clone();
        let closed_key = key.clone();
        let closed_sub = cx.subscribe_in(&panel, window, move |this, panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
            this.release_ssh_tab_number(&closed_key, tab_number);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(closed_sub);
        self.register_tab_panel(panel.clone(), cx);
        self.add_center(Arc::new(panel), window, cx);
```

Note: `move` stays on this closure (it already captures `closed_config`/`closed_term`/`closed_key`, unrelated to this change) — the pre-existing SSH per-host `tab_number` variable (used earlier in `open_ssh` for the `:n` title suffix) is untouched, only the new-this-feature `tab_seq` local is removed.

- [ ] **Step 3: Wire `open_telnet`**

Change:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let tab_count_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(tab_count_sub);
        self.add_center(Arc::new(panel), window, cx);
```

to:

```rust
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(tab_count_sub);
        self.register_tab_panel(panel.clone(), cx);
        self.add_center(Arc::new(panel), window, cx);
```

- [ ] **Step 4: Wire `open_serial`**

Change:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let tab_count_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(tab_count_sub);
        self.add_center(Arc::new(panel), window, cx);
```

to:

```rust
        // tab_number (0) is a throwaway placeholder — register_tab_panel's
        // renumber_tabs call below corrects it before anything renders.
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, 0));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, panel, event, _window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.unregister_tab_panel(panel, cx);
        });
        self._subscriptions.push(tab_count_sub);
        self.register_tab_panel(panel.clone(), cx);
        self.add_center(Arc::new(panel), window, cx);
```

- [ ] **Step 5: Build and run the full test suite**

Run: `cargo build`
Expected: clean build, no `dead_code` warnings on `register_tab_panel`/`unregister_tab_panel`/`renumber_tabs`/`set_tab_number` (now all used), no leftover references to `tab_numbers`/`allocate_tab_number`/`release_tab_number`/`lowest_free_number`'s workspace-wide use (the SSH-scoped `allocate_ssh_tab_number`/`release_ssh_tab_number`/`lowest_free_number` itself stay — untouched, still used for the per-host dedup suffix).

Run: `cargo test`
Expected: all tests pass (no test constructs `TerminalPanel` or calls the four `open_*` methods directly, so no test code needs updating).

- [ ] **Step 6: Manual verification**

Run the app (`cargo run`) and check by hand (no automated/screenshot check, per this project's convention):
- Open 3 tabs (any kind): titles read `1-`, `2-`, `3-...`.
- Close the `2-...` tab: the remaining two immediately renumber to `1-` and `2-` (not `1-` and `3-`).
- Open a new tab: it takes `3-`.
- Press `Ctrl+1`/`Ctrl+2`/`Ctrl+3`: each correctly jumps to the tab currently printing that number.
- Drag a tab to a different position: confirm its printed number does *not* immediately update (the one remaining, accepted gap) — but closing or opening any tab afterward corrects everyone's numbers again.

- [ ] **Step 7: Commit**

```bash
git add src/workspace.rs
git commit -m "$(cat <<'EOF'
feat: renumber tabs live on open/close instead of sticky allocation

EOF
)"
```
