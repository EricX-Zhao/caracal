# Quick Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a minimal quick-commands feature: a bottom drawer (toggled from a new
status-bar icon) listing saved command snippets, sent to the currently-focused terminal tab
in "execute" (send + Enter) or "append" (paste only) mode. The same status-bar work also
adds a live cursor-position readout (`row:col`) for the focused terminal.

**Architecture:** New `src/quick_commands.rs` mirrors `src/config.rs`/`src/settings.rs`'s
established load/save/path shape for a new `quick_commands.toml`. `TerminalView` gains two
small public methods (`send_text`, `cursor_position`) with no changes to its existing
behavior. `Workspace` gains a `focused_terminal: Option<WeakEntity<TerminalView>>` (set at
the single existing `set_active_title_from` call site, which already fires from all 5
terminal-creation sites — no new per-site wiring needed), a `show_quick_commands: bool`
toggle, and owns a `QuickCommandsPanel` entity embedded directly in `render()` (not through
the `PanelId`/activity-bar system — this is a new bottom region, not a side-dock slot).

**Tech Stack:** Rust, gpui + gpui_platform (git rev pinned in `Cargo.toml`), gpui-component
(`Input`/`InputState`), `serde`/`toml` (already a dependency).

## Global Constraints

- Quick commands persist to a **new**, separate `~/.config/caracal/quick_commands.toml` —
  never folded into `connections.toml` or `settings.toml`.
- v1 scope is deliberately minimal: flat list, no categories, no search, no sort modes, no
  view-mode switcher, no pin, no color/icon tags, no import, no `{{variable}}` templating.
  Do not add any of these — they were explicitly deferred.
- The add/edit form is **inline** (toggle-visibility, embedded in the panel), not a
  standalone window — matches `saved_connections.rs`'s `ConnForm` idiom, not
  `settings_window.rs`'s window-opening one.
- The drawer toggle icon lives in the **status bar** (`Workspace::render_status_bar`), not
  the header/menu bar — no changes to `src/panels/header.rs`.
- The drawer has a **fixed height** (~220px), not resizable.
- Full spec: `docs/superpowers/specs/2026-07-07-quick-commands-design.md`. Roadmap context:
  `docs/reference/nyaterm-gap-roadmap.md`.

---

### Task 1: `src/quick_commands.rs` — data model and persistence

**Files:**
- Create: `src/quick_commands.rs`
- Modify: `src/main.rs` (add `mod quick_commands;`)

**Interfaces:**
- Produces: `QuickCommand { id: String, label: String, command: String, execution_mode:
  ExecutionMode }`, `ExecutionMode { Execute, Append }` (default `Execute`),
  `quick_commands::quick_commands_path() -> PathBuf`, `quick_commands::load() -> Vec<QuickCommand>`,
  `quick_commands::save(&[QuickCommand]) -> anyhow::Result<()>` — consumed by Task 4
  (`quick_commands_panel.rs`).

- [ ] **Step 1: Write the failing tests**

Create `src/quick_commands.rs` with just this much first:

```rust
//! Persisted quick commands: saved command snippets sent to the focused
//! terminal from the bottom quick-commands drawer. Plain Rust — no
//! `gpui_component` here (CLAUDE.md §1 boundary).
//!
//! Stored at `$XDG_CONFIG_HOME/caracal/quick_commands.toml` (else
//! `~/.config/caracal/quick_commands.toml`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_execution_mode_is_execute() {
        let toml_text = r#"
            [[commands]]
            id = "id-1"
            label = "List files"
            command = "ls -la"
        "#;
        let file: QuickCommandsFile = toml::from_str(toml_text).expect("must parse");
        assert_eq!(file.commands.len(), 1);
        assert_eq!(file.commands[0].execution_mode, ExecutionMode::Execute);
    }

    #[test]
    fn round_trip_preserves_fields() {
        let commands = vec![QuickCommand {
            id: "id-1".to_string(),
            label: "Docker ps".to_string(),
            command: "docker ps".to_string(),
            execution_mode: ExecutionMode::Append,
        }];
        let file = QuickCommandsFile {
            commands: commands.clone(),
        };
        let text = toml::to_string_pretty(&file).expect("serialize");
        let parsed: QuickCommandsFile = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].id, "id-1");
        assert_eq!(parsed.commands[0].label, "Docker ps");
        assert_eq!(parsed.commands[0].command, "docker ps");
        assert_eq!(parsed.commands[0].execution_mode, ExecutionMode::Append);
    }

    #[test]
    fn empty_file_yields_empty_commands() {
        let file: QuickCommandsFile = toml::from_str("").expect("empty file must parse");
        assert!(file.commands.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib quick_commands:: 2>&1 | tail -30`
Expected: FAIL to compile — types not found, module not registered in `main.rs`. Register
it now: in `src/main.rs`, current state:

```rust
mod assets;
mod config;
mod panels;
mod settings;
mod terminal;
mod workspace;
```

Replace with:

```rust
mod assets;
mod config;
mod panels;
mod quick_commands;
mod settings;
mod terminal;
mod workspace;
```

Re-run. Expected: FAIL — `QuickCommand`/`ExecutionMode`/`QuickCommandsFile` not defined.

- [ ] **Step 3: Implement the types and persistence**

Add above the `#[cfg(test)]` block in `src/quick_commands.rs`:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How a quick command reaches the terminal: sent + Enter, or just placed on
/// the input line for the user to review/edit first.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    Execute,
    Append,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        ExecutionMode::Execute
    }
}

/// One saved quick command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuickCommand {
    pub id: String,
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub execution_mode: ExecutionMode,
}

/// The whole persisted quick-commands file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct QuickCommandsFile {
    #[serde(default)]
    pub commands: Vec<QuickCommand>,
}

/// `~/.config/caracal/quick_commands.toml`.
pub fn quick_commands_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("caracal").join("quick_commands.toml")
}

/// Load quick commands. Missing file → empty. A parse error is logged and
/// also yields empty, so a corrupt file never crashes startup.
pub fn load() -> Vec<QuickCommand> {
    let path = quick_commands_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    match toml::from_str::<QuickCommandsFile>(&text) {
        Ok(file) => file.commands,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Persist quick commands, creating the parent directory if needed.
pub fn save(commands: &[QuickCommand]) -> anyhow::Result<()> {
    let path = quick_commands_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = QuickCommandsFile {
        commands: commands.to_vec(),
    };
    let text = toml::to_string_pretty(&file)?;
    std::fs::write(&path, text)?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib quick_commands:: -- --nocapture`
Expected: PASS — all 3 tests succeed.

- [ ] **Step 5: Run the full suite to check for regressions**

Run: `cargo test 2>&1 | tail -15`
Expected: all pre-existing tests still pass (53 before this task), plus the 3 new ones (56
total), 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/quick_commands.rs src/main.rs
git commit -m "feat: add quick_commands.rs persisted command snippets"
```

---

### Task 2: `TerminalView` — `send_text` and `cursor_position`

**Files:**
- Modify: `src/terminal/view.rs`

**Interfaces:**
- Produces: `TerminalView::pub fn send_text(&self, text: &str, execute: bool)`,
  `TerminalView::pub fn cursor_position(&self) -> (usize, usize)` — consumed by Task 3
  (`cursor_position`, via `Workspace::render_status_bar`) and Task 4 (`send_text`, via
  `Workspace::send_to_focused_terminal`).

No new tests for this task: `send_text` writes to a live PTY backend and isn't meaningfully
unit-testable in isolation, consistent with the existing `send_input`/`paste_from_clipboard`
methods (private, also untested) it's modeled on. `cursor_position` reads through
`self.term.lock().renderable_content()`, the exact same call `terminal/grid_snapshot.rs`'s
already-tested `snapshot_content` uses — a `TerminalView`-level test would need the gpui test
harness (`#[gpui::test]`, a full `TerminalView` construction) to exercise, which is
disproportionate for two lines of arithmetic already covered in spirit by
`grid_snapshot.rs`'s existing cursor tests. Skip it; verify with `cargo build`.

- [ ] **Step 1: Add `send_text`**

Current state (`src/terminal/view.rs`, right after `mark_exited`):

```rust
    pub fn mark_exited(&mut self) {
        self.exited = true;
    }

    // --- Font configuration interface (for a future settings UI) ---
```

Replace with:

```rust
    pub fn mark_exited(&mut self) {
        self.exited = true;
    }

    /// Send `text` into this terminal as if pasted (honours the term's
    /// `BRACKETED_PASTE` mode, same encoder `paste_from_clipboard` uses). If
    /// `execute` is `true`, a trailing Enter (`\r`) is sent after the encoded
    /// payload. Used by the quick-commands panel to inject saved command
    /// snippets. No-op for empty `text` (matches `encode_paste`'s own
    /// empty-string `None` behavior).
    pub fn send_text(&self, text: &str, execute: bool) {
        let mode: TermMode = *self.term.lock().mode();
        let Some(mut bytes) = encode_paste(text, mode, PastePayload::Clipboard) else {
            return;
        };
        if execute {
            bytes.push(b'\r');
        }
        self.backend.write(&bytes);
        self.term
            .lock()
            .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
    }

    /// The cursor's current (row, column), 0-indexed, adjusted for
    /// scrollback (`display_offset`) exactly like
    /// `grid_snapshot::snapshot_content`'s own cursor computation. Used by
    /// the status bar to show `row+1:col+1`.
    pub fn cursor_position(&self) -> (usize, usize) {
        let term = self.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset as i32;
        let row = (content.cursor.point.line.0 + display_offset).max(0) as usize;
        let col = content.cursor.point.column.0;
        (row, col)
    }

    // --- Font configuration interface (for a future settings UI) ---
```

- [ ] **Step 2: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -40`
Expected: builds successfully. `TermMode`, `encode_paste`, `PastePayload` are already
imported at the top of this file (used by the existing `paste_from_clipboard`) — no new
`use` lines needed. If `content.cursor.point.line.0`/`.column.0` don't match the actual
`alacritty_terminal` field names in this pinned version, cross-check against
`src/terminal/grid_snapshot.rs`'s `snapshot_content` function (lines ~82-90), which computes
`cur_row`/`cur_col` the exact same way — that's the ground truth to copy, not this plan.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all 56 tests still pass (no new tests added by this task).

- [ ] **Step 4: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add TerminalView::send_text and cursor_position"
```

---

### Task 3: `Workspace` — track the focused terminal, expose `send_to_focused_terminal`

**Files:**
- Modify: `src/workspace.rs`

**Interfaces:**
- Consumes: `TerminalView::send_text` (Task 2).
- Produces: `Workspace.focused_terminal: Option<WeakEntity<TerminalView>>`,
  `Workspace::pub fn send_to_focused_terminal(&self, text: &str, execute: bool, cx: &App)` —
  consumed by Task 4 (`QuickCommandsPanel::send`) and by `render_status_bar` in Task 4 (reads
  `focused_terminal` directly for the cursor-position display).

This task touches exactly one existing method (`set_active_title_from`), which already runs
on focus for all 5 terminal-creation sites — no changes needed at those 5 call sites
themselves. No new tests: this is view-tree wiring, consistent with the rest of
`workspace.rs`'s terminal-creation code having no tests of its own.

- [ ] **Step 1: Add the field**

Current state (`src/workspace.rs`, in the `Workspace` struct, right after the
`settings_window` field):

```rust
    /// The open settings window, if any — re-triggering the menu item
    /// focuses this instead of opening a duplicate.
    settings_window: Option<WindowHandle<Root>>,
```

Replace with:

```rust
    /// The open settings window, if any — re-triggering the menu item
    /// focuses this instead of opening a duplicate.
    settings_window: Option<WindowHandle<Root>>,
    /// The most recently focused terminal, if any — used to send quick
    /// commands and to show the status bar's cursor-position readout. Set in
    /// `set_active_title_from`, which already runs on focus for every
    /// terminal. A dead weak ref (tab closed) reads as "nothing focused"
    /// until another tab is focused.
    focused_terminal: Option<WeakEntity<TerminalView>>,
```

- [ ] **Step 2: Initialize the field in `Workspace::new`**

Current state (the `Self { .. }` literal):

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            saved_panel: saved.into(),
```

Replace with:

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            focused_terminal: None,
            saved_panel: saved.into(),
```

- [ ] **Step 3: Update `set_active_title_from` and add `send_to_focused_terminal`**

Current state:

```rust
    /// Update the header's active title from a (possibly-dropped) terminal.
    fn set_active_title_from(&mut self, term: &WeakEntity<TerminalView>, cx: &App) {
        if let Some(t) = term.upgrade() {
            self.active_title = t.read(cx).title().to_string().into();
        }
    }
```

Replace with:

```rust
    /// Update the header's active title and the focused-terminal pointer
    /// (used by quick commands and the status bar's cursor-position display)
    /// from a (possibly-dropped) terminal.
    fn set_active_title_from(&mut self, term: &WeakEntity<TerminalView>, cx: &App) {
        self.focused_terminal = Some(term.clone());
        if let Some(t) = term.upgrade() {
            self.active_title = t.read(cx).title().to_string().into();
        }
    }

    /// Send `text` to the currently-focused terminal tab, if any, per
    /// `execute`. No-op if no terminal is focused or its weak ref has died.
    /// Called by [`crate::panels::quick_commands_panel::QuickCommandsPanel`].
    pub fn send_to_focused_terminal(&self, text: &str, execute: bool, cx: &App) {
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        terminal.read(cx).send_text(text, execute);
    }
```

- [ ] **Step 4: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -40`
Expected: builds successfully (an unused-method warning on `send_to_focused_terminal` is
expected and fine — Task 4 wires its only caller).

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all 56 tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: track the focused terminal on Workspace, add send_to_focused_terminal"
```

---

### Task 4: `QuickCommandsPanel` + status bar (icon toggle, cursor position, drawer)

**Files:**
- Create: `src/panels/quick_commands_panel.rs`
- Modify: `src/panels/mod.rs` (register the new module)
- Modify: `src/workspace.rs` (imports, `show_quick_commands` + `quick_commands_panel`
  fields, `render_status_bar` rewrite, drawer insertion in `render()`)

**Interfaces:**
- Consumes: `quick_commands::{QuickCommand, ExecutionMode, load, save}` (Task 1),
  `Workspace::send_to_focused_terminal` (Task 3), `TerminalView::cursor_position` (Task 2),
  `AppIcon::QuickCmd` (pre-existing, `src/panels/icons.rs` — already maps to
  `IconName::Asterisk`, no changes needed there).
- Produces: `QuickCommandsPanel::new(workspace: WeakEntity<Workspace>, window: &mut Window,
  cx: &mut Context<Self>) -> Self`.

No automated tests for this task — it's UI wiring (a new panel + status-bar layout), and the
codebase's `panels/*.rs` render code isn't unit-tested anywhere else either. Verification is
`cargo build` plus the manual smoke test in Task 5.

- [ ] **Step 1: Write `QuickCommandsPanel`**

Create `src/panels/quick_commands_panel.rs`:

```rust
//! `QuickCommandsPanel`: the bottom-drawer quick-commands list, toggled from
//! the status bar's quick-commands icon (see `Workspace::render_status_bar`).
//! Minimal v1 (per the design spec): flat list, inline add/edit form, two
//! send modes (execute / append) — no categories, search, sort, pin, tags,
//! import, or variable templating.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};
use crate::quick_commands::{self, ExecutionMode, QuickCommand};
use crate::workspace::Workspace;

/// The inline "add/edit quick command" form.
struct QuickCommandForm {
    label: Entity<InputState>,
    command: Entity<InputState>,
    execution_mode: ExecutionMode,
    /// `Some(id)` when editing an existing command in place; `None` when
    /// adding a new one.
    edit_id: Option<String>,
}

pub struct QuickCommandsPanel {
    workspace: WeakEntity<Workspace>,
    commands: Vec<QuickCommand>,
    form: Option<QuickCommandForm>,
}

impl QuickCommandsPanel {
    /// No `window` param — unlike most panel constructors in this codebase,
    /// nothing here needs it: the form's `InputState`s are created lazily in
    /// `toggle_form`/`open_edit_form`, which each receive their own `window`
    /// from their click-handler call site (matches `StubPanel::new`'s
    /// window-less signature, used for the same reason).
    pub fn new(workspace: WeakEntity<Workspace>, _cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            commands: quick_commands::load(),
            form: None,
        }
    }

    fn generate_id() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("qc-{}", nanos)
    }

    fn persist(&self) {
        if let Err(e) = quick_commands::save(&self.commands) {
            log::error!("failed to save quick commands: {e}");
        }
    }

    fn toggle_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() {
            self.form = None;
        } else {
            self.form = Some(QuickCommandForm {
                label: cx.new(|cx| InputState::new(window, cx).placeholder("名称")),
                command: cx.new(|cx| InputState::new(window, cx).placeholder("命令")),
                execution_mode: ExecutionMode::Execute,
                edit_id: None,
            });
        }
        cx.notify();
    }

    fn open_edit_form(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cmd) = self.commands.iter().find(|c| c.id == id) else {
            return;
        };
        self.form = Some(QuickCommandForm {
            label: cx.new(|cx| InputState::new(window, cx).default_value(cmd.label.clone())),
            command: cx.new(|cx| InputState::new(window, cx).default_value(cmd.command.clone())),
            execution_mode: cmd.execution_mode,
            edit_id: Some(id),
        });
        cx.notify();
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let label = form.label.read(cx).value().to_string();
        let command = form.command.read(cx).value().to_string();
        if label.trim().is_empty() || command.trim().is_empty() {
            return;
        }
        if let Some(id) = form.edit_id.clone() {
            if let Some(existing) = self.commands.iter_mut().find(|c| c.id == id) {
                existing.label = label;
                existing.command = command;
                existing.execution_mode = form.execution_mode;
            }
        } else {
            self.commands.push(QuickCommand {
                id: Self::generate_id(),
                label,
                command,
                execution_mode: form.execution_mode,
            });
        }
        self.form = None;
        self.persist();
        cx.notify();
    }

    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commands.retain(|c| c.id != id);
        self.persist();
        cx.notify();
    }

    fn send(&self, cmd: &QuickCommand, cx: &App) {
        let execute = matches!(cmd.execution_mode, ExecutionMode::Execute);
        let _ = self.workspace.read_with(cx, |ws, cx| {
            ws.send_to_focused_terminal(&cmd.command, execute, cx);
        });
    }

    fn set_form_mode(&mut self, mode: ExecutionMode, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            form.execution_mode = mode;
            cx.notify();
        }
    }

    /// One toggle pill's styling — shared visual idiom with
    /// `saved_connections.rs`'s connection-type pills.
    fn pill(id: &'static str, label: &str, active: bool, cx: &App) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active {
                cx.theme().primary
            } else {
                cx.theme().accent
            })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .child(label.to_string())
    }

    fn render_form(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let form = self.form.as_ref()?;
        let is_execute = matches!(form.execution_mode, ExecutionMode::Execute);
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .m_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(Input::new(&form.label))
                .child(Input::new(&form.command))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            Self::pill("qc-mode-execute", "执行", is_execute, cx).on_click(
                                cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                    this.set_form_mode(ExecutionMode::Execute, cx);
                                }),
                            ),
                        )
                        .child(
                            Self::pill("qc-mode-append", "追加", !is_execute, cx).on_click(
                                cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                    this.set_form_mode(ExecutionMode::Append, cx);
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .justify_end()
                        .child(
                            div()
                                .id("qc-form-cancel")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .hover(|s| s.bg(cx.theme().accent))
                                .child("取消")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                    this.toggle_form(window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("qc-form-save")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .child("保存")
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _window, cx| {
                                    this.save_form(cx);
                                })),
                        ),
                ),
        )
    }

    /// One command row. Mirrors `saved_connections.rs`'s `render_connection`
    /// structure: a `clickable` sub-div (send) and a separate `action_bar`
    /// sub-div (edit/delete) as SIBLINGS under a non-clickable outer row —
    /// nesting the action icons' `.on_click` inside the row's own `.on_click`
    /// would make both fire on one click, so they must not be nested.
    fn render_row(cmd: &QuickCommand, cx: &mut Context<Self>) -> impl IntoElement {
        let id = cmd.id.clone();
        let cmd_for_send = cmd.clone();
        let id_for_edit = id.clone();
        let id_for_delete = id.clone();
        let mode_label = match cmd.execution_mode {
            ExecutionMode::Execute => "执行",
            ExecutionMode::Append => "追加",
        };

        let action_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .opacity(0.3)
            .child(
                div()
                    .id(SharedString::from(format!("qc-edit-{id_for_edit}")))
                    .p(px(4.0))
                    .rounded_sm()
                    .hover(|s| {
                        s.bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                    })
                    .child(icon(AppIcon::Pencil).text_color(cx.theme().muted_foreground))
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                        this.open_edit_form(id_for_edit.clone(), window, cx);
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("qc-delete-{id_for_delete}")))
                    .p(px(4.0))
                    .rounded_sm()
                    .hover(|s| {
                        s.bg(cx.theme().danger)
                            .text_color(cx.theme().danger_foreground)
                    })
                    .child(icon(AppIcon::Delete).text_color(cx.theme().muted_foreground))
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                        this.delete(&id_for_delete, cx);
                    })),
            );

        let clickable = div()
            .id(SharedString::from(format!("qc-send-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_1()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_sm()
                    .child(cmd.label.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(mode_label),
            )
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.send(&cmd_for_send, cx);
            }));

        div()
            .id(SharedString::from(format!("qc-row-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .py_1()
            .rounded_md()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(clickable)
            .child(action_bar)
    }
}

impl Render for QuickCommandsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = self.render_form(cx);
        let commands = self.commands.clone();
        let is_empty = commands.is_empty();

        // Built with a `for` loop (not `.children(iter().map(...))`) reusing
        // `cx` sequentially each iteration — matches the established pattern
        // in `saved_connections.rs`'s `render_tree`/`render_ungrouped_section`
        // (`for group in ... { tree = tree.child(self.render_group(group, 0, cx)); }`),
        // which avoids passing `&mut Context<Self>` into a `.map()` closure.
        let mut list = div()
            .id("qc-list")
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
                    .child("暂无快捷命令，点 + 添加"),
            );
        } else {
            for cmd in &commands {
                list = list.child(Self::render_row(cmd, cx));
            }
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
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
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("快捷命令"),
                    )
                    .child(
                        div()
                            .id("qc-toggle-form")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(if self.form.is_some() { "取消" } else { "+ 添加" })
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.toggle_form(window, cx);
                            })),
                    ),
            )
            .children(form)
            .child(list)
    }
}
```

- [ ] **Step 2: Register the module**

In `src/panels/mod.rs`, add `pub mod quick_commands_panel;` alongside the other `pub mod`
declarations (read the file first to match its existing style/ordering).

- [ ] **Step 3: Wire `Workspace`**

Current imports (`src/workspace.rs`):

```rust
use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::settings_window::SettingsWindow;
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::settings;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::{FontConfig, TerminalView};
```

Replace with:

```rust
use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::icons::{AppIcon, icon};
use crate::panels::quick_commands_panel::QuickCommandsPanel;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::settings_window::SettingsWindow;
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::settings;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::{FontConfig, TerminalView};
```

Current state (the `focused_terminal` field added in Task 3, plus what follows):

```rust
    /// The most recently focused terminal, if any — used to send quick
    /// commands and to show the status bar's cursor-position readout. Set in
    /// `set_active_title_from`, which already runs on focus for every
    /// terminal. A dead weak ref (tab closed) reads as "nothing focused"
    /// until another tab is focused.
    focused_terminal: Option<WeakEntity<TerminalView>>,
```

Add right after it:

```rust
    /// Whether the bottom quick-commands drawer is open. Closed by default.
    show_quick_commands: bool,
    /// The quick-commands panel shown in the drawer. Owned here (not part of
    /// the `PanelId` side-dock system — this is a new bottom region).
    quick_commands_panel: Entity<QuickCommandsPanel>,
```

Current state (the `Self { .. }` literal in `Workspace::new`, right after Task 3's
`focused_terminal: None,`):

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            focused_terminal: None,
            saved_panel: saved.into(),
```

Replace with:

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            focused_terminal: None,
            show_quick_commands: false,
            quick_commands_panel,
            saved_panel: saved.into(),
```

Just above that `Self { .. }` literal (right after `let body_resize = cx.new(|_|
ResizableState::default());`), add the panel's construction — current state:

```rust
        let body_resize = cx.new(|_| ResizableState::default());

        Self {
```

Replace with:

```rust
        let body_resize = cx.new(|_| ResizableState::default());

        let workspace_handle = cx.entity().downgrade();
        let quick_commands_panel = cx.new(|cx| QuickCommandsPanel::new(workspace_handle, cx));

        Self {
```

- [ ] **Step 4: Rewrite `render_status_bar`, add the toggle method**

Current state:

```rust
    /// Bottom status bar — reserved for future info (connection state, cwd, …).
    /// Empty for now.
    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        div()
            .w_full()
            .h(px(22.0))
            .bg(cx.theme().muted)
            .border_t_1()
            .border_color(cx.theme().border)
    }
```

Replace with:

```rust
    /// Bottom status bar: a left icon cluster (currently just the
    /// quick-commands toggle; structured so a second icon can be added later
    /// without redesign) and a right-aligned cursor-position readout for the
    /// focused terminal (blank when nothing is focused).
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cursor_text = self
            .focused_terminal
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|t| {
                let (row, col) = t.read(cx).cursor_position();
                format!("{}:{}", row + 1, col + 1)
            })
            .unwrap_or_default();

        div()
            .w_full()
            .h(px(22.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .bg(cx.theme().muted)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("status-quick-commands")
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_color(if self.show_quick_commands {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    })
                    .hover(|s| s.text_color(cx.theme().foreground))
                    .child(icon(AppIcon::QuickCmd))
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.show_quick_commands = !this.show_quick_commands;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(cursor_text),
            )
    }
```

- [ ] **Step 5: Insert the drawer into `render()`**

Current state:

```rust
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(left_bar)
                    .child(body)
                    .child(right_bar),
            )
            .child(status_bar)
            .children(gpui_component::Root::render_notification_layer(window, cx))
            .children(gpui_component::Root::render_dialog_layer(window, cx))
    }
}
```

Replace with:

```rust
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let border = cx.theme().border;
        let show_quick_commands = self.show_quick_commands;
        let quick_commands_panel = self.quick_commands_panel.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(left_bar)
                    .child(body)
                    .child(right_bar),
            )
            .when(show_quick_commands, |d| {
                d.child(
                    div()
                        .w_full()
                        .h(px(220.0))
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(border)
                        .child(quick_commands_panel),
                )
            })
            .child(status_bar)
            .children(gpui_component::Root::render_notification_layer(window, cx))
            .children(gpui_component::Root::render_dialog_layer(window, cx))
    }
}
```

- [ ] **Step 6: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully. Treat this as the real verification, not a formality — this
task has more gpui-API surface than Tasks 1-3 (a new panel with `Input`/`InputState`,
`.hover`/`.opacity`/`.overflow_y_scroll`/`.when`, click handlers). If something doesn't
compile, check the actual method/trait signature in the vendored source
(`~/.cargo/git/checkouts/zed-a70e2ad075855582/*/crates/gpui/src/` for `gpui`,
`~/.cargo/git/checkouts/gpui-component-95ce574d8a0da8b8/*/crates/` for `gpui-component`)
rather than guessing — and cross-check against how `src/panels/saved_connections.rs`'s
`render_connection`/`render_form`/`pill` already use the same patterns (input fields, hover
pills, sibling clickable+action-bar rows), since this file's code was modeled directly on
that one.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all 56 tests still pass (no new tests added by this task).

- [ ] **Step 8: Commit**

```bash
git add src/panels/quick_commands_panel.rs src/panels/mod.rs src/workspace.rs
git commit -m "feat: add QuickCommandsPanel (bottom drawer) and status-bar toggle + cursor position"
```

---

### Task 5: Build verification and manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Release build**

Run: `cargo build --release 2>&1 | tail -30`
Expected: builds successfully.

- [ ] **Step 2: Manual smoke test**

If a display is available in this environment, run `cargo run --release` and verify by hand
(if no display is available, note that explicitly rather than claiming it was checked):

1. Open a local terminal tab. Confirm the status bar's right side shows a cursor position
   (`row:col`) that updates as you type/move the cursor.
2. Click the quick-commands icon in the status bar (bottom-left). Confirm a drawer opens
   above the status bar, showing "暂无快捷命令，点 + 添加".
3. Click "+ 添加", fill in a label and a command (e.g. `ls -la`), leave mode on 执行, save.
   Confirm the new row appears in the list.
4. Click the row (not the hover icons). Confirm the command is sent to the focused terminal
   AND executed (Enter pressed automatically).
5. Add a second command with mode 追加, click its row. Confirm the command text appears in
   the terminal's input line WITHOUT being executed (no Enter).
6. Hover a row, confirm edit/delete icons appear; click edit, change the label, save;
   confirm the row updates. Click delete on the other row; confirm it disappears.
7. Click the status-bar quick-commands icon again to close the drawer; confirm it hides and
   the icon's color reverts to unhighlighted.
8. Switch focus to a different terminal tab (or open a second one) and confirm the status
   bar's cursor position updates to that tab's cursor, and sending a quick command now goes
   to the newly-focused tab.
9. Quit and relaunch the app; reopen the drawer; confirm the two (edited/remaining) commands
   from steps 3-6 are still there (proves `quick_commands.toml` round-trips).

- [ ] **Step 3: Report results**

Summarize which of the 9 manual checks passed, and paste the full text of any that didn't,
before considering this task/plan complete.
