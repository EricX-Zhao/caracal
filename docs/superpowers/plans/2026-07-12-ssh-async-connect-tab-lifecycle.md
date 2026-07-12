# SSH async connect + in-terminal errors + tab-close session teardown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Double-clicking a saved SSH connection opens a terminal tab instantly (never blocks the UI thread), shows connect failures inline in that terminal instead of doing nothing, and tears down a host's shared session (+ its SFTP/monitor panels) when its last terminal tab closes.

**Architecture:** Reuse the existing manual-reconnect machinery (`reconnect_ssh_terminal` / `TerminalView::reconnect_with`) for the *initial* connect too: open the tab with a placeholder "connecting" backend immediately, dial `SshSession::connect` on a background thread, and swap in the real shell (or a "failed" banner) once it resolves. Tab-close cleanup is event-driven: `TerminalPanel` gains a generic `on_removed` → `Closed` event hook that `Workspace` subscribes to per SSH tab, evicting the shared session once no tab for that host remains.

**Tech Stack:** Rust, GPUI (`Entity`/`Context`/`cx.spawn_in`/`cx.background_spawn`), the existing `flume` channel + feeder-thread PTY bridge.

Full design: `docs/superpowers/specs/2026-07-12-ssh-async-connect-tab-lifecycle-design.md`.

**On line numbers below:** every `(currently lines N-M)` annotation was read from the real, unmodified files at plan-writing time. Each task's edits shift the line numbers of everything after them in the same file, so by the time you reach a later task those numbers will be off by however many lines earlier tasks added or removed. Treat them as a rough locator, not ground truth — find the edit point by matching the shown "before" code (method name, field name, or exact text), not by jumping to the cited line.

## Global Constraints

- No new dependencies.
- Banner/error text stays Chinese (only the *reason* substring, from `anyhow::Error`'s `Display`, may be in English, same as existing `log::error!` calls already are). Note: the old fixed disconnected-banner string "连接已断开，按 Enter 重新连接" (`src/terminal/view.rs`) is *not* preserved verbatim — Task 2 unifies it under `conn_banner_text`'s shared template, so it becomes host-prefixed with "重连" instead of "重新连接" (see Task 2's notes and the design doc).
- `TerminalView` stays backend-agnostic (CLAUDE.md §2) — no `gpui_component` imports, no `SshSession`-specific logic beyond the `Arc<dyn PtyBackend>` it already takes.
- `TerminalPanel` stays a thin adapter (file header, `src/panels/terminal.rs:1-4`) — it may emit a generic "I was removed" event but must not know *why* a tab closing matters.
- Build must stay green (`cargo build`) after every task's commit — this repo has no other CI-blocking gate to check locally, but don't leave a task mid-compile.

---

### Task 1: `ConnBanner` enum + pure banner-text function

**Files:**
- Modify: `src/terminal/view.rs` (insert after line 133, right before `pub struct TerminalView` at line 135)

**Interfaces:**
- Consumes: nothing new.
- Produces: `enum ConnBanner { Connecting, Failed(String) }` (private to `terminal::view`); `fn conn_banner_text(host_label: &str, banner: &ConnBanner) -> String` (private). Task 2 wires both into `TerminalView`.

This task only adds new, self-contained code — nothing calls it yet, so `cargo build` will show `dead_code` warnings on `ConnBanner` and `conn_banner_text` until Task 2 lands. That's expected for this one task; don't try to suppress it with `#[allow(dead_code)]` (Task 2 removes the warning for real).

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block at the bottom of `src/terminal/view.rs` (after the `to_font_carries_fallback_chain` test, before the closing `}` of the module):

```rust
    #[test]
    fn conn_banner_text_connecting() {
        assert_eq!(
            conn_banner_text("root@example.com", &ConnBanner::Connecting),
            "正在连接 root@example.com…"
        );
    }

    #[test]
    fn conn_banner_text_failed_includes_reason() {
        assert_eq!(
            conn_banner_text(
                "root@example.com",
                &ConnBanner::Failed("连接失败: Connection refused".to_string())
            ),
            "root@example.com 连接失败: Connection refused，按 Enter 重连"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin caracal conn_banner_text`
Expected: FAIL to compile — `ConnBanner` and `conn_banner_text` don't exist yet.

- [ ] **Step 3: Add the enum and function**

Insert into `src/terminal/view.rs`, immediately after line 133 (`"monospace".into()\n}`) and before line 135 (`pub struct TerminalView {`):

```rust

/// Non-live state shown as a full-terminal banner overlay
/// (`conn_banner_text`, `TerminalView::render`). Only ever set on a
/// `remote_reconnect` (SSH) backend.
#[derive(Clone, Debug, PartialEq)]
enum ConnBanner {
    /// Dialing out (initial connect or a manual reconnect). Enter does
    /// nothing yet — there's nothing to retry.
    Connecting,
    /// Dead, with a human-readable reason. Enter re-dials (emits
    /// `TerminalViewEvent::ReconnectRequested`).
    Failed(String),
}

/// Pure text for the connecting/failed banner overlay
/// (`TerminalView::render`). Extracted standalone so it's unit-testable
/// without a `Window`/`Context`.
fn conn_banner_text(host_label: &str, banner: &ConnBanner) -> String {
    match banner {
        ConnBanner::Connecting => format!("正在连接 {host_label}…"),
        ConnBanner::Failed(reason) => format!("{host_label} {reason}，按 Enter 重连"),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --bin caracal conn_banner_text`
Expected: PASS (2 tests). `cargo build` succeeds; it will print `warning: enum \`ConnBanner\` is never constructed` and `warning: function \`conn_banner_text\` is never used` — expected until Task 2, not a regression to fix here.

- [ ] **Step 5: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add ConnBanner enum + banner text formatting for SSH terminal tabs"
```

---

### Task 2: Wire `ConnBanner` into `TerminalView`, replacing `disconnected: bool`

**Files:**
- Modify: `src/terminal/view.rs`
- Modify: `src/workspace.rs` (one call-site fix, see Step 5)

**Interfaces:**
- Consumes: `ConnBanner`, `conn_banner_text` (Task 1).
- Produces: `TerminalView` fields `banner: Option<ConnBanner>`, `host_label: String` (replacing `disconnected: bool`); `TerminalView::with_backend(window, cx, remote_reconnect: bool, host_label: String, make_backend) -> Self` (signature gains `host_label`); `TerminalView::new_ssh_shell(window, cx, session: Arc<SshSession>, host_label: String) -> Self` (signature gains `host_label`). Task 3 builds on these fields; Task 4 passes real `host_label` values from `Workspace`.

This is a mechanical swap with **no observable behavior change** — the disconnected banner still shows the same text, Enter still reconnects the same way. Verified by the full existing test suite passing, not new tests.

- [ ] **Step 1: Replace the `disconnected` field with `banner` + add `host_label`**

In `src/terminal/view.rs`, find the `remote_reconnect` / `disconnected` fields (around line 171-186):

```rust
    remote_reconnect: bool,
    /// Set by the disconnect-watch task (see `spawn_generation`) when the
    /// current backend generation's read side ends. Only ever becomes
    /// user-visible when `remote_reconnect` is true (see `mark_disconnected`).
    disconnected: bool,
    _drain_task: Task<()>,
```

Replace with:

```rust
    remote_reconnect: bool,
    /// "user@host" label used by the connecting/failed banner text
    /// (`conn_banner_text`). Empty for non-SSH backends, which never show
    /// a banner (`remote_reconnect` gates that).
    host_label: String,
    /// `Some` while a non-live state (connecting or dead) should be shown
    /// as a full-terminal overlay; `None` means the backend is live. Only
    /// ever `Some` when `remote_reconnect` is true. Set by
    /// `mark_disconnected` / `mark_connect_failed` / `mark_connecting`
    /// (the latter two added in Task 3), cleared by `reconnect_with`.
    banner: Option<ConnBanner>,
    _drain_task: Task<()>,
```

- [ ] **Step 2: Thread `host_label` through `with_backend` and its callers**

Replace `with_backend`'s signature and body (`src/terminal/view.rs`, currently lines 308-369):

```rust
    fn with_backend(
        window: &mut Window,
        cx: &mut Context<Self>,
        remote_reconnect: bool,
        host_label: String,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, events_tx.clone());

        let (backend, disconnect_watch) = Self::spawn_generation(
            term.clone(),
            events_tx.clone(),
            DEFAULT_COLS as u16,
            DEFAULT_ROWS as u16,
            make_backend,
            cx,
        );

        let drain_task = cx.spawn(async move |weak, cx| {
            run_drain(weak, events_rx, cx).await;
        });

        Self {
            term,
            backend,
            events_tx,
            focus_handle,
            font_config: FontConfig::default(),
            title: "terminal".to_string(),
            exited: false,
            last_cell_w: 0.0,
            last_cell_h: 0.0,
            last_cols: 0,
            last_rows: 0,
            last_origin_x: 0.0,
            last_origin_y: 0.0,
            selection_dragging: None,
            remote_reconnect,
            host_label,
            banner: None,
            _drain_task: drain_task,
            _disconnect_watch: disconnect_watch,
        }
    }
```

Update its five call sites to pass a `host_label`. `new`, `new_local_with`, `new_telnet`, `new_serial` (none of these are SSH, so the banner never shows for them — pass an empty label):

```rust
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_backend(window, cx, false, String::new(), |cols, rows, bytes_tx| {
            Arc::new(LocalPty::spawn(cols, rows, bytes_tx).expect("failed to spawn local pty"))
        })
    }

    pub fn new_local_with(
        window: &mut Window,
        cx: &mut Context<Self>,
        shell: &str,
        working_dir: Option<&str>,
    ) -> Self {
        let shell = shell.to_string();
        Self::with_backend(window, cx, false, String::new(), move |cols, rows, bytes_tx| {
            Arc::new(
                LocalPty::spawn_with(cols, rows, bytes_tx, &shell, working_dir)
                    .expect("failed to spawn local pty"),
            )
        })
    }
```

```rust
    pub fn new_telnet(window: &mut Window, cx: &mut Context<Self>, config: TelnetConfig) -> Self {
        Self::with_backend(window, cx, false, String::new(), move |_cols, _rows, bytes_tx| {
            match TelnetBackend::connect(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mtelnet connect failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            }
        })
    }

    pub fn new_serial(window: &mut Window, cx: &mut Context<Self>, config: SerialConfig) -> Self {
        Self::with_backend(window, cx, false, String::new(), move |_cols, _rows, bytes_tx| {
            match SerialBackend::open(config, bytes_tx.clone()) {
                Ok(backend) => Arc::new(backend),
                Err(e) => {
                    let _ = bytes_tx
                        .send(format!("\r\n\x1b[1;31mserial open failed:\x1b[0m {e}\r\n").into_bytes());
                    Arc::new(DeadBackend)
                }
            }
        })
    }
```

And `new_ssh_shell` gains a real `host_label` param (its doc comment already explains the sharing rationale — keep it, just add the param):

```rust
    pub fn new_ssh_shell(
        window: &mut Window,
        cx: &mut Context<Self>,
        session: Arc<SshSession>,
        host_label: String,
    ) -> Self {
        Self::with_backend(window, cx, true, host_label, move |cols, rows, bytes_tx| {
            session.open_shell(cols, rows, bytes_tx)
        })
    }
```

- [ ] **Step 3: Update `mark_disconnected`**

Replace (currently lines 401-407):

```rust
    fn mark_disconnected(&mut self, cx: &mut Context<Self>) {
        if !self.remote_reconnect || matches!(self.banner, Some(ConnBanner::Failed(_))) {
            return;
        }
        self.banner = Some(ConnBanner::Failed("连接已断开".to_string()));
        cx.notify();
    }
```

- [ ] **Step 4: Update `on_key_down` and `render`**

In `on_key_down` (currently lines 558-568), replace the disconnected-gate block:

```rust
    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Non-live state (connecting or dead SSH connection): swallow all
        // input except Enter, and only forward that as
        // `ReconnectRequested` once there's actually something to retry
        // (`Failed`, not `Connecting`) — `Workspace` is the only thing
        // that knows how to redial (CLAUDE.md §2).
        if let Some(banner) = &self.banner {
            if matches!(banner, ConnBanner::Failed(_)) && ev.keystroke.key == "enter" {
                cx.emit(TerminalViewEvent::ReconnectRequested);
            }
            return;
        }
```

(the rest of the function, starting at the "Copy shortcut" comment, is unchanged).

In `render` (currently lines 828-875), replace the whole `impl Render for TerminalView` block:

```rust
impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let font = self.font_config.to_font();
        let metrics = cell_metrics(window, &font, self.font_config.size);
        let banner_text = self.banner.as_ref().map(|b| conn_banner_text(&self.host_label, b));
        div()
            .track_focus(&self.focus_handle)
            .key_context(TERMINAL_KEY_CONTEXT)
            .relative()
            .size_full()
            .px(metrics.width * EDGE_PADDING_COLS)
            .py(metrics.height * EDGE_PADDING_ROWS)
            .on_key_down(cx.listener(Self::on_key_down))
            // Actions reclaiming Root-context keys (tab / shift-tab / ctrl-c).
            .on_action(cx.listener(Self::on_interrupt))
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_back_tab))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(terminal_canvas(
                view,
                self.term.clone(),
                self.backend.clone(),
                font,
                self.font_config.size,
                self.focus_handle.clone(),
            ))
            // Connecting/disconnected-SSH banner: overlays the last-painted
            // frame (left in place, not cleared) with a dimmed backdrop +
            // centered message, matching nyaterm's "connection lost"
            // treatment. Text comes from `conn_banner_text`.
            .when_some(banner_text, |this, text| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(hsla(0.0, 0.0, 0.0, 0.72))
                        .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                        .child(text),
                )
            })
    }
}
```

- [ ] **Step 5: Fix the one call site in `workspace.rs`**

`src/workspace.rs`'s `open_ssh` (around line 286) currently calls `TerminalView::new_ssh_shell(window, cx, session)`. This task doesn't redesign `open_ssh` yet (Task 4 does) — just fix the now-broken call to keep the build green:

```rust
            let terminal = cx.new(|cx| {
                TerminalView::new_ssh_shell(
                    window,
                    cx,
                    session,
                    format!("{}@{}", config.user, config.host),
                )
            });
```

- [ ] **Step 6: Build and run the full test suite**

Run: `cargo build`
Expected: succeeds, no warnings about `ConnBanner`/`conn_banner_text` (both now used).

Run: `cargo test --bin caracal`
Expected: PASS — all existing tests plus Task 1's two new ones.

- [ ] **Step 7: Commit**

```bash
git add src/terminal/view.rs src/workspace.rs
git commit -m "refactor: replace TerminalView's disconnected bool with ConnBanner state"
```

---

### Task 3: `new_ssh_connecting` placeholder constructor + `mark_connect_failed` / `mark_connecting`

**Files:**
- Modify: `src/terminal/view.rs`

**Interfaces:**
- Consumes: `banner`/`host_label` fields, `ConnBanner` (Tasks 1-2).
- Produces: `TerminalView::new_ssh_connecting(window: &mut Window, cx: &mut Context<Self>, host_label: String) -> Self` (pub); `TerminalView::mark_connect_failed(&mut self, reason: String, cx: &mut Context<Self>)` (pub); `TerminalView::mark_connecting(&mut self, cx: &mut Context<Self>)` (pub); private `TerminalView::base_setup(window, cx) -> (FocusHandle, SharedTerm, flume::Sender<Event>, Task<()>)` and `TerminalView::assemble(focus_handle, term, events_tx, drain_task, backend, disconnect_watch, remote_reconnect, host_label, banner) -> Self` helpers extracted from `with_backend` (the latter avoids duplicating the ~14-field struct literal between `with_backend` and `new_ssh_connecting`). Task 4 (`Workspace::open_ssh`, `reconnect_ssh_terminal`) is the consumer of the three `pub fn`s.

None of these are called anywhere yet after this task — `cargo build` will warn `associated items \`new_ssh_connecting\`, \`mark_connect_failed\`, \`mark_connecting\` are never used`. Expected here (this is a bin crate, so `pub` alone doesn't suppress the lint — see project memory on foundation tasks); Task 4 removes the warning for real.

- [ ] **Step 1: Extract `base_setup` and `assemble` from `with_backend`**

Replace `with_backend` (from Task 2's Step 2) with a version that delegates to two new shared helpers, placed immediately above it — `base_setup` (the focus/term/drain-task prelude) and `assemble` (the final `Self { .. }` construction, so it isn't duplicated when `new_ssh_connecting` needs the same fields with different backend/banner values in Step 2):

```rust
    /// Shared prelude for both a full backend generation (`with_backend`)
    /// and a placeholder-only view (`new_ssh_connecting`): the focus
    /// handle, `Term`, event channel, and long-lived drain task. Doesn't
    /// touch the backend itself — callers wire that up separately (a real
    /// generation via `spawn_generation`, or nothing at all for a
    /// placeholder).
    fn base_setup(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, SharedTerm, flume::Sender<Event>, Task<()>) {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, events_tx.clone());

        let drain_task = cx.spawn(async move |weak, cx| {
            run_drain(weak, events_rx, cx).await;
        });

        (focus_handle, term, events_tx, drain_task)
    }

    /// Shared tail for both a full backend generation (`with_backend`)
    /// and a placeholder-only view (`new_ssh_connecting`): fills in the
    /// remaining fields that never vary by caller (default font, initial
    /// title, zeroed cell metrics, etc.) around the handful that do.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        focus_handle: FocusHandle,
        term: SharedTerm,
        events_tx: flume::Sender<Event>,
        drain_task: Task<()>,
        backend: Arc<dyn PtyBackend>,
        disconnect_watch: Task<()>,
        remote_reconnect: bool,
        host_label: String,
        banner: Option<ConnBanner>,
    ) -> Self {
        Self {
            term,
            backend,
            events_tx,
            focus_handle,
            font_config: FontConfig::default(),
            title: "terminal".to_string(),
            exited: false,
            last_cell_w: 0.0,
            last_cell_h: 0.0,
            last_cols: 0,
            last_rows: 0,
            last_origin_x: 0.0,
            last_origin_y: 0.0,
            selection_dragging: None,
            remote_reconnect,
            host_label,
            banner,
            _drain_task: drain_task,
            _disconnect_watch: disconnect_watch,
        }
    }

    fn with_backend(
        window: &mut Window,
        cx: &mut Context<Self>,
        remote_reconnect: bool,
        host_label: String,
        make_backend: impl FnOnce(u16, u16, flume::Sender<Vec<u8>>) -> Arc<dyn PtyBackend>,
    ) -> Self {
        let (focus_handle, term, events_tx, drain_task) = Self::base_setup(window, cx);

        let (backend, disconnect_watch) = Self::spawn_generation(
            term.clone(),
            events_tx.clone(),
            DEFAULT_COLS as u16,
            DEFAULT_ROWS as u16,
            make_backend,
            cx,
        );

        Self::assemble(
            focus_handle,
            term,
            events_tx,
            drain_task,
            backend,
            disconnect_watch,
            remote_reconnect,
            host_label,
            None,
        )
    }
```

- [ ] **Step 2: Add `new_ssh_connecting`**

Add right after `new_ssh_shell`:

```rust
    /// A placeholder SSH tab shown the instant the user double-clicks a
    /// saved connection, before the dial resolves. No feeder/
    /// disconnect-watch is spawned — a `DeadBackend` generation's
    /// `bytes_tx` would be dropped immediately (same pattern telnet/
    /// serial's connect-failure fallback uses in `new_telnet`/
    /// `new_serial`), which would fire `mark_disconnected` within the
    /// same tick and stomp the `Connecting` banner set here. `Workspace`
    /// swaps in the real generation via `reconnect_with` once
    /// `SshSession::connect` resolves (success) or calls
    /// `mark_connect_failed` (failure).
    pub fn new_ssh_connecting(window: &mut Window, cx: &mut Context<Self>, host_label: String) -> Self {
        let (focus_handle, term, events_tx, drain_task) = Self::base_setup(window, cx);
        Self::assemble(
            focus_handle,
            term,
            events_tx,
            drain_task,
            Arc::new(DeadBackend),
            Task::ready(()),
            true,
            host_label,
            Some(ConnBanner::Connecting),
        )
    }
```

- [ ] **Step 3: Add `mark_connect_failed` and `mark_connecting`**

Add right after `mark_disconnected`:

```rust
    /// Called by `Workspace` when an SSH dial (initial connect or a
    /// manual reconnect) fails. Flips the banner to `Failed(reason)` —
    /// Enter now re-emits `TerminalViewEvent::ReconnectRequested` (see
    /// `on_key_down`).
    pub fn mark_connect_failed(&mut self, reason: String, cx: &mut Context<Self>) {
        if !self.remote_reconnect {
            return;
        }
        self.banner = Some(ConnBanner::Failed(reason));
        cx.notify();
    }

    /// Called by `Workspace` right before redialing a dead SSH tab, so
    /// the banner reads "connecting" instead of stale "failed" text
    /// during the redial.
    pub fn mark_connecting(&mut self, cx: &mut Context<Self>) {
        if !self.remote_reconnect {
            return;
        }
        self.banner = Some(ConnBanner::Connecting);
        cx.notify();
    }
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: succeeds. `warning: associated items \`new_ssh_connecting\`, \`mark_connect_failed\`, \`mark_connecting\` are never used` is expected here — Task 4 wires them in.

Run: `cargo test --bin caracal`
Expected: PASS (unchanged from Task 2 — this task adds no new pure-testable logic; `new_ssh_connecting`/`mark_connect_failed`/`mark_connecting` all need a live `Window`/`Context`, which this codebase has no test harness for — see the design doc's Testing section).

- [ ] **Step 5: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: add new_ssh_connecting placeholder + mark_connect_failed/mark_connecting"
```

---

### Task 4: Async connect flow in `Workspace::open_ssh` + `reconnect_ssh_terminal`

**Files:**
- Modify: `src/workspace.rs`

**Interfaces:**
- Consumes: `TerminalView::new_ssh_connecting`, `mark_connect_failed`, `mark_connecting` (Task 3); `TerminalView::new_ssh_shell(.., host_label)` (Task 2).
- Produces: `Workspace::open_ssh` (public signature unchanged: `pub fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>)`) now opens the tab instantly and connects asynchronously. `reconnect_ssh_terminal` now shows the real failure reason and a `Connecting` banner while redialing. Task 6 adds one more piece *inside* `open_ssh` (a `Closed`-event subscription) between the panel's creation and `add_center` call — this task's version works fully without it, just without tab-close cleanup yet.

This task delivers the full user-visible fix for requirements 1 and 2 (instant tab, in-terminal error). No new unit tests — this is inherently a `Window`/async-runtime-dependent flow with no test harness in this codebase (see design doc's Testing section); verified by manual smoke test in Step 4.

- [ ] **Step 1: Rewrite `open_ssh`**

Replace the whole method (currently `src/workspace.rs:284-313`):

```rust
    pub fn open_ssh(&mut self, config: SshConfig, window: &mut Window, cx: &mut Context<Self>) {
        let key = config.key();
        let host_label = format!("{}@{}", config.user, config.host);

        let terminal = if let Some(session) = self.ssh_sessions.get(&key).cloned() {
            cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session, host_label.clone()))
        } else {
            cx.new(|cx| TerminalView::new_ssh_connecting(window, cx, host_label.clone()))
        };
        Self::seed_font_from_settings(&terminal, cx);
        let follow = config.clone();
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        // `term_weak` is needed again below (the background-connect
        // closure and, from Task 6, the close-cleanup subscription), so
        // the on-focus closure gets its own clone rather than moving the
        // outer binding.
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
        self._subscriptions.push(sub);
        // Remember which host this tab is for, so a
        // `ReconnectRequested` (Enter pressed on the disconnected
        // banner — see `terminal/view.rs`) knows what to redial.
        self.ssh_reconnect_configs.insert(terminal.entity_id(), config.clone());
        let reconnect_sub =
            cx.subscribe_in(&terminal, window, |this, terminal, event, window, cx| {
                let TerminalViewEvent::ReconnectRequested = event;
                this.reconnect_ssh_terminal(terminal.clone(), window, cx);
            });
        self._subscriptions.push(reconnect_sub);

        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);

        if self.ssh_sessions.contains_key(&key) {
            self.show_sftp(config.clone(), window, cx);
            self.show_monitor(config, window, cx);
            return;
        }

        // Not cached: dial in the background so the tab above opens
        // instantly and a slow/unreachable host can't freeze the UI (same
        // rationale as `reconnect_ssh_terminal`, below).
        let dial_config = config.clone();
        let connect_task = cx.background_spawn(async move { SshSession::connect(dial_config) });
        let term_for_connect = term_weak.clone();
        cx.spawn_in(window, async move |this, cx| {
            match connect_task.await {
                Ok(session) => {
                    let _ = this.update_in(cx, move |this, window, cx| {
                        this.ssh_sessions.insert(key, session.clone());
                        if let Some(t) = term_for_connect.upgrade() {
                            t.update(cx, |view, cx| {
                                let session = session.clone();
                                view.reconnect_with(
                                    move |cols, rows, bytes_tx| session.open_shell(cols, rows, bytes_tx),
                                    cx,
                                );
                            });
                        }
                        // Don't yank the side panels to this host if the
                        // user has since focused a different tab — the
                        // on-focus handler above will show them correctly
                        // if the user comes back to this tab later.
                        let is_focused = this
                            .focused_terminal
                            .as_ref()
                            .map(|w| w.entity_id())
                            == Some(term_for_connect.entity_id());
                        if is_focused {
                            this.show_sftp(config.clone(), window, cx);
                            this.show_monitor(config, window, cx);
                        }
                    });
                }
                Err(e) => {
                    log::error!("SSH connect to {key} failed: {e}");
                    let _ = term_for_connect.update(cx, |view, cx| {
                        view.mark_connect_failed(format!("连接失败: {e}"), cx);
                    });
                }
            }
        })
        .detach();
    }
```

- [ ] **Step 2: Update `reconnect_ssh_terminal`**

Replace the method (currently `src/workspace.rs:332-368`):

```rust
    fn reconnect_ssh_terminal(
        &mut self,
        terminal: Entity<TerminalView>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.ssh_reconnect_configs.get(&terminal.entity_id()).cloned() else {
            return;
        };
        self.ssh_sessions.remove(&config.key());
        terminal.update(cx, |view, cx| view.mark_connecting(cx));

        let dial_config = config.clone();
        let connect_task = cx.background_spawn(async move { SshSession::connect(dial_config) });
        cx.spawn(async move |this, cx| {
            match connect_task.await {
                Ok(session) => {
                    let key = config.key();
                    let _ = this.update(cx, |this, _cx| {
                        this.ssh_sessions.insert(key, session.clone());
                    });
                    let _ = terminal.update(cx, |view, cx| {
                        let session = session.clone();
                        view.reconnect_with(
                            move |cols, rows, bytes_tx| session.open_shell(cols, rows, bytes_tx),
                            cx,
                        );
                    });
                }
                Err(e) => {
                    log::error!("SSH reconnect to {} failed: {e}", config.key());
                    let _ = terminal.update(cx, |view, cx| {
                        view.mark_connect_failed(format!("连接失败: {e}"), cx);
                    });
                }
            }
        })
        .detach();
    }
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds, no new warnings (`new_ssh_connecting`/`mark_connect_failed`/`mark_connecting` are all called now).

Run: `cargo test --bin caracal`
Expected: PASS.

Run: `cargo clippy --bin caracal 2>&1 | grep -i "workspace.rs\|view.rs"`
Expected: no new lines beyond whatever pre-existing warnings this repo already carries in these files (check with `git stash` + rerun if unsure which warnings are pre-existing).

- [ ] **Step 4: Manual smoke test**

Run: `cargo run` (or `cargo build --release && ./target/release/caracal` if a debug run is too slow on your machine).

1. Double-click a saved connection to a **reachable** host. Confirm: the terminal tab appears immediately (no freeze), shows "正在连接 user@host…", then flips to the live shell prompt once connected.
2. Double-click a saved connection to an **unreachable host or wrong port** (or temporarily edit one to a bad port). Confirm: tab appears immediately, banner flips to "user@host 连接失败: `<reason>`，按 Enter 重连", and pressing Enter retries (banner goes back to "正在连接…" during the retry).
3. Kill the remote `sshd` (or otherwise drop an established connection) on a live tab. Confirm a "`user@host` 连接已断开，按 Enter 重连" banner appears (host-prefixed — this is now `conn_banner_text`'s shared template, not the old fixed string) and Enter still reconnects (this is the pre-existing `mark_disconnected` path — confirm the reconnect *behavior* is unaffected, even though the exact wording changed under Task 2).

- [ ] **Step 5: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: open SSH terminal tabs instantly, connect in the background, show failures inline"
```

---

### Task 5: `TerminalPanelEvent::Closed` + `Panel::on_removed`

**Files:**
- Modify: `src/panels/terminal.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub enum TerminalPanelEvent { Closed }` + `impl gpui::EventEmitter<TerminalPanelEvent> for TerminalPanel {}`; `Panel::on_removed` override that emits `Closed`. Task 6 subscribes to this from `Workspace::open_ssh`.

This event fires for every terminal tab (local/telnet/serial too, not just SSH) — `TerminalPanel` stays generic and doesn't know why closing matters; only `Workspace` (Task 6) decides what to do with it, and only for SSH tabs.

- [ ] **Step 1: Add the event type**

In `src/panels/terminal.rs`, right after the existing `impl gpui::EventEmitter<PanelEvent> for TerminalPanel {}` line (currently line 71):

```rust
impl gpui::EventEmitter<PanelEvent> for TerminalPanel {}

/// Emitted when the dock actually removes this panel
/// (`Panel::on_removed`, below) — a generic, backend-agnostic "this tab
/// is gone" signal. `TerminalPanel` doesn't know or care why (matches
/// its "adapter only" mandate, file header above); `Workspace` is the
/// one that knows what, if anything, needs cleaning up for a given
/// backend kind (see `open_ssh` in `workspace.rs`, the only current
/// subscriber — local/Telnet/Serial tabs emit this too, just unobserved,
/// since they share no session to clean up).
#[derive(Clone, Debug)]
pub enum TerminalPanelEvent {
    Closed,
}

impl gpui::EventEmitter<TerminalPanelEvent> for TerminalPanel {}
```

- [ ] **Step 2: Override `on_removed`**

In the `impl Panel for TerminalPanel` block, add after `on_added_to` (currently the block ends at line 122-123):

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

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds, no new warnings (`on_removed` is a trait method the framework calls directly regardless of subscribers, so it's not dead code; `TerminalPanelEvent::Closed` is constructed inside it).

Run: `cargo test --bin caracal`
Expected: PASS (unchanged — no testable pure logic added here).

- [ ] **Step 4: Commit**

```bash
git add src/panels/terminal.rs
git commit -m "feat: emit TerminalPanelEvent::Closed when a terminal tab is removed"
```

---

### Task 6: Subscribe to `Closed` and evict the session on last-tab-close

**Files:**
- Modify: `src/workspace.rs`

**Interfaces:**
- Consumes: `TerminalPanelEvent::Closed` (Task 5); `open_ssh`'s structure from Task 4 (specifically the `let panel = cx.new(|_cx| TerminalPanel::new(terminal));` / `self.add_center(...)` pair).
- Produces: `Workspace::handle_ssh_tab_closed(&mut self, config: SshConfig, terminal: &WeakEntity<TerminalView>, window: &mut Window, cx: &mut Context<Self>)` (private). Nothing later consumes this — it's the end of the chain.

This delivers requirement 3. Verified by manual smoke test (Step 4) — the "last tab for a host" check is a small `HashMap` scan not worth extracting purely for testability (matches this codebase's existing style for similarly-sized checks, e.g. `drag_reorder_target` in `saved_connections.rs`).

- [ ] **Step 1: Import `TerminalPanelEvent`**

In `src/workspace.rs`, change:

```rust
use crate::panels::terminal::TerminalPanel;
```

to:

```rust
use crate::panels::terminal::{TerminalPanel, TerminalPanelEvent};
```

- [ ] **Step 2: Update the `ssh_reconnect_configs` doc comment**

Replace (currently `src/workspace.rs:61-67`):

```rust
    /// The `SshConfig` behind each SSH-backed `TerminalView`, so a
    /// `TerminalViewEvent::ReconnectRequested` (user pressed Enter on a
    /// disconnected tab) knows which host to redial. Populated in
    /// `open_ssh`, pruned on tab close (`handle_ssh_tab_closed`) — also
    /// doubles as the "how many tabs does this host still have open"
    /// count that decides whether closing a tab should tear down the
    /// shared session.
    ssh_reconnect_configs: HashMap<EntityId, SshConfig>,
```

- [ ] **Step 3: Subscribe to `Closed` inside `open_ssh`**

In `open_ssh` (from Task 4), insert between the panel's creation and `add_center`:

```rust
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let closed_config = config.clone();
        let closed_term = term_weak.clone();
        let closed_sub = cx.subscribe_in(&panel, window, move |this, _panel, event, window, cx| {
            let TerminalPanelEvent::Closed = event;
            this.handle_ssh_tab_closed(closed_config.clone(), &closed_term, window, cx);
        });
        self._subscriptions.push(closed_sub);
        self.add_center(Arc::new(panel), window, cx);
```

(this replaces the two-line `let panel = ...; self.add_center(...);` pair from Task 4's Step 1 with the five-line version above).

- [ ] **Step 4: Add `handle_ssh_tab_closed`**

Add as a new method on `Workspace` (a sensible spot is right after `reconnect_ssh_terminal`):

```rust
    /// Cleanup when an SSH-backed terminal tab is removed from the dock
    /// (`TerminalPanelEvent::Closed`, emitted by `TerminalPanel::on_removed`).
    /// If any other tab for the same host is still open, does nothing —
    /// the shared session is still needed. Otherwise evicts the cached
    /// `SshSession` and closes that host's SFTP/monitor panels (falling
    /// back to their placeholders if either was the one currently shown).
    fn handle_ssh_tab_closed(
        &mut self,
        config: SshConfig,
        terminal: &WeakEntity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ssh_reconnect_configs.remove(&terminal.entity_id());
        let key = config.key();
        let any_tabs_left = self.ssh_reconnect_configs.values().any(|c| c.key() == key);
        if any_tabs_left {
            return;
        }
        self.ssh_sessions.remove(&key);
        self.sftp_panels.remove(&key);
        self.monitor_panels.remove(&key);
        if self.active_sftp.as_deref() == Some(key.as_str()) {
            self.show_sftp_placeholder(window, cx);
        }
        if self.active_monitor.as_deref() == Some(key.as_str()) {
            self.show_monitor_placeholder(window, cx);
        }
        cx.notify();
    }
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: succeeds, no new warnings.

Run: `cargo test --bin caracal`
Expected: PASS.

- [ ] **Step 6: Manual smoke test**

Run: `cargo run`.

1. Open two terminal tabs to the **same** saved SSH host (double-click it twice). Confirm both share one session (e.g. SFTP panel shows the same host without a second connect delay).
2. Close one of the two tabs. Confirm the session, SFTP panel, and monitor panel are still alive (focus the remaining tab, open SFTP — should still work without redialing).
3. Close the second (last) tab for that host. Confirm: if SFTP or the monitor panel was showing that host, it falls back to its placeholder; reopening the same connection afterward dials fresh (not reusing a torn-down session — e.g. via a log line or simply confirming it still works end-to-end).

- [ ] **Step 7: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: evict SSH session and close its SFTP/monitor panels on last-tab-close"
```
