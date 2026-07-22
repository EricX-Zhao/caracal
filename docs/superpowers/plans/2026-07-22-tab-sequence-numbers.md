# Tab Sequence Numbers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prefix every terminal tab's title with a 1-indexed `"N-"` sequence number, assigned when the tab opens and reused (lowest-free-number) when tabs close, matching the position `secondary-1..9` would jump to in the common case.

**Architecture:** `Workspace` gains a workspace-wide `HashSet<u32>` pool (mirroring the existing per-host `ssh_tab_numbers` pool) with `allocate_tab_number`/`release_tab_number` helpers built on the already-tested pure `lowest_free_number` function. `TerminalPanel` gains a `tab_number: u32` field, set once at construction and rendered as a `"N-"` prefix in `title()`. All four tab-opening methods (`open_local_with`, `open_ssh`, `open_telnet`, `open_serial`) allocate a number before constructing the panel and release it in their existing close-subscription closure.

**Tech Stack:** Rust, gpui / gpui-component (dock `Panel` trait), no new dependencies.

## Global Constraints

- Numbers are 1-indexed, assigned once at tab creation, and are **not** recomputed when tabs are drag-reordered — this is an explicitly accepted limitation (see `docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md`), not a bug to fix in this plan.
- No changes to any keyboard shortcut or to `TerminalPanelEvent`'s variants.
- No screenshot-driven GUI verification (per this project's established convention) — the rendered prefix itself is checked manually by the user running the app, not by an automated test.

---

### Task 1: Workspace-wide tab-number pool

**Files:**
- Modify: `src/workspace.rs:262` (struct field, add sibling to `ssh_tab_numbers`)
- Modify: `src/workspace.rs:512` (`Workspace::new`'s struct literal)
- Modify: `src/workspace.rs:669-676` (add two new methods right after `release_ssh_tab_number`)

**Interfaces:**
- Produces: `Workspace::allocate_tab_number(&mut self) -> u32` and `Workspace::release_tab_number(&mut self, n: u32)` — used by Task 2's four call sites.
- Consumes: the existing pure `Workspace::lowest_free_number(used: &HashSet<u32>) -> u32` at `src/workspace.rs:619` (unchanged, already covered by `lowest_free_number_starts_at_one_when_empty` / `_skips_used_numbers` / `_reuses_a_gap` in the `tests` module at `src/workspace.rs:2011-2028`).

This task has no new unit tests of its own: `allocate_tab_number`/`release_tab_number` are two-line wrappers around the already-fully-tested `lowest_free_number`, exactly mirroring `allocate_ssh_tab_number`/`release_ssh_tab_number` (`src/workspace.rs:660-676`), which also have no dedicated tests in this codebase — the pure helper underneath is what's tested. Verification here is "it compiles and the existing test suite still passes."

- [ ] **Step 1: Add the `tab_numbers` field**

In `src/workspace.rs`, right after the `ssh_tab_numbers` field (around line 262):

```rust
    ssh_tab_numbers: HashMap<String, HashSet<u32>>,
    /// Sequence numbers (the `"N-"` tab-title prefix) currently in use
    /// across every open terminal tab, workspace-wide — unlike
    /// `ssh_tab_numbers` above, this single pool is shared by every tab
    /// kind (local/SSH/Telnet/Serial), not scoped per SSH host. Populated
    /// in each `open_*` method via `allocate_tab_number`, released
    /// wherever a `TerminalPanelEvent::Closed` is observed. Not
    /// recomputed on drag-reorder — see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md.
    tab_numbers: HashSet<u32>,
```

- [ ] **Step 2: Initialize it in `Workspace::new`**

In the `Self { ... }` struct literal (around line 512), add a line right after `ssh_tab_numbers: HashMap::new(),`:

```rust
            ssh_tab_numbers: HashMap::new(),
            tab_numbers: HashSet::new(),
```

- [ ] **Step 3: Add the allocate/release methods**

Right after `release_ssh_tab_number` (around line 669-676), add:

```rust
    /// Allocate the lowest unused positive sequence number for a newly
    /// opened terminal tab (the `"N-"` title prefix), workspace-wide
    /// across all tab kinds — same reuse-lowest-free-number scheme as
    /// `allocate_ssh_tab_number`, just not scoped to one SSH host.
    fn allocate_tab_number(&mut self) -> u32 {
        let n = Self::lowest_free_number(&self.tab_numbers);
        self.tab_numbers.insert(n);
        n
    }

    /// Release a tab number previously returned by `allocate_tab_number`,
    /// so the next tab opened anywhere in the workspace can reuse it.
    fn release_tab_number(&mut self, n: u32) {
        self.tab_numbers.remove(&n);
    }
```

- [ ] **Step 4: Verify it compiles and existing tests still pass**

Run: `cargo test --lib workspace::tests`
Expected: all existing tests pass (including `lowest_free_number_*`, `next_tab_index_*`, `prev_tab_index_*`, `goto_tab_index_*`), no compile errors. `allocate_tab_number`/`release_tab_number` will report as unused (`dead_code`) at this point — that's expected and gets resolved in Task 2.

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs
git commit -m "$(cat <<'EOF'
feat: add workspace-wide tab sequence number pool

EOF
)"
```

---

### Task 2: Render the sequence number and wire it into every tab-opening path

**Files:**
- Modify: `src/panels/terminal.rs:95-114` (struct + `new`)
- Modify: `src/panels/terminal.rs:241-263` (`title()`)
- Modify: `src/workspace.rs:572-609` (`open_local_with`)
- Modify: `src/workspace.rs:697-769` (`open_ssh`)
- Modify: `src/workspace.rs:944-965` (`open_telnet`)
- Modify: `src/workspace.rs:969-990` (`open_serial`)

**Interfaces:**
- Consumes: `Workspace::allocate_tab_number`/`release_tab_number` from Task 1.
- Produces: `TerminalPanel::new(terminal: Entity<TerminalView>, tab_number: u32) -> Self` (signature change — every call site in the codebase is updated in this same task, see the grep confirmation below).

This is one task, not four, because changing `TerminalPanel::new`'s signature and updating its four call sites must land together for the crate to compile — a reviewer can't sensibly accept one half without the other. (Confirmed via `grep -rn "TerminalPanel::new" src/` that these are the *only* four call sites; no test constructs `TerminalPanel` directly.)

There's no automated test for the rendered `"N-"` prefix itself: `title()` returns a `gpui` element tree (`impl IntoElement`), and this codebase doesn't unit-test rendered element output anywhere — GUI-visible output is checked by hand (per this project's established no-screenshot-driven-verification convention). Task 1's allocate/release logic is what's unit-tested; this task is verified by compiling clean and a manual run.

- [ ] **Step 1: Add `tab_number` to `TerminalPanel` and thread it through `new`**

In `src/panels/terminal.rs`, change the struct (around line 95-105):

```rust
pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix — assigned once by `Workspace::allocate_tab_number` when the
    /// tab was opened. Not recomputed if the tab is later drag-reordered
    /// (see docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    tab_number: u32,
    /// Lazily built on first render (`TerminalPanel::new` takes no `cx`, and
    /// the handle needs `self.terminal.read(cx)` to get the shared `Term`).
    scrollbar_handle: Option<TerminalScrollbarHandle>,
}
```

And `new` (around line 107-114):

```rust
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

- [ ] **Step 2: Render the prefix in `title()`**

In `src/panels/terminal.rs`, change the first line of `title()` (around line 242):

```rust
    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = format!("{}-{}", self.tab_number, self.terminal.read(cx).title());
        div()
```

(the rest of `title()` — the close button etc. — is unchanged)

- [ ] **Step 3: Wire `open_local_with` (src/workspace.rs:572-609)**

Change:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
        });
        self._subscriptions.push(tab_count_sub);
```

to:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let tab_count_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(tab_count_sub);
```

- [ ] **Step 4: Wire `open_ssh` (src/workspace.rs:697-769)**

Change:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let closed_config = config.clone();
        let closed_term = term_weak.clone();
        let closed_key = key.clone();
        let closed_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
            this.release_ssh_tab_number(&closed_key, tab_number);
        });
        self._subscriptions.push(closed_sub);
```

to:

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
```

Note: `tab_number` here is the pre-existing, *different* per-host SSH dedup number (`let tab_number = self.allocate_ssh_tab_number(&key);` at line 706, used for the `"{display_name}:{n}"` title suffix) — it's untouched. `tab_seq` is the new, unrelated global sequence number this plan adds. Both end up in the same rendered title (e.g. `1-myhost:2`), answering different questions.

- [ ] **Step 5: Wire `open_telnet` (src/workspace.rs:944-965)**

Change:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
        });
        self._subscriptions.push(tab_count_sub);
```

to:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let tab_count_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(tab_count_sub);
```

- [ ] **Step 6: Wire `open_serial` (src/workspace.rs:969-990)**

Change:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let tab_count_sub = cx.subscribe_in(&panel, window, |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
        });
        self._subscriptions.push(tab_count_sub);
```

to:

```rust
        let tab_seq = self.allocate_tab_number();
        let panel = cx.new(|_cx| TerminalPanel::new(terminal, tab_seq));
        let tab_count_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, _window, _cx| {
            let TerminalPanelEvent::Closed = event;
            this.tab_count = this.tab_count.saturating_sub(1);
            this.release_tab_number(tab_seq);
        });
        self._subscriptions.push(tab_count_sub);
```

- [ ] **Step 7: Build and run the full test suite**

Run: `cargo build`
Expected: clean build, no warnings about unused `allocate_tab_number`/`release_tab_number` (now used at all four call sites) and no leftover `TerminalPanel::new` arity errors.

Run: `cargo test --lib`
Expected: all tests pass (no test constructs `TerminalPanel` or calls the four `open_*` methods directly, so no test code needs updating — confirmed by the earlier grep).

- [ ] **Step 8: Manual verification**

Run the app (`cargo run`) and check by hand (no automated/screenshot check, per this project's convention):
- Open a local shell tab: title reads `1-<name>`.
- Open two more tabs (any kind, e.g. one more local + one SSH): titles read `2-...` and `3-...` (SSH one keeps its own `:n` host-dedup suffix too, e.g. `3-myhost:1`).
- Close the `2-...` tab, then open a new one: the new tab takes `2-...` again (reused, not `4-...`).
- Press `Ctrl+1`/`Ctrl+2`/`Ctrl+3` (or `Cmd+` on macOS): confirm each jumps to the tab currently in that visual slot — unaffected by this change.
- Drag a tab to a different position: confirm its printed number does *not* change (the accepted limitation).

- [ ] **Step 9: Commit**

```bash
git add src/panels/terminal.rs src/workspace.rs
git commit -m "$(cat <<'EOF'
feat: prefix terminal tab titles with a sequence number

EOF
)"
```
