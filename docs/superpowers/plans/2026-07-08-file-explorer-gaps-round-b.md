# File Explorer Gaps Round B Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add hidden-files toggle, session-only directory history, and terminal cwd-sync (both directions) to caracal's SFTP file browser (`SftpPanel`).

**Architecture:** Hidden files becomes a client-side filter (`FileTableDelegate` keeps both the raw server list and the currently-displayed filtered list). Directory history and all navigation converge on one shared `navigate_to` helper on `SftpPanel`. cwd-sync has two independent directions: "send to terminal" reuses the existing `Workspace::send_to_focused_terminal`; "sync from terminal" is a new best-effort mechanism — inject `pwd`, wait a fixed delay, read back the terminal's grid via a new `TerminalView::line_text` (built on a programmatic `alacritty_terminal` `Lines`-selection, the same mechanism mouse triple-click already uses) — orchestrated by a new `Workspace::guess_focused_terminal_cwd`.

**Tech Stack:** Rust, GPUI (git `gpui`/`gpui_platform`), `gpui-component` (git), `alacritty_terminal` 0.26.0 (already a dependency).

## Global Constraints

- No new crate dependencies.
- No bookmarks — dropped from this round's scope entirely (see spec).
- No true bidirectional auto-sync (no OSC7) — "sync from terminal" is a single fixed-delay best-effort read, not a continuous signal; failures surface as a status message, never a wrong navigation, never a panic.
- Hidden-files preference and directory history are NOT persisted — session/panel-instance state only.
- No unit tests for GPUI rendering/terminal-grid-reading code — matches the existing zero-test convention across `panels/*.rs` and the existing untested state of `TerminalView::cursor_position`/`copy_selection_to_clipboard`.
- Chinese UI copy for all new user-facing strings: tooltip "显示隐藏文件" when hidden files are currently hidden, "隐藏点文件" when currently shown; history dropdown tooltip "最近访问", empty-history label "暂无历史记录"; "发送路径到终端" / "从终端同步目录" tooltips; failure status message exactly "无法从终端获取当前目录".
- Build with `cargo build` and run `cargo test` after every task; both must be clean before moving to the next task.

---

### Task 1: Hidden files toggle

**Files:**
- Modify: `src/panels/sftp.rs` (`FileTableDelegate` struct + `::new` ~line 97-117, `refresh` ~line 400-422, `SftpPanel` struct ~line 274-289, `SftpPanel::new` ~line 291-348, `render_toolbar` ~line 1106-1180)

**Interfaces:**
- Produces: `FileTableDelegate::apply_hidden_filter(&mut self, show_hidden: bool)`, `SftpPanel.show_hidden: bool` field, `SftpPanel::toggle_hidden_files(&mut self, cx: &mut Context<Self>)`. Not consumed by later tasks — independent, leaf feature.

- [ ] **Step 1: Split `FileTableDelegate.entries` into raw + filtered lists**

In `src/panels/sftp.rs`, change the struct and constructor (~line 97-117):

```rust
/// Delegate that feeds the `DataTable` from the panel's `entries` vec.
struct FileTableDelegate {
    /// Every entry the server returned for the current directory,
    /// unfiltered. `entries` (below) is derived from this by
    /// `apply_hidden_filter` — kept separately so toggling hidden files
    /// doesn't need a fresh SFTP round-trip.
    all_entries: Vec<SftpEntry>,
    entries: Vec<SftpEntry>,
    columns: Vec<Column>,
    panel: WeakEntity<SftpPanel>,
}

impl FileTableDelegate {
    fn new(panel: WeakEntity<SftpPanel>) -> Self {
        Self {
            all_entries: Vec::new(),
            entries: Vec::new(),
            columns: vec![
                Column::new("name", "名称").width(px(150.)).sortable(),
                Column::new("mtime", "修改时间").width(px(110.)),
                Column::new("size", "大小").width(px(64.)).sortable().text_right(),
                Column::new("perms", "权限").width(px(72.)),
            ],
            panel,
        }
    }

    /// Recompute the displayed `entries` from `all_entries`, filtering out
    /// dotfiles (names starting with `.`) unless `show_hidden` is true.
    fn apply_hidden_filter(&mut self, show_hidden: bool) {
        self.entries = if show_hidden {
            self.all_entries.clone()
        } else {
            self.all_entries
                .iter()
                .filter(|e| !e.name.starts_with('.'))
                .cloned()
                .collect()
        };
    }
}
```

- [ ] **Step 2: Update `refresh()` to populate `all_entries` and re-filter**

In `src/panels/sftp.rs`'s `refresh` (~line 400-422), change the `Ok(Ok(entries))` arm:

```rust
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let rx = self.session.sftp_read_dir(self.path.clone());
        let table_state = self.table_state.clone();
        cx.spawn(async move |this, cx| {
            let result = rx.recv_async().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(entries)) => {
                        this.status = format!("{} item(s)", entries.len());
                        let show_hidden = this.show_hidden;
                        table_state.update(cx, |state, cx| {
                            state.delegate_mut().all_entries = entries;
                            state.delegate_mut().apply_hidden_filter(show_hidden);
                            state.refresh(cx);
                        });
                    }
                    Ok(Err(e)) => this.status = format!("read_dir failed: {e}"),
                    Err(_) => this.status = "session closed".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
```

- [ ] **Step 3: Add `show_hidden` to `SftpPanel`**

In `src/panels/sftp.rs`'s `SftpPanel` struct (~line 274-289), add after `download_dir`:

```rust
    /// Default local download directory. Editable in the bottom bar.
    download_dir: PathBuf,
    /// Whether dotfiles (names starting with `.`) are shown. Toggled by the
    /// toolbar's hidden-files button; not persisted.
    show_hidden: bool,
```

In `SftpPanel::new`'s `Self { ... }` literal (~line 308-321), add after `download_dir,`:

```rust
            download_dir,
            show_hidden: false,
```

- [ ] **Step 4: Add `toggle_hidden_files`**

In `src/panels/sftp.rs`, add this method to `impl SftpPanel` (e.g. right after `refresh`, ~line 423):

```rust
    fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let show_hidden = self.show_hidden;
        self.table_state.update(cx, |state, cx| {
            state.delegate_mut().apply_hidden_filter(show_hidden);
            state.refresh(cx);
        });
        cx.notify();
    }
```

- [ ] **Step 5: Add the toolbar toggle button**

In `src/panels/sftp.rs`'s `render_toolbar` (~line 1106-1180), the method currently ends with the refresh button:

```rust
            .child(
                Button::new("sftp-refresh")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Refresh))
                    .tooltip("刷新")
                    .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
            )
    }
```

Add a flex spacer and the new button right after it (right-aligning the new button, matching nyaterm's toolbar placement):

```rust
            .child(
                Button::new("sftp-refresh")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Refresh))
                    .tooltip("刷新")
                    .on_click(cx.listener(|this, _, _w, cx| this.refresh(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("sftp-toggle-hidden")
                    .xsmall()
                    .ghost()
                    .icon(if self.show_hidden { IconName::EyeOff } else { IconName::Eye })
                    .tooltip(if self.show_hidden { "隐藏点文件" } else { "显示隐藏文件" })
                    .on_click(cx.listener(|this, _, _w, cx| this.toggle_hidden_files(cx))),
            )
    }
```

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: clean build, no warnings. `IconName::Eye`/`IconName::EyeOff` are bundled `gpui-component` icon assets (`eye.svg`/`eye-off.svg`), no new import beyond the already-present `IconName` (from `gpui_component::{ActiveTheme, Disableable, IconName, Sizable, WindowExt}`).

- [ ] **Step 7: Manual smoke test**

Run: `cargo run`, open an SFTP panel on a directory containing dotfiles, and verify:
- Dotfiles are hidden by default.
- Clicking the toggle button shows them; the icon and tooltip flip accordingly.
- Clicking again hides them, without a full-panel refresh flicker (no new network round-trip — instant).
- Navigating to a different directory and back preserves whichever hidden-files state was last toggled (it's `SftpPanel`-instance state, not reset per-directory).

- [ ] **Step 8: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: add hidden-files toggle to SFTP panel"
```

---

### Task 2: Directory history

**Files:**
- Modify: `src/panels/sftp.rs` (`SftpPanel` struct ~line 274-289, `SftpPanel::new` ~line 291-348, `commit_path`/`enter_dir`/`go_up` ~line 376-438, imports ~line 29-44, `render_path_bar` ~line 1182-1206)

**Interfaces:**
- Produces: `SftpPanel::navigate_to(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>)` — consumed by Task 4 (`sync_cwd_from_terminal`'s success path and the history dropdown's own click handler, added in this task).
- Consumes: nothing from other tasks.

- [ ] **Step 1: Add `history` to `SftpPanel`**

In `src/panels/sftp.rs`'s `SftpPanel` struct (~line 274-289), add after `show_hidden` (added by Task 1):

```rust
    show_hidden: bool,
    /// Most-recently-visited directories, oldest first, capped at 20. Shown
    /// by the path bar's history dropdown; not persisted.
    history: Vec<String>,
```

In `SftpPanel::new`'s `Self { ... }` literal, add after `show_hidden: false,`:

```rust
            show_hidden: false,
            history: Vec::new(),
```

- [ ] **Step 2: Add `navigate_to` and refactor the three existing navigation methods to use it**

In `src/panels/sftp.rs`, add this method to `impl SftpPanel` (e.g. right before `commit_path`, ~line 376):

```rust
    /// Shared navigation: set `self.path`, push it onto `history` (skipping
    /// a consecutive duplicate, capped at 20 entries), sync the path-bar
    /// input, and refresh. Every navigation call site (`enter_dir`, `go_up`,
    /// committing a typed path, the history dropdown, and cwd-sync) routes
    /// through this so history-pushing has exactly one implementation.
    fn navigate_to(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.last() != Some(&path) {
            self.history.push(path.clone());
            if self.history.len() > 20 {
                self.history.remove(0);
            }
        }
        self.path = path;
        self.status = "Loading…".to_string();
        self.sync_path_input(window, cx);
        self.refresh(cx);
        cx.notify();
    }
```

Replace `commit_path` (~line 376-397):

```rust
    fn commit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self
            .path_input
            .as_ref()
            .expect("path_input created on first render before any Enter");
        let raw = input.read(cx).value().to_string();
        let new = if raw.trim().is_empty() || raw.trim() == "~" {
            ".".to_string()
        } else {
            raw.trim().to_string()
        };
        self.navigate_to(new, window, cx);
    }
```

Replace `enter_dir` and `go_up` (~line 424-438):

```rust
    fn enter_dir(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let path = remote_join(&self.path, name);
        self.navigate_to(path, window, cx);
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = remote_parent(&self.path);
        self.navigate_to(path, window, cx);
    }
```

(`sync_path_input`, right after `go_up`, is unchanged — `navigate_to` calls it exactly as `commit_path`/`enter_dir`/`go_up` used to.)

- [ ] **Step 3: Add `DropdownButton` and `WeakEntity`-based imports**

`WeakEntity` is already imported in `src/panels/sftp.rs` (from Round A). Add `DropdownButton` to the existing `gpui_component::button` import (~line 35):

```rust
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
```

- [ ] **Step 4: Add the history dropdown to the path bar**

In `src/panels/sftp.rs`'s `render_path_bar` (~line 1182-1206), the method currently ends with the copy-path button:

```rust
            .child(
                Button::new("sftp-copy-path")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Copy)
                    .tooltip("复制路径")
                    .on_click(cx.listener(|this, _, _w, cx| this.copy_path(cx))),
            )
    }
```

Add the history dropdown right after it:

```rust
            .child(
                Button::new("sftp-copy-path")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Copy)
                    .tooltip("复制路径")
                    .on_click(cx.listener(|this, _, _w, cx| this.copy_path(cx))),
            )
            .child({
                let history = self.history.clone();
                let weak = cx.entity().downgrade();
                DropdownButton::new("sftp-history")
                    .xsmall()
                    .button(
                        Button::new("sftp-history-btn")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Undo2)
                            .tooltip("最近访问"),
                    )
                    .dropdown_menu(move |menu, _window, _cx| {
                        if history.is_empty() {
                            return menu.label("暂无历史记录");
                        }
                        let mut menu = menu;
                        for path in history.iter().rev().take(5) {
                            let path = path.clone();
                            let weak = weak.clone();
                            menu = menu.item(PopupMenuItem::new(path.clone()).on_click(
                                move |_ev, window, cx| {
                                    let path = path.clone();
                                    let _ = weak.update(cx, |this, cx| {
                                        this.navigate_to(path, window, cx);
                                    });
                                },
                            ));
                        }
                        menu
                    })
            })
    }
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: clean build, no warnings.

- [ ] **Step 6: Manual smoke test**

Run: `cargo run`, open an SFTP panel, navigate through 3-4 different directories, then:
- Open the history dropdown — confirm it lists the visited directories, most-recent first, capped at 5 shown.
- Click one — confirm it navigates there.
- Navigate to the exact same directory twice in a row (e.g. click refresh, or re-enter a directory you're already in) — confirm history doesn't grow a duplicate consecutive entry.

- [ ] **Step 7: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: add session-only directory history to SFTP panel"
```

---

### Task 3: `TerminalView::line_text`

**Files:**
- Modify: `src/terminal/view.rs` (imports ~line 9-11, new method near `cursor_position` ~line 331-342)

**Interfaces:**
- Produces: `TerminalView::line_text(&self, visual_row: usize) -> String` — consumed by Task 4.
- Consumes: nothing from other tasks (independent of Tasks 1-2, foundation for Task 4).

- [ ] **Step 1: Add `line_text`**

In `src/terminal/view.rs`, add this method to `impl TerminalView` right after `cursor_position` (~line 342):

```rust
    /// Best-effort plain-text read of one grid row, identified in the same
    /// "visual row from viewport top" coordinate space `cursor_position`
    /// returns (i.e. `visual_row` is what `cursor_position().0` gives you,
    /// not a raw `alacritty_terminal::index::Line`). Built on a temporary
    /// programmatic `Lines`-type selection — the same mechanism mouse
    /// triple-click already uses (`terminal/selection.rs`) — rather than
    /// direct grid-cell iteration. Saves and restores whatever selection
    /// was already active first, so this never clobbers a real
    /// in-progress user selection. Used by
    /// `Workspace::guess_focused_terminal_cwd` to screen-scrape `pwd`'s
    /// output for the SFTP panel's "sync from terminal" button — see
    /// `docs/superpowers/specs/2026-07-08-file-explorer-gaps-round-b-design.md`
    /// for why this is best-effort, not a reliable signal.
    pub fn line_text(&self, visual_row: usize) -> String {
        use alacritty_terminal::index::{Column, Line, Point, Side};
        use alacritty_terminal::selection::Selection;

        let mut term = self.term.lock();
        let display_offset = term.renderable_content().display_offset as i32;
        let raw_line = visual_row as i32 - display_offset;
        let saved = term.selection.take();
        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Point::new(Line(raw_line), Column(0)),
            Side::Left,
        ));
        let text = term.selection_to_string().unwrap_or_default();
        term.selection = saved;
        text.trim().to_string()
    }
```

`SelectionType` is already imported at the top of this file (`use alacritty_terminal::selection::SelectionType;`, ~line 10) — no change needed there.

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: clean build. `cargo build` will report `line_text` as an unused method (dead-code warning) at this point — that's expected and temporary; Task 4 wires it up and the warning disappears. This is the same pattern the previous round used for `rename_entry`/`show_properties` (marked `#[allow(dead_code)]` there); here, add the same attribute so this task's own build is warning-clean:

```rust
    #[allow(dead_code)] // wired to Workspace::guess_focused_terminal_cwd in Task 4
    pub fn line_text(&self, visual_row: usize) -> String {
```

Re-run `cargo build` — expect clean, 0 warnings.

- [ ] **Step 3: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add TerminalView::line_text for best-effort cwd screen-scraping"
```

---

### Task 4: cwd sync (send to terminal + sync from terminal)

**Files:**
- Modify: `src/workspace.rs` (imports ~line 28-33, `Workspace::send_to_focused_terminal` region ~line 412-417, `show_sftp` ~line 421-434)
- Modify: `src/panels/sftp.rs` (imports ~line 29-44, `SftpPanel` struct ~line 274-289, `SftpPanel::new` ~line 291-348, new methods near `copy_path`, `render_path_bar` ~line 1182+)

**Interfaces:**
- Consumes: `SftpPanel::navigate_to` (Task 2), `TerminalView::line_text` (Task 3, with its `#[allow(dead_code)]` removed here since this task is what wires it up).
- Produces: `Workspace::guess_focused_terminal_cwd(&self, cx: &mut Context<Self>) -> Task<Option<String>>`, `SftpPanel::send_path_to_terminal(&self, cx: &Context<Self>)`, `SftpPanel::sync_cwd_from_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>)`. Not consumed elsewhere — final integration task.

- [ ] **Step 1: Add `Task` to `workspace.rs`'s gpui imports**

In `src/workspace.rs` (~line 28-33), add `Task`:

```rust
use gpui::{
    AnyView, App, AppContext, Bounds, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div,
    prelude::FluentBuilder, px, size,
};
```

- [ ] **Step 2: Add `Workspace::guess_focused_terminal_cwd`**

In `src/workspace.rs`, add this method right after `send_to_focused_terminal` (~line 412-417):

```rust
    pub fn send_to_focused_terminal(&self, text: &str, execute: bool, cx: &App) {
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        terminal.read(cx).send_text(text, execute);
    }

    /// Best-effort: ask the currently-focused terminal what directory it's
    /// in by injecting `pwd` and reading back the line the shell echoes.
    /// No OSC7/shell-integration exists in this terminal emulator, so this
    /// is a fixed-delay guess, not a reliable signal — see
    /// `docs/superpowers/specs/2026-07-08-file-explorer-gaps-round-b-design.md`.
    /// Resolves to `None` if there's no focused terminal, or if the line
    /// read back doesn't look like an absolute path.
    pub fn guess_focused_terminal_cwd(&self, cx: &mut Context<Self>) -> Task<Option<String>> {
        let Some(terminal) = self.focused_terminal.as_ref().and_then(|w| w.upgrade()) else {
            return Task::ready(None);
        };
        let start_row = terminal.read(cx).cursor_position().0;
        terminal.read(cx).send_text("pwd", true);
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let line = terminal.read_with(cx, |term, _cx| term.line_text(start_row + 1));
            let trimmed = line.trim().to_string();
            if trimmed.starts_with('/') {
                Some(trimmed)
            } else {
                None
            }
        })
    }
```

- [ ] **Step 3: Wire `WeakEntity<Workspace>` into `SftpPanel::new`**

In `src/workspace.rs`'s `show_sftp` (~line 421-434):

```rust
    fn show_sftp(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        if !self.sftp_panels.contains_key(&key) {
            let Some(session) = self.ssh_session(&config) else {
                return;
            };
            let label = format!("{}@{}", config.user, config.host);
            let workspace = cx.entity().downgrade();
            let panel: AnyView =
                cx.new(|cx| SftpPanel::new(session, label, workspace, window, cx)).into();
            self.sftp_panels.insert(key.clone(), panel);
        }
        self.active_sftp = Some(key);
        self.left_active = Some(PanelId::Sftp);
        cx.notify();
    }
```

- [ ] **Step 4: Add `workspace` to `SftpPanel` and thread it through the constructor**

In `src/panels/sftp.rs`'s imports (~line 29-44), add:

```rust
use crate::workspace::Workspace;
```

In `SftpPanel` struct (~line 274-289), add after `history` (added by Task 2):

```rust
    history: Vec<String>,
    /// Back-reference for cwd-sync — see `send_path_to_terminal`/
    /// `sync_cwd_from_terminal`.
    workspace: WeakEntity<Workspace>,
```

Change `SftpPanel::new`'s signature (~line 292-297) to accept it, inserting right after `label`:

```rust
    pub fn new(
        session: Arc<SshSession>,
        label: impl Into<SharedString>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

In the `Self { ... }` literal, add after `history: Vec::new(),`:

```rust
            history: Vec::new(),
            workspace,
```

- [ ] **Step 5: Add `send_path_to_terminal` and `sync_cwd_from_terminal`**

In `src/panels/sftp.rs`, add these methods to `impl SftpPanel` (e.g. right after `copy_path`, near where the previous round added `copy_entry_path`):

```rust
    fn send_path_to_terminal(&self, cx: &Context<Self>) {
        let cmd = format!("cd '{}'", self.path);
        let _ = self.workspace.read_with(cx, |ws, cx| {
            ws.send_to_focused_terminal(&cmd, true, cx);
        });
    }

    fn sync_cwd_from_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(task) = workspace.update(cx, |ws, cx| ws.guess_focused_terminal_cwd(cx))
            else {
                return;
            };
            let guess = task.await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| match guess {
                    Some(path) => this.navigate_to(path, window, cx),
                    None => {
                        this.status = "无法从终端获取当前目录".to_string();
                        cx.notify();
                    }
                });
            });
        })
        .detach();
    }
```

- [ ] **Step 6: Remove `TerminalView::line_text`'s `#[allow(dead_code)]`**

In `src/terminal/view.rs`, remove the `#[allow(dead_code)] // wired to Workspace::guess_focused_terminal_cwd in Task 4` line directly above `pub fn line_text(&self, visual_row: usize) -> String {` — it's genuinely called now, from `guess_focused_terminal_cwd` (Step 2 above).

- [ ] **Step 7: Add the two path-bar buttons**

In `src/panels/sftp.rs`'s `render_path_bar`, add these two buttons right after the history dropdown added in Task 2 (before the method's closing `}`):

```rust
            .child(
                Button::new("sftp-send-to-terminal")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ArrowRight)
                    .tooltip("发送路径到终端")
                    .on_click(cx.listener(|this, _, _w, cx| this.send_path_to_terminal(cx))),
            )
            .child(
                Button::new("sftp-sync-from-terminal")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ArrowLeft)
                    .tooltip("从终端同步目录")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.sync_cwd_from_terminal(window, cx)
                    })),
            )
    }
```

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: clean build, no warnings (including no leftover dead-code warning for `line_text`, confirming Step 6's wiring is genuinely reachable).

- [ ] **Step 9: Manual smoke test**

Run: `cargo run`, open a shell terminal tab and an SFTP panel on the same host, then:
- In the SFTP panel, navigate to some directory, click "发送路径到终端" — confirm a `cd '...'` command appears (and executes, since it's sent with Enter) in the terminal tab that last had focus.
- Click into the terminal, manually run `cd` to a different directory, wait for the shell prompt to settle, click back to the SFTP panel and click "从终端同步目录" — confirm the browser navigates to that directory. This is best-effort: if it doesn't work first try, check the terminal actually has a normal single-line prompt and try again; document any reproducible failure mode.
- Click "从终端同步目录" with no terminal tab ever having been focused (e.g. a fresh workspace with only the SFTP panel open) — confirm a graceful status message, not a panic.
- Click "从终端同步目录" while a long-running command (e.g. `sleep 5`) is executing in the focused terminal — confirm no crash; either a status message or (if the heuristic happens to misfire) no worse than a wrong-but-plausible-looking navigation, never a crash.

- [ ] **Step 10: Commit**

```bash
git add src/workspace.rs src/panels/sftp.rs src/terminal/view.rs
git commit -m "feat: add terminal cwd sync (send-to and best-effort sync-from) to SFTP panel"
```

---

### Task 5: Final verification

**Files:** None (verification only).

**Interfaces:** None.

- [ ] **Step 1: Full build and test suite**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --all-targets`
Expected: no new warnings in `src/panels/sftp.rs`, `src/terminal/view.rs`, or `src/workspace.rs` (the only files this plan touches) compared to `main`. Pre-existing warnings elsewhere are out of scope.

- [ ] **Step 3: End-to-end manual smoke test**

Run: `cargo run` and walk through Task 1 Step 7, Task 2 Step 6, and Task 4 Step 9's checklists in one pass, plus: confirm the existing toolbar/path-bar actions (new file/folder/upload/download/delete/up/refresh, copy-path, and — from the previous round — the right-click context menu's 打开/下载/重命名/属性/复制路径/删除) still all work unchanged, since this plan's `SftpPanel::new` signature change and `enter_dir`/`go_up`/`commit_path` refactor touch code every other feature in this panel depends on.

- [ ] **Step 4: No commit needed for this task** — it's verification only. If Step 2/3 surface a bug, fix it in the relevant task's file, re-run that task's own build/test steps, then re-run this task's steps.
