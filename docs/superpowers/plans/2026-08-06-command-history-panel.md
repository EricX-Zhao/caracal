# Command History panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the pre-existing, stubbed `PanelId::History` ("命令历史") right-sidebar panel as a real, working view of the focused connection's persisted command history.

**Architecture:** A new `CommandHistoryPanel` (plus a `CommandHistoryPlaceholder` for "no terminal focused yet") follows the exact `MonitorPanel`/`StubPanel` precedent: one panel entity cached per connection key in a `Workspace`-level `HashMap<String, AnyView>`, swapped in on terminal focus via each connection-opening method's existing `cx.on_focus` closure, without forcing the right dock to switch to it. The panel reads `command_history::load_for(key)` once at construction and re-reads only on an explicit refresh click; a live substring search box filters the in-memory list on every render.

**Tech Stack:** Rust, gpui, gpui-component (`Input`/`InputState`, `Button`), rust-i18n.

## Global Constraints

- CLAUDE.md §1: `terminal/view.rs` and `command_history.rs` must never import `gpui_component` — only plain gpui/no-gpui. Panel files (`src/panels/*.rs`) and `workspace.rs` are the only places allowed to import `gpui_component`.
- Row click fills the focused terminal's input line via `send_to_focused_terminal(text, execute: false, cx)` — never auto-executes.
- Search is live substring containment, case-insensitive, not prefix matching.
- No live auto-refresh; a manual refresh button only, mirroring Monitor/SFTP.
- The panel does **not** force `right_active` to switch to it on terminal focus — only *which connection's data* is shown follows focus (mirrors `active_monitor`, not `active_sftp`).
- No delete/clear/export actions, no cross-connection search, no context menu, no multi-select (spec's Non-goals).

---

### Task 1: `command_history::filter_entries` pure function

**Files:**
- Modify: `src/command_history.rs`
- Test: same file, `#[cfg(test)] mod tests` block already present

**Interfaces:**
- Produces: `pub fn filter_entries(entries: &[String], query: &str) -> Vec<String>` — case-insensitive substring filter, returns matches newest-first (input `entries` is oldest-first, matching `load_for`'s on-disk order). Empty `query` returns every entry, newest-first. Consumed by `CommandHistoryPanel` in Task 4.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/command_history.rs`, right after the existing `matching_suggestions_*` tests:

```rust
    #[test]
    fn filter_entries_empty_query_returns_all_newest_first() {
        let entries = vec!["ls".to_string(), "git status".to_string(), "pwd".to_string()];
        assert_eq!(
            filter_entries(&entries, ""),
            vec!["pwd".to_string(), "git status".to_string(), "ls".to_string()]
        );
    }

    #[test]
    fn filter_entries_matches_by_case_insensitive_substring() {
        let entries = vec!["git status".to_string(), "ls -la".to_string(), "GIT PUSH".to_string()];
        assert_eq!(
            filter_entries(&entries, "git"),
            vec!["GIT PUSH".to_string(), "git status".to_string()]
        );
    }

    #[test]
    fn filter_entries_no_match_returns_empty() {
        let entries = vec!["ls".to_string(), "pwd".to_string()];
        assert!(filter_entries(&entries, "docker").is_empty());
    }

    #[test]
    fn filter_entries_preserves_non_consecutive_duplicates() {
        // Non-consecutive duplicate entries are allowed by `record_into` (see
        // `record_into_allows_a_non_consecutive_repeat` above) — this is
        // manual browsing, not the suggestion dropdown's deduped
        // `matching_suggestions`, so both copies must appear.
        let entries = vec!["ls".to_string(), "git status".to_string(), "ls".to_string()];
        assert_eq!(filter_entries(&entries, "ls"), vec!["ls".to_string(), "ls".to_string()]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked command_history:: -- --test-threads=1`
Expected: FAIL with "cannot find function `filter_entries`"

- [ ] **Step 3: Implement `filter_entries`**

Add to `src/command_history.rs`, right after `matching_suggestions` (before the `#[cfg(test)]` block):

```rust
/// Pure: substring-filters `entries` (case-insensitive) and returns them
/// newest-first — `entries` itself is oldest-first (matching `load_for`'s
/// on-disk order), so this reverses as it filters. An empty `query` matches
/// everything (unlike `matching_suggestions`'s empty-prefix-matches-nothing
/// rule, which exists so an empty *typed* input line doesn't show every
/// historical command as a live "suggestion" — this is the History panel's
/// manual-browsing search box, where showing the full list by default is
/// the useful behavior, not noise).
pub fn filter_entries(entries: &[String], query: &str) -> Vec<String> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .rev()
        .filter(|entry| query.is_empty() || entry.to_lowercase().contains(&query))
        .cloned()
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked command_history:: -- --test-threads=1`
Expected: PASS (all `command_history::tests::*`, including the 4 new ones)

- [ ] **Step 5: Commit**

```bash
git add src/command_history.rs
git commit -m "feat: add filter_entries for command history panel search"
```

---

### Task 2: `TerminalView::history_key()` accessor

**Files:**
- Modify: `src/terminal/view.rs:582-585` (right after the existing `title()` accessor)

**Interfaces:**
- Consumes: the existing private `history_key: String` field (already set by every `TerminalView` constructor — see `src/terminal/view.rs:217`).
- Produces: `pub fn history_key(&self) -> &str`. Consumed by `Workspace`'s four connection-opening methods in Task 5.

- [ ] **Step 1: Add the accessor**

In `src/terminal/view.rs`, immediately after the existing `title()` method:

```rust
    #[allow(dead_code)] // consumed by CommandHistoryPanel wiring in workspace.rs
    pub fn title(&self) -> &str {
        &self.title
    }

    #[allow(dead_code)] // consumed by CommandHistoryPanel wiring in workspace.rs
    pub fn history_key(&self) -> &str {
        &self.history_key
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: builds cleanly (this is a trivial getter mirroring `title()` — no dedicated unit test, same as `title()` itself has none).

- [ ] **Step 3: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add TerminalView::history_key accessor"
```

---

### Task 3: Locale keys

**Files:**
- Modify: `locales/app.yml` (insert a new `CommandHistory:` top-level block right after the `Monitor:` block, before `Terminal:`)

**Interfaces:**
- Produces: `CommandHistory.title`, `CommandHistory.search_placeholder`, `CommandHistory.refresh_tooltip`, `CommandHistory.empty_state`, `CommandHistory.no_terminal_focused` — all consumed by `CommandHistoryPanel`/`CommandHistoryPlaceholder` in Task 4.

- [ ] **Step 1: Insert the new block**

In `locales/app.yml`, find the end of the `Monitor:` block (the `no_ssh_host` key, immediately before the `Terminal:` block starts) and insert this new block between them:

```yaml
CommandHistory:
  title:
    zh-CN: "命令历史: %{label}"
    en: "Command History: %{label}"
  search_placeholder:
    zh-CN: "搜索历史命令..."
    en: "Search command history..."
  refresh_tooltip:
    zh-CN: "刷新"
    en: "Refresh"
  empty_state:
    zh-CN: "暂无匹配的历史命令"
    en: "No matching command history"
  no_terminal_focused:
    zh-CN: "未聚焦任何终端"
    en: "No terminal focused"
```

- [ ] **Step 2: Verify the file still parses**

Run: `cargo build` (rust-i18n's `i18n!()` macro parses `locales/*.yml` at compile time — a YAML syntax error fails the build)
Expected: builds cleanly

- [ ] **Step 3: Commit**

```bash
git add locales/app.yml
git commit -m "feat: add CommandHistory locale keys"
```

---

### Task 4: `CommandHistoryPanel` + `CommandHistoryPlaceholder`

**Files:**
- Create: `src/panels/command_history_panel.rs`
- Modify: `src/panels/mod.rs` (register the new module)

**Interfaces:**
- Consumes: `command_history::load_for(key: &str) -> Vec<String>`, `command_history::filter_entries(entries: &[String], query: &str) -> Vec<String>` (Task 1); `Workspace::send_to_focused_terminal(&self, text: &str, execute: bool, cx: &mut App)` (already exists, `src/workspace.rs:1264`).
- Produces: `pub struct CommandHistoryPanel` with `pub fn new(history_key: String, label: SharedString, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self`; `pub struct CommandHistoryPlaceholder` with `pub fn new(cx: &mut Context<Self>) -> Self`. Both are `Focusable + Render` (no `Panel`/`PanelEvent` — mirrors `StubPanel`, the direct precedent for a right-dock `AnyView` panel that is *not* part of the tab-dock system, unlike `MonitorPanel`). Consumed by `Workspace`'s panel registry in Task 5.

No dedicated unit tests for this file — it needs a live gpui window to render, matching every other panel in this codebase (zero-test convention, same as `MonitorPanel`'s/`StubPanel`'s own render code).

- [ ] **Step 1: Register the module**

In `src/panels/mod.rs`, insert alphabetically between `activity_bar` and `header`:

```rust
pub mod activity_bar;
pub mod command_history_panel;
pub mod header;
```

- [ ] **Step 2: Write `src/panels/command_history_panel.rs`**

```rust
//! `CommandHistoryPanel`: the right-sidebar "命令历史" panel. Shows one
//! connection's persisted command history (`command_history::load_for`),
//! live-filterable by substring, with click-to-fill (not execute) on any
//! row. See docs/superpowers/specs/2026-08-06-command-history-panel-design.md.

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Sizable};

use crate::command_history;
use crate::panels::icons::{AppIcon, icon};
use crate::workspace::Workspace;

pub struct CommandHistoryPanel {
    focus_handle: FocusHandle,
    history_key: String,
    /// The connection's display label at the moment this panel was first
    /// created for `history_key` — set once, like `MonitorPanel::label`,
    /// and not refreshed on later focus events (see `Workspace::show_history`'s
    /// doc comment for why: multiple tabs can share one `history_key`).
    label: SharedString,
    entries: Vec<String>,
    search_query: Entity<InputState>,
    /// Back-reference so a row click can send its text to whichever
    /// terminal currently has focus — mirrors `QuickCommandsPanel.workspace`.
    workspace: WeakEntity<Workspace>,
}

impl CommandHistoryPanel {
    pub fn new(
        history_key: String,
        label: SharedString,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = command_history::load_for(&history_key);
        let search_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("CommandHistory.search_placeholder"))
        });
        Self {
            focus_handle: cx.focus_handle(),
            history_key,
            label,
            entries,
            search_query,
            workspace,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.entries = command_history::load_for(&self.history_key);
        cx.notify();
    }

    fn send(&self, line: &str, cx: &mut App) {
        let _ = self.workspace.update(cx, |ws, cx| {
            ws.send_to_focused_terminal(line, false, cx);
        });
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .child(rust_i18n::t!("CommandHistory.title", label = self.label.clone())),
            )
            .child(
                Button::new("command-history-refresh")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Refresh))
                    .tooltip(rust_i18n::t!("CommandHistory.refresh_tooltip"))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| this.refresh(cx))),
            )
    }

    /// One history row. `idx` (the row's position in the already-filtered,
    /// newest-first list) is the element id suffix rather than a hash of
    /// `line`'s content, because non-consecutive duplicate entries are a
    /// real, valid case (see `command_history.rs`'s
    /// `record_into_allows_a_non_consecutive_repeat` test) and a
    /// content-hash id would collide between two identical entries; `idx`
    /// is always unique within one render pass.
    fn render_row(idx: usize, line: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let line_for_click = line.clone();
        div()
            .id(SharedString::from(format!("ch-row-{idx}")))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .min_w(px(0.0))
            .overflow_hidden()
            .text_ellipsis()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(line)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.send(&line_for_click, cx);
            }))
    }
}

impl Focusable for CommandHistoryPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandHistoryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search_query.read(cx).value().to_string();
        let filtered = command_history::filter_entries(&self.entries, &query);
        let is_empty = filtered.is_empty();

        let mut list = div()
            .id("ch-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll();
        if is_empty {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_full()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(rust_i18n::t!("CommandHistory.empty_state")),
            );
        } else {
            for (idx, line) in filtered.into_iter().enumerate() {
                list = list.child(Self::render_row(idx, line, cx));
            }
        }

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_header(cx))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .child(
                        Input::new(&self.search_query)
                            .prefix(icon(AppIcon::Search).text_color(cx.theme().muted_foreground))
                            .w_full(),
                    ),
            )
            .child(list)
    }
}

// --- placeholder for when no terminal has ever been focused -----------------

pub struct CommandHistoryPlaceholder {
    focus_handle: FocusHandle,
}

impl CommandHistoryPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for CommandHistoryPlaceholder {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CommandHistoryPlaceholder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("CommandHistory.no_terminal_focused"))
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: builds cleanly (the new module isn't wired into `Workspace` yet, so it's unused — `cargo build` will warn `dead_code` on the whole file at this point; that's expected and resolved by Task 5, not a bug to fix now).

- [ ] **Step 4: Commit**

```bash
git add src/panels/mod.rs src/panels/command_history_panel.rs
git commit -m "feat: add CommandHistoryPanel and CommandHistoryPlaceholder"
```

---

### Task 5: Wire the panel into `Workspace`'s registry

**Files:**
- Modify: `src/workspace.rs`

**Interfaces:**
- Consumes: `CommandHistoryPanel::new` and `CommandHistoryPlaceholder::new` (Task 4), `TerminalView::history_key()` and the existing `TerminalView::title()` (Task 2 / pre-existing).
- Produces: the `PanelId::History` slot resolves to a real, per-connection panel; no other module depends on anything new here (this is the final integration task).

- [ ] **Step 1: Add the import**

In `src/workspace.rs`, alongside the existing panel imports (`src/workspace.rs:44-56`), add:

```rust
use crate::panels::command_history_panel::{CommandHistoryPanel, CommandHistoryPlaceholder};
```

- [ ] **Step 2: Add the registry fields**

In the `Workspace` struct, immediately after the existing `active_monitor: Option<String>` field (`src/workspace.rs:326`), add:

```rust
    /// One 命令历史 panel per connection key (created on first use, reused
    /// after) — mirrors `monitor_panels` field-for-field. Unlike Monitor,
    /// not SSH-only: `history_key` covers all 4 connection types.
    history_panels: HashMap<String, AnyView>,
    /// Shown in the History slot before any terminal has ever been focused.
    history_placeholder: AnyView,
    /// Connection key whose history panel the `PanelId::History` slot
    /// resolves to. Like `active_monitor` (not `active_sftp`), updating
    /// this does NOT force `right_active` to switch to `PanelId::History`.
    active_history: Option<String>,
```

- [ ] **Step 3: Construct the placeholder and stop building a History stub**

In `Workspace::new`, right after the existing `let monitor_placeholder: AnyView = cx.new(MonitorPlaceholder::new).into();` (`src/workspace.rs:501`), add:

```rust
        let history_placeholder: AnyView = cx.new(CommandHistoryPlaceholder::new).into();
```

Then change the stub-panels loop (`src/workspace.rs:505-509`) from:

```rust
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [PanelId::Network, PanelId::History] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid, cx)).into();
            stub_panels.insert(pid, view);
        }
```

to:

```rust
        let mut stub_panels: HashMap<PanelId, AnyView> = HashMap::new();
        for pid in [PanelId::Network] {
            let view: AnyView = cx.new(|cx| StubPanel::new(pid, cx)).into();
            stub_panels.insert(pid, view);
        }
```

- [ ] **Step 4: Initialize the new fields in the `Self { ... }` literal**

Right after the existing `active_monitor: None,` line (`src/workspace.rs:539`), add:

```rust
            history_panels: HashMap::new(),
            history_placeholder,
            active_history: None,
```

- [ ] **Step 5: Add `show_history`/`show_history_placeholder`**

Right after the existing `show_monitor_placeholder` method (`src/workspace.rs:1349-1352`), add:

```rust
    /// Bind the History slot to `key`'s connection (creating the panel once
    /// if needed, reusing it after). Mirrors `show_monitor`: does NOT force
    /// `right_active` — see the `active_history` field's doc comment.
    /// `label` is only used the first time `key`'s panel is created (see
    /// `CommandHistoryPanel.label`'s doc comment) — a later call with a
    /// different `label` for an already-cached `key` (e.g. a second SSH tab
    /// to the same host, with a different tab number in its title) is a
    /// no-op on the label, same limitation `MonitorPanel.label` already has.
    fn show_history(&mut self, key: String, label: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.history_panels.contains_key(&key) {
            let workspace = cx.entity().downgrade();
            let panel: AnyView = cx
                .new(|cx| CommandHistoryPanel::new(key.clone(), label.into(), workspace, window, cx))
                .into();
            self.history_panels.insert(key.clone(), panel);
        }
        self.active_history = Some(key);
        cx.notify();
    }

    /// Detach the History slot from any connection so it resolves to the
    /// "no terminal focused" placeholder. Mirrors `show_monitor_placeholder`.
    fn show_history_placeholder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_history = None;
        cx.notify();
    }
```

- [ ] **Step 6: Add the `resolve` arm**

Change `resolve` (`src/workspace.rs:1535-1553`) from:

```rust
    fn resolve(&self, id: PanelId) -> Option<AnyView> {
        match id {
            PanelId::Sftp => Some(
                self.active_sftp
                    .as_ref()
                    .and_then(|k| self.sftp_panels.get(k).cloned())
                    .unwrap_or_else(|| self.sftp_placeholder.clone()),
            ),
            PanelId::Monitor => Some(
                self.active_monitor
                    .as_ref()
                    .and_then(|k| self.monitor_panels.get(k).cloned())
                    .unwrap_or_else(|| self.monitor_placeholder.clone()),
            ),
            PanelId::Sessions => Some(self.sessions_panel.clone()),
            PanelId::Security => Some(self.security_auth_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
        }
    }
```

to:

```rust
    fn resolve(&self, id: PanelId) -> Option<AnyView> {
        match id {
            PanelId::Sftp => Some(
                self.active_sftp
                    .as_ref()
                    .and_then(|k| self.sftp_panels.get(k).cloned())
                    .unwrap_or_else(|| self.sftp_placeholder.clone()),
            ),
            PanelId::Monitor => Some(
                self.active_monitor
                    .as_ref()
                    .and_then(|k| self.monitor_panels.get(k).cloned())
                    .unwrap_or_else(|| self.monitor_placeholder.clone()),
            ),
            PanelId::History => Some(
                self.active_history
                    .as_ref()
                    .and_then(|k| self.history_panels.get(k).cloned())
                    .unwrap_or_else(|| self.history_placeholder.clone()),
            ),
            PanelId::Sessions => Some(self.sessions_panel.clone()),
            PanelId::Security => Some(self.security_auth_panel.clone()),
            other => self.stub_panels.get(&other).cloned(),
        }
    }
```

- [ ] **Step 7: Hook `open_local_with`'s focus closure**

In `open_local_with` (`src/workspace.rs:605-609`), change:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
```

to:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
            if let Some(t) = term_weak.upgrade() {
                let key = t.read(cx).history_key().to_string();
                let label = t.read(cx).title().to_string();
                this.show_history(key, label, window, cx);
            }
        });
```

- [ ] **Step 8: Hook `open_ssh`'s focus closure**

In `open_ssh` (`src/workspace.rs:776-794`), change:

```rust
        let sub = cx.on_focus(&handle, window, {
            let term_weak = term_weak.clone();
            move |this, window, cx| {
                this.set_active_title_from(&term_weak, cx);
                // Only show SFTP/monitor once the session is actually
                // cached — while a tab is still connecting, this closure
                // fires immediately (the tab is focused on creation) and
                // must not trigger `show_sftp`'s own on-demand
                // synchronous connect, which would race the background
                // dial below.
                if this.ssh_sessions.contains_key(&follow.key()) {
                    this.show_sftp(follow.clone(), window, cx);
                    this.show_monitor(follow.clone(), window, cx);
                } else {
                    this.show_sftp_placeholder(window, cx);
                    this.show_monitor_placeholder(window, cx);
                }
            }
        });
```

to:

```rust
        let sub = cx.on_focus(&handle, window, {
            let term_weak = term_weak.clone();
            move |this, window, cx| {
                this.set_active_title_from(&term_weak, cx);
                // Only show SFTP/monitor once the session is actually
                // cached — while a tab is still connecting, this closure
                // fires immediately (the tab is focused on creation) and
                // must not trigger `show_sftp`'s own on-demand
                // synchronous connect, which would race the background
                // dial below.
                if this.ssh_sessions.contains_key(&follow.key()) {
                    this.show_sftp(follow.clone(), window, cx);
                    this.show_monitor(follow.clone(), window, cx);
                } else {
                    this.show_sftp_placeholder(window, cx);
                    this.show_monitor_placeholder(window, cx);
                }
                // Unlike SFTP/Monitor, History doesn't need a cached SSH
                // session — the connection's own key/title are known
                // immediately, connecting or not.
                if let Some(t) = term_weak.upgrade() {
                    let key = t.read(cx).history_key().to_string();
                    let label = t.read(cx).title().to_string();
                    this.show_history(key, label, window, cx);
                }
            }
        });
```

- [ ] **Step 9: Hook `open_telnet`'s focus closure**

In `open_telnet` (`src/workspace.rs:1006-1010`), change:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
        });
```

to:

```rust
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
            this.show_monitor_placeholder(window, cx);
            if let Some(t) = term_weak.upgrade() {
                let key = t.read(cx).history_key().to_string();
                let label = t.read(cx).title().to_string();
                this.show_history(key, label, window, cx);
            }
        });
```

- [ ] **Step 10: Hook `open_serial`'s focus closure**

In `open_serial` (`src/workspace.rs:1036-1040`), apply the identical change as Step 9 (same before/after text).

- [ ] **Step 11: Verify it builds and existing tests still pass**

Run: `cargo build && cargo test --locked`
Expected: builds cleanly, no warnings about the new panel/fields being unused, all existing tests still pass (203+4 = 207 tests, the 4 new ones from Task 1).

- [ ] **Step 12: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: wire CommandHistoryPanel into the History activity-bar slot"
```

---

### Task 6: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full build + test suite**

Run: `cargo build && cargo test --locked`
Expected: clean build, zero warnings, all tests pass.

- [ ] **Step 2: Manual smoke test** (ask the user to perform this — GUI verification is not something to screenshot-drive; see the project's own convention)

Confirm each of these (from the design spec's Testing section):
1. Opening History for a connection that already has recorded commands shows them, newest-first.
2. Typing in the search box filters live by substring (case-insensitive).
3. Clicking a row fills the focused terminal's input line without executing it.
4. The refresh button picks up a command recorded after the panel was first shown (run a command in the terminal, click refresh, it appears).
5. Switching focus between two different connections' terminal tabs shows each one's own distinct history (proves per-connection-key scoping).
6. The panel does not force the right dock to switch to it when a terminal gains focus (consistent with Monitor, unlike SFTP) — the right dock should stay wherever the user last left it (e.g. Sessions) unless they click the History activity-bar icon themselves.
7. Before any terminal has ever been focused (fresh app launch, click the History icon immediately), the placeholder ("未聚焦任何终端" / "No terminal focused") shows instead of a panel.

- [ ] **Step 3: Report results to the user**

No commit for this task — it's verification only. If the manual smoke test surfaces a bug, fix it as a follow-up commit before considering the plan complete.
