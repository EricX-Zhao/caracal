# Sidebar Resize Fix + SFTP Browser Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix a layout bug where closing either sidebar corrupts the other panels' resize behavior, and add three SFTP browser improvements: whole-folder upload/download, a blank-area right-click menu, and a "clear completed transfers" icon.

**Architecture:** Task 1 replaces `workspace.rs`'s `gpui_component::resizable` body-split group with a hand-rolled drag implementation (the same pattern already used for the quick-commands drawer), eliminating the upstream library bug at its root. Tasks 2–5 extend the existing SFTP transfer plumbing (`ssh.rs`'s `SftpRequest`/`TransferEvent`, `sftp.rs`'s `Transfer`/`TransferStatus`) to support directories as a first-class transfer unit, then wire three small UI additions on top of it.

**Tech Stack:** Rust, `gpui`/`gpui_component` (UI), `russh`/`russh_sftp` (SSH/SFTP), `tokio` (async runtime), `rust_i18n` (locale strings in `locales/app.yml`).

## Global Constraints

- One Session = one connection (CLAUDE.md §2): all new SFTP work reuses the existing `SshSession`/`sftp_slot` — never opens a second connection or a second SFTP subsystem channel.
- Every new user-facing string needs both `zh-CN` and `en` entries in `locales/app.yml` under the `Sftp:` key, referenced via `rust_i18n::t!("Sftp.key", ...)` — never a hardcoded literal in a `.child(...)`/`.tooltip(...)` call.
- No screenshot-driven GUI verification — the user's `DISPLAY=:0` is their real desktop. Where a step calls for manual verification, describe what to check and ask the user to confirm rather than attempting a screenshot.
- Verification commands follow this repo's existing convention: `cargo build` for compile checks, `cargo test --lib <module::path>` for scoped unit tests, matching e.g. `docs/superpowers/plans/2026-07-14-frameless-window-titlebar.md`.
- Commit messages: plain, present-tense, no "Co-Authored-By" trailer unless the user's global git workflow adds one automatically.

---

## Task 1: Fix sidebar resize corruption (hand-roll the body-split layout)

**Files:**
- Modify: `src/workspace.rs:9` (module doc comment), `src/workspace.rs:39` (import), `src/workspace.rs:283-300` (struct fields), `src/workspace.rs:434` + `src/workspace.rs:456-468` (constructor), `src/workspace.rs:1196-1241` (new resize methods, inserted here), `src/workspace.rs:1311-1422` (`render_body` rewrite), `src/workspace.rs:1485-1490` (`Render for Workspace`'s mouse wiring)

**Interfaces:**
- Consumes: nothing new from other tasks (self-contained).
- Produces: nothing consumed by later tasks (this task is independent of Tasks 2–5).

**Root cause** (confirmed by reading the vendored `gpui_component` source at the pinned rev `1505b1487131adbb443f6c69e87847db35bfa2d1`): the body row is one `h_resizable("body-split")` group with three permanent children (left region, center dock, right region); closing a sidebar only toggles `.visible(false)`. `ResizablePanel::render` special-cases invisible panels by rendering a bare `div()` with no `on_prepaint` hook, so the shared `ResizableState`'s `bounds`/`size` for that panel index freeze at their last value before hiding. Every drag calls `sync_real_panel_sizes`, which re-derives *all* panels' sizes — including the hidden, stale one — so the computed total overshoots the real container width, and the resulting overflow-correction claws that phantom width back out of whichever panel is actually being dragged, differently on every mouse-move frame (the flicker). This task removes `gpui_component::resizable` from the body split entirely rather than working around the bug.

There is no automated test for this task — it's pure UI layout/mouse-interaction code with no existing test harness in this file for that category (the file's current `#[cfg(test)]` module only covers pure helper functions like `resolve_appearance_font`). Verification is `cargo build` plus a manual drag-test checklist at the end.

- [ ] **Step 1: Remove the `gpui_component::resizable` import**

In `src/workspace.rs`, delete line 39:

```rust
use gpui_component::resizable::{ResizableState, resizable_panel, h_resizable};
```

- [ ] **Step 2: Update the module doc comment**

In `src/workspace.rs`, change:

```rust
//! single-panel containers whose content is chosen by the activity bars and
//! whose widths are controlled by an `h_resizable` group surrounding the body.
```

to:

```rust
//! single-panel containers whose content is chosen by the activity bars and
//! whose widths are controlled by hand-rolled drag handles in `render_body`
//! (not a `gpui_component` `h_resizable` group — see `left_width`'s doc
//! comment for why).
```

- [ ] **Step 3: Replace the `body_resize` field with plain width/drag fields**

In `src/workspace.rs`, find:

```rust
    // --- horizontal body resize state ---------------------------------------
    body_resize: Entity<ResizableState>,
    /// Current height of the quick-commands drawer, dragged via the handle
    /// at its top edge. A plain field (not a `gpui_component` resizable
    /// group) — nesting a `v_resizable` group inside one panel of the outer
    /// `h_resizable("body-split")` group corrupted the *sibling* panels'
    /// (left/right side regions) layout entirely, so the drag here is
    /// implemented directly with raw mouse events instead
    /// (`start_quick_commands_resize`/`on_quick_commands_drag_move`/
    /// `stop_quick_commands_resize`).
    quick_commands_height: Pixels,
```

Replace with:

```rust
    // --- horizontal body resize state ---------------------------------------
    /// Width of the left side region. A plain field, not a `gpui_component`
    /// `ResizableState` — that state type keeps stale `bounds`/`size` for
    /// whichever body-split panel is currently hidden via `.visible(false)`
    /// (see `render_body`'s doc comment), which corrupted the *other*
    /// panels' resize math (wrong min-width clamp + flicker while dragging).
    /// Hand-rolled the same way the quick-commands drawer's height already
    /// is, via raw mouse events (`start_left_resize`/`on_left_drag_move`/
    /// `stop_left_resize`, and the `_right_` equivalents below).
    left_width: Pixels,
    /// `Some((mouse_x_at_drag_start, width_at_drag_start))` while the
    /// left-region resize handle is being dragged; `None` otherwise.
    left_drag: Option<(Pixels, Pixels)>,
    /// Width of the right side region — mirrors `left_width` field-for-field.
    right_width: Pixels,
    right_drag: Option<(Pixels, Pixels)>,
    /// Current height of the quick-commands drawer, dragged via the handle
    /// at its top edge — the original hand-rolled-resize precedent that
    /// `left_width`/`right_width` above now follow too.
    quick_commands_height: Pixels,
```

- [ ] **Step 4: Update the constructor**

In `src/workspace.rs`, delete this line (around line 434):

```rust
        let body_resize = cx.new(|_| ResizableState::default());
```

Then find, in the `Self { ... }` struct literal:

```rust
            left_active: None,
            right_active: Some(PanelId::Sessions),
            body_resize,
            quick_commands_height: px(220.0),
```

Replace with:

```rust
            left_active: None,
            right_active: Some(PanelId::Sessions),
            left_width: px(200.0),
            left_drag: None,
            right_width: px(220.0),
            right_drag: None,
            quick_commands_height: px(220.0),
```

- [ ] **Step 5: Run `cargo build` — expect errors naming `render_body`/methods not yet updated**

Run: `cargo build`
Expected: FAIL — `body_resize` no longer exists but `render_body` (Step 7) still references it, and `ResizableState`/`resizable_panel`/`h_resizable` are still referenced there too. This is expected; Steps 6–8 fix it.

- [ ] **Step 6: Add the left/right resize drag methods**

In `src/workspace.rs`, find the end of `stop_quick_commands_resize`:

```rust
    fn stop_quick_commands_resize(
        &mut self,
        _ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quick_commands_drag.take().is_some() {
            cx.notify();
        }
    }
}
```

Insert new methods before that closing `}` (i.e. still inside the same `impl Workspace` block):

```rust
    fn stop_quick_commands_resize(
        &mut self,
        _ev: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quick_commands_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Mouse-down on the left region's resize handle: record where the drag
    /// started (mouse X + current width) so `on_left_drag_move` can compute
    /// the new width from the delta.
    fn start_left_resize(&mut self, ev: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.left_drag = Some((ev.position.x, self.left_width));
        cx.notify();
    }

    /// No-ops when not currently dragging (called unconditionally from the
    /// outer `Render for Workspace`'s `on_mouse_move`, same pattern as
    /// `on_quick_commands_drag_move`).
    fn on_left_drag_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.left_drag else {
            return;
        };
        let delta = ev.position.x - start_x;
        let new_width = (start_width + delta).clamp(px(180.0), px(560.0));
        if new_width != self.left_width {
            self.left_width = new_width;
            cx.notify();
        }
    }

    fn stop_left_resize(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.left_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Mirror of `start_left_resize` for the right region.
    fn start_right_resize(&mut self, ev: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.right_drag = Some((ev.position.x, self.right_width));
        cx.notify();
    }

    /// Dragging this handle left (toward the center) should *grow* the
    /// right region, hence `start_x - mouse_x` — the mirror image of
    /// `on_left_drag_move`'s `mouse_x - start_x`.
    fn on_right_drag_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.right_drag else {
            return;
        };
        let delta = start_x - ev.position.x;
        let new_width = (start_width + delta).clamp(px(200.0), px(500.0));
        if new_width != self.right_width {
            self.right_width = new_width;
            cx.notify();
        }
    }

    fn stop_right_resize(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.right_drag.take().is_some() {
            cx.notify();
        }
    }
}
```

(Note the duplicated `stop_quick_commands_resize` above is intentional — it shows the exact anchor text to find, followed by everything to insert after it, ending with the same closing `}` the original had.)

- [ ] **Step 7: Rewrite `render_body`**

In `src/workspace.rs`, replace the entire `render_body` function (from its doc comment through its closing `}`, currently spanning roughly lines 1311–1422) with:

```rust
    /// The body row between the two activity bars: left region | center dock
    /// | right region, with a hand-rolled drag handle between each side
    /// region and the center (see `left_width`'s doc comment for why this
    /// isn't a `gpui_component` `h_resizable` group).
    ///
    /// Side widths are absolute pixels that do **not** rescale when the
    /// window itself resizes — VSCode-style: a sidebar keeps whatever width
    /// the user last dragged it to, and the center dock absorbs the change
    /// via its own `flex_1`.
    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let border = cx.theme().border;
        let left_view = self.left_active.and_then(|id| self.resolve(id));
        let right_view = self.right_active.and_then(|id| self.resolve(id));
        let show_left = left_view.is_some();
        let show_right = right_view.is_some();

        let left_region = if let Some(view) = left_view {
            div()
                .h_full()
                .flex_shrink_0()
                .w(self.left_width)
                .child(side_region_content(view, border, true))
                .into_any_element()
        } else {
            div().h_full().flex_shrink_0().w(px(0.0)).into_any_element()
        };

        let left_handle = if show_left {
            div()
                .id("body-left-resize-handle")
                .h_full()
                .w(px(4.0))
                .flex_shrink_0()
                .cursor(gpui::CursorStyle::ResizeColumn)
                .bg(border)
                .hover(|s| s.bg(cx.theme().primary))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::start_left_resize))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let right_handle = if show_right {
            div()
                .id("body-right-resize-handle")
                .h_full()
                .w(px(4.0))
                .flex_shrink_0()
                .cursor(gpui::CursorStyle::ResizeColumn)
                .bg(border)
                .hover(|s| s.bg(cx.theme().primary))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::start_right_resize))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let right_region = if let Some(view) = right_view {
            div()
                .h_full()
                .flex_shrink_0()
                .w(self.right_width)
                .child(side_region_content(view, border, false))
                .into_any_element()
        } else {
            div().h_full().flex_shrink_0().w(px(0.0)).into_any_element()
        };

        let center = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(self.dock_area.clone()),
            )
            .when(self.show_quick_commands, |d| {
                d.child(
                    div()
                        .w_full()
                        .h(self.quick_commands_height)
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .child(
                            // Drag handle: a thin strip at the drawer's top
                            // edge. Mouse-move/up are handled on the outer
                            // `Render for Workspace` div (`on_quick_commands_
                            // drag_move`/`stop_quick_commands_resize`) so the
                            // drag keeps tracking even if the cursor strays
                            // off this thin strip mid-drag.
                            div()
                                .id("quick-commands-resize-handle")
                                .w_full()
                                .h(px(4.0))
                                .flex_shrink_0()
                                .cursor(gpui::CursorStyle::ResizeRow)
                                .bg(border)
                                .hover(|s| s.bg(cx.theme().primary))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::start_quick_commands_resize),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.0))
                                .overflow_hidden()
                                .child(self.quick_commands_panel.clone()),
                        ),
                )
            });

        // Wrap in a flex_1 container so the row fills the remaining row
        // width alongside the two fixed 44px activity bars.
        div().flex_1().min_w(px(0.0)).child(
            div()
                .flex()
                .flex_row()
                .size_full()
                .child(left_region)
                .child(left_handle)
                .child(center)
                .child(right_handle)
                .child(right_region),
        )
    }
```

- [ ] **Step 8: Wire the new drag handlers into `Render for Workspace`**

In `src/workspace.rs`, find in the `Render for Workspace` impl:

```rust
            .on_mouse_move(cx.listener(Self::on_quick_commands_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_quick_commands_resize))
```

Replace with:

```rust
            .on_mouse_move(cx.listener(Self::on_quick_commands_drag_move))
            .on_mouse_move(cx.listener(Self::on_left_drag_move))
            .on_mouse_move(cx.listener(Self::on_right_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_quick_commands_resize))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_left_resize))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_right_resize))
```

(`gpui::Div::on_mouse_move`/`on_mouse_up` push onto an internal `Vec` of listeners rather than replacing a single slot, so multiple calls accumulate independently — confirmed by reading `crates/gpui/src/elements/div.rs` in the vendored `gpui` checkout. Each handler no-ops when its own `_drag` field is `None`, so there's no interference between the three independent drags.)

- [ ] **Step 9: Build**

Run: `cargo build`
Expected: succeeds with no errors. Pre-existing warnings unrelated to this change are fine; there should be no new warnings about unused `ResizableState`/`resizable_panel`/`h_resizable` (they're no longer imported).

- [ ] **Step 10: Manual verification (ask the user — no screenshot-driven check)**

Ask the user to run the app (`cargo run`) and confirm, for each of the four combinations of left-open/closed × right-open/closed:
- Both sidebars resize smoothly when dragging their handles, with no visible flicker.
- Each sidebar respects its min/max width (left: 180–560px, right: 200–500px) — dragging past either bound stops cleanly at the bound rather than jumping or oscillating.
- Closing one sidebar and then dragging the *other* sidebar's handle behaves exactly as it did before either sidebar was touched (this is the bug's actual repro case).
- Resizing the OS window itself keeps both sidebars at their last dragged width, growing/shrinking only the center terminal area.

- [ ] **Step 11: Commit**

```bash
git add src/workspace.rs
git commit -m "fix: hand-roll body-split sidebar resize to fix min-width/flicker bug

gpui_component's ResizableState keeps stale bounds for a hidden panel,
corrupting the resize math for its visible siblings. Replacing the
h_resizable group with plain width fields + raw mouse events (the same
pattern already used for the quick-commands drawer) removes the bug at
its root instead of working around it."
```

---

## Task 2: SFTP backend — recursive directory transfer support

**Files:**
- Modify: `src/terminal/ssh.rs:10` (import), `src/terminal/ssh.rs:160-187` (`TransferEvent` enum), `src/terminal/ssh.rs:95-139` (`SftpRequest` enum), `src/terminal/ssh.rs:346-362` (new `SshSession` methods, inserted after `sftp_upload`), `src/terminal/ssh.rs:823-877` (new `service_sftp` match arms, inserted after the `Upload` arm), `src/terminal/ssh.rs:1062` (`reply_sftp_error` match arm), `src/terminal/ssh.rs:1096` (new helper functions, inserted after `sftp_read_dir`)

**Interfaces:**
- Consumes: `SftpEntry` (existing, `src/terminal/ssh.rs:62-73`), `remote_join` (existing private fn, `src/terminal/ssh.rs:1044`), `StreamingOutcome` (existing, `src/terminal/ssh.rs:1101-1104`), `sftp_read_dir` (existing, `src/terminal/ssh.rs:1078`).
- Produces: `SshSession::sftp_download_dir(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle>`, `SshSession::sftp_upload_dir(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle>`, and a new `TransferEvent::DoneWithFailures { transferred: u64, failed_paths: Vec<String> }` variant — all consumed by Task 3.

There's no test server available in this environment, so the recursive walk/transfer logic can't be exercised by an automated test the way `is_sftp_session_dead` is (a pure string-matching function). Verification here is `cargo build` + `cargo test --lib terminal::ssh` (to confirm the existing tests still pass) + a manual end-to-end test folded into Task 3's verification (once the UI can trigger a folder transfer).

- [ ] **Step 1: Add `Path` to the `std::path` import**

In `src/terminal/ssh.rs`, change:

```rust
use std::path::PathBuf;
```

to:

```rust
use std::path::{Path, PathBuf};
```

- [ ] **Step 2: Add the `DoneWithFailures` `TransferEvent` variant**

In `src/terminal/ssh.rs`, find:

```rust
    /// Transfer was cancelled by the user. Partial bytes may have been
    /// transferred; the streaming loop stopped and the remote/local file is
    /// left in whatever state was reached before the abort.
    Cancelled {
        transferred: u64,
    },
}
```

Replace with:

```rust
    /// Transfer was cancelled by the user. Partial bytes may have been
    /// transferred; the streaming loop stopped and the remote/local file is
    /// left in whatever state was reached before the abort.
    Cancelled {
        transferred: u64,
    },
    /// A directory transfer (`sftp_download_dir`/`sftp_upload_dir`) finished
    /// after skipping one or more files that failed individually
    /// (permission denied, vanished mid-walk, etc.) — the rest of the
    /// directory still completed. Never emitted for single-file transfers.
    DoneWithFailures {
        transferred: u64,
        failed_paths: Vec<String>,
    },
}
```

- [ ] **Step 3: Add `DownloadDir`/`UploadDir` to `SftpRequest`**

In `src/terminal/ssh.rs`, find:

```rust
    Upload {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Create a directory on the remote.
```

Replace with:

```rust
    Upload {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Recursively download every file under `remote` into `local`,
    /// creating matching subdirectories as needed. One `TransferHandle` /
    /// event stream for the whole job — see `TransferEvent::DoneWithFailures`.
    DownloadDir {
        remote: String,
        local: PathBuf,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Recursively upload every file under `local` into `remote`. Mirrors
    /// `DownloadDir`.
    UploadDir {
        local: PathBuf,
        remote: String,
        reply: flume::Sender<TransferHandle>,
        id: u64,
        cancel: Arc<AtomicBool>,
        cancels: Arc<std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    },
    /// Create a directory on the remote.
```

- [ ] **Step 4: Add the two public `SshSession` methods**

In `src/terminal/ssh.rs`, find the end of `sftp_upload`:

```rust
    pub fn sftp_upload(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Upload {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }
```

Insert immediately after it:

```rust

    /// Recursively download `remote` (a directory) into `local`. Same
    /// `TransferHandle`/event-stream shape as [`Self::sftp_download`], but
    /// `Started.total`/`Progress.transferred` are aggregate across every
    /// file in the tree, and the terminal event may be
    /// [`TransferEvent::DoneWithFailures`] if some files failed.
    pub fn sftp_download_dir(&self, remote: String, local: PathBuf) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::DownloadDir {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }

    /// Recursively upload `local` (a directory) into `remote`. Mirrors
    /// [`Self::sftp_download_dir`].
    pub fn sftp_upload_dir(&self, local: PathBuf, remote: String) -> flume::Receiver<TransferHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().unwrap().insert(id, cancel.clone());
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::UploadDir {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels: self.cancels.clone(),
        }));
        rx
    }
```

- [ ] **Step 5: Update `reply_sftp_error`'s match arm**

In `src/terminal/ssh.rs`, find:

```rust
        SftpRequest::Download { reply, id, .. } | SftpRequest::Upload { reply, id, .. } => {
```

Replace with:

```rust
        SftpRequest::Download { reply, id, .. }
        | SftpRequest::Upload { reply, id, .. }
        | SftpRequest::DownloadDir { reply, id, .. }
        | SftpRequest::UploadDir { reply, id, .. } => {
```

- [ ] **Step 6: Add the walk/transfer helper functions**

In `src/terminal/ssh.rs`, find the end of `sftp_read_dir`:

```rust
async fn sftp_read_dir(sftp: &SftpSession, path: &str) -> Result<Vec<SftpEntry>> {
    let mut entries = Vec::new();
    for entry in sftp.read_dir(path).await? {
        let md = entry.metadata();
        entries.push(SftpEntry {
            name: entry.file_name(),
            is_dir: entry.file_type().is_dir(),
            size: md.size.unwrap_or(0),
            mtime: md.mtime.unwrap_or(0),
            // `permissions` carries the file-type mode bits too (e.g.
            // 0o100644 / 0o040755). The panel slices out the 9 perm bits
            // for display; the file-type bits are ignored for the table.
            perms: md.permissions.unwrap_or(0),
        });
    }
    // Directories first, then alphabetical.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}
```

Insert immediately after it:

```rust

/// One file to transfer as part of a recursive directory job: its remote
/// path, its local path, and its size (known up front from the initial
/// directory walk — used to size the job's aggregate progress bar).
struct DirTransferItem {
    remote: String,
    local: PathBuf,
    size: u64,
}

/// Recursively walk `remote_root` via SFTP `read_dir`, creating the
/// matching local directory tree under `local_root` as it goes, and
/// collecting every remote *file* (not directory) found along the way.
async fn walk_remote_dir(
    sftp: &SftpSession,
    remote_root: &str,
    local_root: &Path,
) -> Result<Vec<DirTransferItem>> {
    let mut items = Vec::new();
    let mut stack = vec![(remote_root.to_string(), local_root.to_path_buf())];
    while let Some((remote_dir, local_dir)) = stack.pop() {
        tokio::fs::create_dir_all(&local_dir)
            .await
            .map_err(|e| anyhow!("create_dir_all {local_dir:?}: {e}"))?;
        for entry in sftp_read_dir(sftp, &remote_dir).await? {
            let remote_path = remote_join(&remote_dir, &entry.name);
            let local_path = local_dir.join(&entry.name);
            if entry.is_dir {
                stack.push((remote_path, local_path));
            } else {
                items.push(DirTransferItem {
                    remote: remote_path,
                    local: local_path,
                    size: entry.size,
                });
            }
        }
    }
    Ok(items)
}

/// Recursively walk `local_root` on disk, creating the matching remote
/// directory tree under `remote_root` via SFTP `mkdir` as it goes (best
/// effort — a directory that already exists on the remote is not an error
/// here), and collecting every local *file* found along the way.
async fn walk_local_dir(
    sftp: &SftpSession,
    local_root: &Path,
    remote_root: &str,
) -> Result<Vec<DirTransferItem>> {
    let mut items = Vec::new();
    let mut stack = vec![(local_root.to_path_buf(), remote_root.to_string())];
    while let Some((local_dir, remote_dir)) = stack.pop() {
        let _ = sftp.create_dir(&remote_dir).await;
        let mut rd = tokio::fs::read_dir(&local_dir)
            .await
            .map_err(|e| anyhow!("read_dir {local_dir:?}: {e}"))?;
        while let Some(entry) = rd
            .next_entry()
            .await
            .map_err(|e| anyhow!("read_dir {local_dir:?}: {e}"))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|e| anyhow!("file_type {:?}: {e}", entry.path()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let remote_path = remote_join(&remote_dir, &name);
            if file_type.is_dir() {
                stack.push((entry.path(), remote_path));
            } else {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                items.push(DirTransferItem {
                    remote: remote_path,
                    local: entry.path(),
                    size,
                });
            }
        }
    }
    Ok(items)
}

/// Download a single file within a directory job. Unlike
/// `sftp_download_streaming`, this never emits `Progress` events itself —
/// the caller (`run_download_dir`) reports progress once per completed
/// file, using a running total across the whole directory, not per-chunk.
async fn download_one_file(
    sftp: &SftpSession,
    remote: &str,
    local: &PathBuf,
    cancel: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    if let Some(parent) = local.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let mut remote_file = sftp.open(remote).await.map_err(|e| anyhow!("open {remote:?}: {e}"))?;
    let mut local_file = tokio::fs::File::create(local)
        .await
        .map_err(|e| anyhow!("create {local:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = remote_file.read(&mut buf).await.map_err(|e| anyhow!("read {remote:?}: {e}"))?;
        if n == 0 {
            break;
        }
        local_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {local:?}: {e}"))?;
        transferred += n as u64;
    }
    local_file.flush().await.map_err(|e| anyhow!("flush {local:?}: {e}"))?;
    Ok(StreamingOutcome::Completed(transferred))
}

/// Upload a single file within a directory job. Mirrors `download_one_file`
/// — no intermediate `Progress` events, just a final byte count.
async fn upload_one_file(
    sftp: &SftpSession,
    local: &PathBuf,
    remote: &str,
    cancel: &AtomicBool,
) -> Result<StreamingOutcome> {
    const CHUNK: usize = 32 * 1024;
    let mut local_file = tokio::fs::File::open(local).await.map_err(|e| anyhow!("open {local:?}: {e}"))?;
    let mut remote_file = sftp
        .open_with_flags(remote, OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE)
        .await
        .map_err(|e| anyhow!("create {remote:?}: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut transferred: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(StreamingOutcome::Cancelled(transferred));
        }
        let n = local_file.read(&mut buf).await.map_err(|e| anyhow!("read {local:?}: {e}"))?;
        if n == 0 {
            break;
        }
        remote_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| anyhow!("write {remote:?}: {e}"))?;
        transferred += n as u64;
    }
    remote_file.shutdown().await.map_err(|e| anyhow!("close {remote:?}: {e}"))?;
    Ok(StreamingOutcome::Completed(transferred))
}

/// Download every item in `items` sequentially, emitting one aggregate
/// `Started`/`Progress` pair across the whole job (not per-file) plus a
/// final `Done`/`DoneWithFailures`/`Cancelled`. A single file's failure is
/// logged and skipped — the job keeps going (matches `sftp_remove`'s
/// existing best-effort-recursion style).
async fn run_download_dir(
    sftp: &SftpSession,
    items: Vec<DirTransferItem>,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
) {
    let total: u64 = items.iter().map(|i| i.size).sum();
    let _ = events.send(TransferEvent::Started { total });

    let mut transferred: u64 = 0;
    let mut failed_paths = Vec::new();
    let mut cancelled = false;

    for item in items {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        match download_one_file(sftp, &item.remote, &item.local, cancel).await {
            Ok(StreamingOutcome::Completed(n)) => {
                transferred += n;
                let _ = events.send(TransferEvent::Progress { transferred });
            }
            Ok(StreamingOutcome::Cancelled(n)) => {
                transferred += n;
                cancelled = true;
                break;
            }
            Err(e) => {
                log::warn!("sftp: download {:?} failed: {e:#}", item.remote);
                failed_paths.push(item.remote);
            }
        }
    }

    if cancelled {
        let _ = events.send(TransferEvent::Cancelled { transferred });
    } else if failed_paths.is_empty() {
        let _ = events.send(TransferEvent::Done { transferred });
    } else {
        let _ = events.send(TransferEvent::DoneWithFailures { transferred, failed_paths });
    }
}

/// Mirror of `run_download_dir` for uploads.
async fn run_upload_dir(
    sftp: &SftpSession,
    items: Vec<DirTransferItem>,
    events: &flume::Sender<TransferEvent>,
    cancel: &AtomicBool,
) {
    let total: u64 = items.iter().map(|i| i.size).sum();
    let _ = events.send(TransferEvent::Started { total });

    let mut transferred: u64 = 0;
    let mut failed_paths = Vec::new();
    let mut cancelled = false;

    for item in items {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        match upload_one_file(sftp, &item.local, &item.remote, cancel).await {
            Ok(StreamingOutcome::Completed(n)) => {
                transferred += n;
                let _ = events.send(TransferEvent::Progress { transferred });
            }
            Ok(StreamingOutcome::Cancelled(n)) => {
                transferred += n;
                cancelled = true;
                break;
            }
            Err(e) => {
                log::warn!("sftp: upload {:?} failed: {e:#}", item.remote);
                failed_paths.push(item.remote);
            }
        }
    }

    if cancelled {
        let _ = events.send(TransferEvent::Cancelled { transferred });
    } else if failed_paths.is_empty() {
        let _ = events.send(TransferEvent::Done { transferred });
    } else {
        let _ = events.send(TransferEvent::DoneWithFailures { transferred, failed_paths });
    }
}
```

- [ ] **Step 7: Add the `service_sftp` dispatch arms**

In `src/terminal/ssh.rs`, find the end of the `SftpRequest::Upload` arm (the closing of its `tokio::spawn(async move { ... });` block) followed by the start of `SftpRequest::Mkdir`:

```rust
                cancels.lock().unwrap().remove(&id);
            });
        }
        SftpRequest::Mkdir { path, reply } => {
```

Replace with:

```rust
                cancels.lock().unwrap().remove(&id);
            });
        }
        SftpRequest::DownloadDir {
            remote,
            local,
            reply,
            id,
            cancel,
            cancels,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let items = match walk_remote_dir(&sftp, &remote, &local).await {
                    Ok(items) => items,
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                run_download_dir(&sftp, items, &events_tx, &cancel).await;
                cancels.lock().unwrap().remove(&id);
            });
        }
        SftpRequest::UploadDir {
            local,
            remote,
            reply,
            id,
            cancel,
            cancels,
        } => {
            let (events_tx, events_rx) = flume::unbounded::<TransferEvent>();
            let _ = reply.send(TransferHandle { id, events: events_rx });
            let sftp_slot = sftp_slot.clone();
            tokio::spawn(async move {
                let sftp = {
                    let g = sftp_slot.lock().await;
                    match g.clone() {
                        Some(s) => s,
                        None => {
                            let _ = events_tx.send(TransferEvent::Failed {
                                error: "sftp session not initialized".into(),
                            });
                            cancels.lock().unwrap().remove(&id);
                            return;
                        }
                    }
                };
                let items = match walk_local_dir(&sftp, &local, &remote).await {
                    Ok(items) => items,
                    Err(e) => {
                        invalidate_sftp_if_dead(&sftp_slot, &e).await;
                        let _ = events_tx.send(TransferEvent::Failed {
                            error: format!("{e:#}"),
                        });
                        cancels.lock().unwrap().remove(&id);
                        return;
                    }
                };
                run_upload_dir(&sftp, items, &events_tx, &cancel).await;
                cancels.lock().unwrap().remove(&id);
            });
        }
        SftpRequest::Mkdir { path, reply } => {
```

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: succeeds with no errors or new warnings.

- [ ] **Step 9: Run the existing ssh.rs unit tests**

Run: `cargo test --lib terminal::ssh`
Expected: both existing tests (`recognizes_a_dead_sftp_session_by_its_error_text`, `does_not_misclassify_unrelated_sftp_errors_as_a_dead_session`) still pass. No new tests are added in this task — see this task's header note on why.

- [ ] **Step 10: Commit**

```bash
git add src/terminal/ssh.rs
git commit -m "feat: add recursive SFTP directory download/upload to the backend

sftp_download_dir/sftp_upload_dir walk the source tree up front (creating
matching destination directories as they go), then stream files
sequentially over the existing SFTP session, emitting aggregate
Started/Progress events across the whole job. A single file's failure
is logged and skipped rather than aborting the job; a final
DoneWithFailures event reports the skipped paths if any."
```

---

## Task 3: SFTP frontend — wire folder upload/download into the panel

**Files:**
- Modify: `src/panels/sftp.rs:55-61` (`TransferStatus` enum), `src/panels/sftp.rs:85-94` (`Transfer::progress`), `src/panels/sftp.rs:610-634` (`download_selected`), after `src/panels/sftp.rs:716` (new `download_dir_entry`/`upload_dir` methods), `src/panels/sftp.rs:985-1014` (`delete_transfer_file`), `src/panels/sftp.rs:1052-1092` (`pump_events`), `src/panels/sftp.rs:1422-1429` (toolbar), `src/panels/sftp.rs:1670-1699` (`render_transfer_body`'s status/color/menu logic)
- Modify: `locales/app.yml` (new/removed `Sftp:` keys)

**Interfaces:**
- Consumes: `SshSession::sftp_download_dir`/`sftp_upload_dir` and `TransferEvent::DoneWithFailures` from Task 2.
- Produces: `SftpPanel::download_dir_entry(&mut self, name: &str, cx)`, `SftpPanel::upload_dir(&mut self, cx)` — both consumed by Task 4's blank-area menu (`upload_dir`) and by `download_selected`'s own new branch.

- [ ] **Step 1: Write a failing test for `Transfer::progress()`'s new status**

In `src/panels/sftp.rs`, find the existing test module at the end of the file:

```rust
#[cfg(test)]
mod name_column_width_tests {
```

Add a new test module directly before it:

```rust
#[cfg(test)]
mod transfer_progress_tests {
    use super::*;

    fn transfer_with(status: TransferStatus, total: u64, transferred: u64) -> Transfer {
        Transfer {
            id: 1,
            name: "test".to_string(),
            direction: TransferDirection::Download,
            total,
            transferred,
            status,
            started_at: Instant::now(),
            local_path: PathBuf::new(),
        }
    }

    #[test]
    fn done_with_failures_reports_full_progress_when_total_is_zero() {
        let t = transfer_with(TransferStatus::DoneWithFailures(vec!["a.txt".into()]), 0, 0);
        assert_eq!(t.progress(), 1.0);
    }

    #[test]
    fn done_with_failures_reports_ratio_when_total_is_known() {
        let t = transfer_with(TransferStatus::DoneWithFailures(vec!["a.txt".into()]), 100, 80);
        assert_eq!(t.progress(), 0.8);
    }
}

#[cfg(test)]
mod name_column_width_tests {
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib panels::sftp::transfer_progress_tests`
Expected: FAIL to compile — `TransferStatus::DoneWithFailures` doesn't exist yet.

- [ ] **Step 3: Add the `DoneWithFailures` status variant and update `progress()`**

In `src/panels/sftp.rs`, find:

```rust
/// Status of a single transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TransferStatus {
    Queued,
    Active,
    Done,
    Failed(String),
    Cancelled,
}
```

Replace with:

```rust
/// Status of a single transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TransferStatus {
    Queued,
    Active,
    Done,
    /// A directory transfer completed but skipped one or more files
    /// (`TransferEvent::DoneWithFailures`) — carries their remote paths for
    /// display. Never constructed for single-file transfers.
    DoneWithFailures(Vec<String>),
    Failed(String),
    Cancelled,
}
```

Then find:

```rust
    fn progress(&self) -> f32 {
        if self.total == 0 {
            match self.status {
                TransferStatus::Done => 1.0,
                _ => 0.0,
            }
        } else {
            (self.transferred as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
```

Replace with:

```rust
    fn progress(&self) -> f32 {
        if self.total == 0 {
            match self.status {
                TransferStatus::Done | TransferStatus::DoneWithFailures(_) => 1.0,
                _ => 0.0,
            }
        } else {
            (self.transferred as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib panels::sftp::transfer_progress_tests`
Expected: PASS (2 tests). Note this will still fail to *compile* until Steps 5–9 below add the other required match arms (`pump_events`, `render_transfer_body`) that the compiler needs to be exhaustive — if `cargo test` fails with match-exhaustiveness errors instead of passing, continue through Step 9, then return to re-run this command.

- [ ] **Step 5: Add the folder-download branch to `download_selected` and the new `download_dir_entry` method**

In `src/panels/sftp.rs`, find:

```rust
    fn download_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, is_dir) = {
            let state = self.table_state.read(cx);
            let Some(ix) = state.selected_row() else {
                window.push_notification(
                    (NotificationType::Warning, rust_i18n::t!("Sftp.select_file_to_download")),
                    cx,
                );
                return;
            };
            let entries = &state.delegate().entries;
            let Some(entry) = entries.get(ix) else {
                return;
            };
            (entry.name.clone(), entry.is_dir)
        };
        if is_dir {
            window.push_notification(
                (NotificationType::Warning, rust_i18n::t!("Sftp.folder_download_unsupported")),
                cx,
            );
            return;
        }
        self.download(&name, cx);
    }
```

Replace with:

```rust
    fn download_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, is_dir) = {
            let state = self.table_state.read(cx);
            let Some(ix) = state.selected_row() else {
                window.push_notification(
                    (NotificationType::Warning, rust_i18n::t!("Sftp.select_file_to_download")),
                    cx,
                );
                return;
            };
            let entries = &state.delegate().entries;
            let Some(entry) = entries.get(ix) else {
                return;
            };
            (entry.name.clone(), entry.is_dir)
        };
        if is_dir {
            self.download_dir_entry(&name, cx);
        } else {
            self.download(&name, cx);
        }
    }

    /// Mirrors `download`, but for a directory entry: downloads the whole
    /// remote subtree into `self.download_dir.join(name)` via
    /// `sftp_download_dir`. The resulting `Transfer` row shows aggregate
    /// progress across every file in the tree (see
    /// `TransferEvent::DoneWithFailures` for the partial-failure case).
    fn download_dir_entry(&mut self, name: &str, cx: &mut Context<Self>) {
        let remote = remote_join(&self.path, name);
        let display_name = name.to_string();
        let local = self.download_dir.join(name);
        let session = self.session.clone();

        let placeholder_ix = self.transfers.len();
        self.transfers.push(Transfer {
            id: 0,
            name: display_name,
            direction: TransferDirection::Download,
            total: 0,
            transferred: 0,
            status: TransferStatus::Queued,
            started_at: Instant::now(),
            local_path: local.clone(),
        });
        cx.notify();

        let _ = std::fs::create_dir_all(&self.download_dir);

        cx.spawn(async move |this, cx| {
            let hrx = session.sftp_download_dir(remote, local.clone());
            let handle = match hrx.recv_async().await {
                Ok(h) => h,
                Err(_) => {
                    this.update(cx, |this, cx| {
                        if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                            t.status = TransferStatus::Failed("session closed".into());
                        }
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let TransferHandle { id, events } = handle;
            let patched = this
                .update(cx, |this, cx| {
                    if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                        t.id = id;
                        t.status = TransferStatus::Active;
                    }
                    cx.notify();
                })
                .is_ok();
            if patched {
                Self::pump_events(this, id, events, cx).await;
            }
        })
        .detach();
    }
```

- [ ] **Step 6: Add the `upload_dir` method**

In `src/panels/sftp.rs`, find the end of `upload` (immediately before `fn new_file`):

```rust
            Self::pump_events(this, id, events, cx).await;
        })
        .detach();
    }

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
```

Replace with:

```rust
            Self::pump_events(this, id, events, cx).await;
        })
        .detach();
    }

    /// Mirrors `upload`, but lets the user pick a local folder (via a
    /// directories-only native picker — mixing file and folder selection in
    /// one dialog isn't reliably supported, hence the separate toolbar
    /// button/menu item) and uploads the whole subtree via
    /// `sftp_upload_dir`.
    fn upload_dir(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let session = self.session.clone();
        let path = self.path.clone();

        let placeholder_ix = self.transfers.len();
        self.transfers.push(Transfer {
            id: 0,
            name: "(selecting…)".into(),
            direction: TransferDirection::Upload,
            total: 0,
            transferred: 0,
            status: TransferStatus::Queued,
            started_at: Instant::now(),
            local_path: PathBuf::new(),
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                this.update(cx, |this, cx| {
                    if placeholder_ix < this.transfers.len() {
                        this.transfers.remove(placeholder_ix);
                    }
                    cx.notify();
                })
                .ok();
                return;
            };
            let Some(local) = paths.into_iter().next() else {
                this.update(cx, |this, cx| {
                    if placeholder_ix < this.transfers.len() {
                        this.transfers.remove(placeholder_ix);
                    }
                    cx.notify();
                })
                .ok();
                return;
            };
            let name = local
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "upload".to_string());
            let local_path = local.clone();
            let remote = remote_join(&path, &name);
            let hrx = session.sftp_upload_dir(local, remote);
            let handle = match hrx.recv_async().await {
                Ok(h) => h,
                Err(_) => {
                    this.update(cx, |this, cx| {
                        if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                            t.status = TransferStatus::Failed("session closed".into());
                        }
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let TransferHandle { id, events } = handle;
            this.update(cx, |this, cx| {
                if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                    t.id = id;
                    t.name = name.clone();
                    t.status = TransferStatus::Active;
                    t.local_path = local_path.clone();
                }
                cx.notify();
            })
            .ok();
            Self::pump_events(this, id, events, cx).await;
        })
        .detach();
    }

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
```

- [ ] **Step 7: Add the toolbar "upload folder" button**

In `src/panels/sftp.rs`, find:

```rust
            .child(
                Button::new("sftp-upload")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Upload))
                    .tooltip(rust_i18n::t!("Sftp.upload_tooltip"))
                    .on_click(cx.listener(|this, _, _w, cx| this.upload(cx))),
            )
            .child(
                Button::new("sftp-download")
```

Replace with:

```rust
            .child(
                Button::new("sftp-upload")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Upload))
                    .tooltip(rust_i18n::t!("Sftp.upload_tooltip"))
                    .on_click(cx.listener(|this, _, _w, cx| this.upload(cx))),
            )
            .child(
                Button::new("sftp-upload-dir")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Upload))
                    .tooltip(rust_i18n::t!("Sftp.upload_folder_tooltip"))
                    .on_click(cx.listener(|this, _, _w, cx| this.upload_dir(cx))),
            )
            .child(
                Button::new("sftp-download")
```

(Reuses the same upload icon as the existing button — there's no distinct "folder upload" icon in either `gpui_component`'s bundled set or this project's custom SVGs, and adding a new icon asset is out of scope here. The two buttons sit side by side with distinct tooltips, same as `sftp-upload`/`sftp-download` today.)

- [ ] **Step 8: Update `pump_events` for `DoneWithFailures`**

In `src/panels/sftp.rs`, find:

```rust
        while let Ok(event) = events.recv_async().await {
            let done = matches!(
                event,
                TransferEvent::Done { .. }
                    | TransferEvent::Failed { .. }
                    | TransferEvent::Cancelled { .. }
            );
            let updated = this
                .update(cx, |this, cx| {
                    let Some(t) = this.transfers.iter_mut().find(|t| t.id == id) else {
                        return false;
                    };
                    match event {
                        TransferEvent::Started { total } => {
                            t.total = total;
                            t.status = TransferStatus::Active;
                        }
                        TransferEvent::Progress { transferred } => {
                            t.transferred = transferred;
                        }
                        TransferEvent::Done { transferred } => {
                            t.transferred = transferred;
                            t.status = TransferStatus::Done;
                        }
                        TransferEvent::Failed { error } => {
                            t.status = TransferStatus::Failed(error);
                        }
                        TransferEvent::Cancelled { transferred } => {
                            t.transferred = transferred;
                            t.status = TransferStatus::Cancelled;
                        }
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
```

Replace with:

```rust
        while let Ok(event) = events.recv_async().await {
            let done = matches!(
                event,
                TransferEvent::Done { .. }
                    | TransferEvent::Failed { .. }
                    | TransferEvent::Cancelled { .. }
                    | TransferEvent::DoneWithFailures { .. }
            );
            let updated = this
                .update(cx, |this, cx| {
                    let Some(t) = this.transfers.iter_mut().find(|t| t.id == id) else {
                        return false;
                    };
                    match event {
                        TransferEvent::Started { total } => {
                            t.total = total;
                            t.status = TransferStatus::Active;
                        }
                        TransferEvent::Progress { transferred } => {
                            t.transferred = transferred;
                        }
                        TransferEvent::Done { transferred } => {
                            t.transferred = transferred;
                            t.status = TransferStatus::Done;
                        }
                        TransferEvent::DoneWithFailures { transferred, failed_paths } => {
                            t.transferred = transferred;
                            t.status = TransferStatus::DoneWithFailures(failed_paths);
                        }
                        TransferEvent::Failed { error } => {
                            t.status = TransferStatus::Failed(error);
                        }
                        TransferEvent::Cancelled { transferred } => {
                            t.transferred = transferred;
                            t.status = TransferStatus::Cancelled;
                        }
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
```

- [ ] **Step 9: Update `render_transfer_body`'s status text, bar color, and completed-menu condition**

In `src/panels/sftp.rs`, find:

```rust
                let status_text = match &t.status {
                    TransferStatus::Queued => rust_i18n::t!("Sftp.transfer_queued").to_string(),
                    TransferStatus::Active => format!(
                        "{} / {}",
                        human_size(t.transferred),
                        human_size(t.total)
                    ),
                    TransferStatus::Done => {
                        rust_i18n::t!("Sftp.transfer_done", size = human_size(t.transferred)).to_string()
                    }
                    TransferStatus::Failed(e) => rust_i18n::t!("Sftp.transfer_failed", error = e).to_string(),
                    TransferStatus::Cancelled => rust_i18n::t!("Sftp.transfer_cancelled").to_string(),
                };
                let progress = t.progress();
                let bar_color = match t.status {
                    TransferStatus::Failed(_) => cx.theme().danger,
                    TransferStatus::Done => cx.theme().success,
                    TransferStatus::Cancelled => cx.theme().muted_foreground,
                    _ => cx.theme().primary,
                };
                let is_running = matches!(
                    t.status,
                    TransferStatus::Queued | TransferStatus::Active
                );
                let is_done = matches!(t.status, TransferStatus::Done);
```

Replace with:

```rust
                let status_text = match &t.status {
                    TransferStatus::Queued => rust_i18n::t!("Sftp.transfer_queued").to_string(),
                    TransferStatus::Active => format!(
                        "{} / {}",
                        human_size(t.transferred),
                        human_size(t.total)
                    ),
                    TransferStatus::Done => {
                        rust_i18n::t!("Sftp.transfer_done", size = human_size(t.transferred)).to_string()
                    }
                    TransferStatus::DoneWithFailures(failed) => rust_i18n::t!(
                        "Sftp.transfer_done_with_failures",
                        size = human_size(t.transferred),
                        count = failed.len()
                    )
                    .to_string(),
                    TransferStatus::Failed(e) => rust_i18n::t!("Sftp.transfer_failed", error = e).to_string(),
                    TransferStatus::Cancelled => rust_i18n::t!("Sftp.transfer_cancelled").to_string(),
                };
                let progress = t.progress();
                let bar_color = match t.status {
                    TransferStatus::Failed(_) => cx.theme().danger,
                    TransferStatus::Done => cx.theme().success,
                    TransferStatus::DoneWithFailures(_) => cx.theme().warning,
                    TransferStatus::Cancelled => cx.theme().muted_foreground,
                    _ => cx.theme().primary,
                };
                let is_running = matches!(
                    t.status,
                    TransferStatus::Queued | TransferStatus::Active
                );
                let is_done = matches!(
                    t.status,
                    TransferStatus::Done | TransferStatus::DoneWithFailures(_)
                );
```

(`is_done` gates the completed-transfer context menu — open file / open folder / properties / delete — a few dozen lines below this block; widening it means a partially-failed folder transfer still gets that menu, so the user can inspect what did land locally.)

- [ ] **Step 10: Fix `delete_transfer_file` for directory transfers**

In `src/panels/sftp.rs`, find:

```rust
    fn delete_transfer_file(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.transfers.iter().find(|t| t.id == id) else {
            return;
        };
        let local_path = t.local_path.clone();
        let name = t.name.clone();
        let weak_panel = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let weak_panel = weak_panel.clone();
            let local_path = local_path.clone();
            alert
                .title(rust_i18n::t!("Sftp.confirm_delete_title"))
                .description(rust_i18n::t!(
                    "Sftp.confirm_delete_body",
                    name = name.clone(),
                    folder_note = String::new()
                ))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    let _ = std::fs::remove_file(&local_path);
                    let _ = weak_panel.update(cx, |this, cx| {
                        this.transfers.retain(|t| t.id != id);
                        cx.notify();
                    });
                    true
                })
        });
    }
```

Replace with:

```rust
    fn delete_transfer_file(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.transfers.iter().find(|t| t.id == id) else {
            return;
        };
        let local_path = t.local_path.clone();
        let name = t.name.clone();
        let is_dir = local_path.is_dir();
        let weak_panel = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let weak_panel = weak_panel.clone();
            let local_path = local_path.clone();
            alert
                .title(rust_i18n::t!("Sftp.confirm_delete_title"))
                .description(rust_i18n::t!(
                    "Sftp.confirm_delete_body",
                    name = name.clone(),
                    folder_note = if is_dir {
                        rust_i18n::t!("Sftp.confirm_delete_folder_note").to_string()
                    } else {
                        String::new()
                    }
                ))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    if is_dir {
                        let _ = std::fs::remove_dir_all(&local_path);
                    } else {
                        let _ = std::fs::remove_file(&local_path);
                    }
                    let _ = weak_panel.update(cx, |this, cx| {
                        this.transfers.retain(|t| t.id != id);
                        cx.notify();
                    });
                    true
                })
        });
    }
```

(Before this fix, deleting a completed *folder* transfer's local copy silently no-op'd — `remove_file` fails on a directory and the error was discarded. `confirm_delete_folder_note` is an existing locale key already used by the remote-delete flow; no new key needed here.)

- [ ] **Step 11: Update `locales/app.yml`**

In `locales/app.yml`, find:

```yaml
  upload_tooltip:
    zh-CN: "上传"
    en: "Upload"
```

Add immediately after it:

```yaml
  upload_folder_tooltip:
    zh-CN: "上传文件夹"
    en: "Upload Folder"
```

Then find:

```yaml
  transfer_done:
    zh-CN: "完成 %{size}"
    en: "Done %{size}"
```

Add immediately after it:

```yaml
  transfer_done_with_failures:
    zh-CN: "完成 %{size}，%{count} 个失败"
    en: "Done %{size}, %{count} failed"
```

Then find and delete the now-unused key:

```yaml
  folder_download_unsupported:
    zh-CN: "暂不支持下载文件夹"
    en: "Downloading folders isn't supported yet"
```

- [ ] **Step 12: Run all sftp.rs tests**

Run: `cargo test --lib panels::sftp`
Expected: PASS, including the two new `transfer_progress_tests` from Step 1 and the pre-existing `name_column_width_tests`.

- [ ] **Step 13: Build**

Run: `cargo build`
Expected: succeeds with no errors.

- [ ] **Step 14: Manual verification (ask the user)**

Ask the user to:
- Upload a folder (toolbar's new folder-upload button) containing a few files and at least one subdirectory, and confirm the transfer list shows one row with aggregate progress that reaches "完成" (Done) at the end.
- Download a folder (select a directory row, click the toolbar download button) and confirm the same.
- If feasible, test a folder containing one file with permissions that make it unreadable/unwritable, and confirm the transfer still completes with a "完成 ... N 个失败" status rather than aborting the whole job.

- [ ] **Step 15: Commit**

```bash
git add src/panels/sftp.rs locales/app.yml
git commit -m "feat: support uploading/downloading whole folders in the SFTP browser

Wires the backend's sftp_download_dir/sftp_upload_dir into the panel: a
new toolbar 'upload folder' button, folder-aware download_selected, and
a DoneWithFailures transfer status (amber progress bar) for jobs where
some files inside the folder failed but the rest completed. Also fixes
delete_transfer_file silently no-op'ing on a completed folder transfer
(remove_file doesn't work on directories)."
```

---

## Task 4: SFTP frontend — blank-area right-click context menu

**Files:**
- Modify: `src/panels/sftp.rs` (`render` method's `top_pane` construction, around line 1233-1259; new `render_blank_area` method)
- Modify: `locales/app.yml` (no new keys — this task reuses Task 3's `upload_folder_tooltip` plus existing keys)

**Interfaces:**
- Consumes: `SftpPanel::new_file`, `new_folder`, `refresh`, `upload`, `upload_dir` (from Task 3), `copy_path` — all existing/added methods, called via `WeakEntity<SftpPanel>::update`.
- Produces: nothing consumed by later tasks.

**Design note:** `gpui_component`'s `DataTable` already wraps its *entire* table (header + all rows) in exactly one `ContextMenuExt::context_menu()` call (confirmed by reading `crates/ui/src/table/state.rs` in the vendored checkout), whose builder checks `right_clicked_row` and returns an empty menu when no row was clicked — but that check isn't reliable for detecting a genuine blank-space click (`right_clicked_row` isn't reset when a row's own click target changes to blank space; it can go stale). Stacking a *second* `.context_menu()` directly on top of the table would also risk firing simultaneously with a row's own menu, since `ContextMenuExt`'s underlying right-click handler never calls `stop_propagation()`. This project's own `sessions.rs` already solved the identical problem (see its `render_blank_area`, with the same rationale documented in its doc comment) by adding a **separate, non-overlapping trailing sibling** below the list, with its own `.context_menu()` — never stacked on the list itself. This task applies the same pattern one level up: a thin, always-present strip between the file list and the status row, structurally outside `DataTable`, so its hitbox never overlaps a row's.

- [ ] **Step 1: Add the `render_blank_area` method**

In `src/panels/sftp.rs`, find the end of `render_status_row` (immediately before `render_transfer_header`):

```rust
    fn render_status_row(&self, cx: &Context<Self>) -> impl IntoElement {
```

Locate that whole function and insert a new method directly after its closing `}` (i.e. between `render_status_row` and `render_transfer_header`):

```rust

    /// A thin, always-present strip below the file list — right-clicking it
    /// shows root-level actions (new file/folder, refresh, upload). A
    /// separate sibling rather than a context menu on the file list itself
    /// — see this task's header note for why stacking on `DataTable` isn't
    /// safe.
    fn render_blank_area(&self, cx: &Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        div()
            .id("sftp-blank-area")
            .h(px(28.0))
            .flex_shrink_0()
            .context_menu(move |menu, _window, _cx| {
                let new_file = weak.clone();
                let new_folder = weak.clone();
                let refresh = weak.clone();
                let upload = weak.clone();
                let upload_dir = weak.clone();
                let copy_path = weak.clone();
                menu.item(PopupMenuItem::new(rust_i18n::t!("Sftp.new_file_tooltip")).on_click(
                    move |_ev, window, cx| {
                        let _ = new_file.update(cx, |panel, cx| panel.new_file(window, cx));
                    },
                ))
                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.new_folder_tooltip")).on_click(
                    move |_ev, window, cx| {
                        let _ = new_folder.update(cx, |panel, cx| panel.new_folder(window, cx));
                    },
                ))
                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.refresh_tooltip")).on_click(
                    move |_ev, _window, cx| {
                        let _ = refresh.update(cx, |panel, cx| panel.refresh(cx));
                    },
                ))
                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.upload_tooltip")).on_click(
                    move |_ev, _window, cx| {
                        let _ = upload.update(cx, |panel, cx| panel.upload(cx));
                    },
                ))
                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.upload_folder_tooltip")).on_click(
                    move |_ev, _window, cx| {
                        let _ = upload_dir.update(cx, |panel, cx| panel.upload_dir(cx));
                    },
                ))
                .item(PopupMenuItem::new(rust_i18n::t!("Sftp.copy_path")).on_click(
                    move |_ev, _window, cx| {
                        let _ = copy_path.update(cx, |panel, cx| panel.copy_path(cx));
                    },
                ))
            })
    }
```

- [ ] **Step 2: Insert it into the layout**

In `src/panels/sftp.rs`, find, inside `render`:

```rust
        let title_bar = self.render_title_bar(cx);
        let toolbar = self.render_toolbar(window, cx);
        let pending_op = self.render_pending_op_row(cx);
        let path_bar = self.render_path_bar(&path_input, cx);
        let file_list = self.render_file_list(cx);
        let status_row = self.render_status_row(cx);
```

Replace with:

```rust
        let title_bar = self.render_title_bar(cx);
        let toolbar = self.render_toolbar(window, cx);
        let pending_op = self.render_pending_op_row(cx);
        let path_bar = self.render_path_bar(&path_input, cx);
        let file_list = self.render_file_list(cx);
        let blank_area = self.render_blank_area(cx);
        let status_row = self.render_status_row(cx);
```

Then find:

```rust
            .child(title_bar)
            .child(toolbar)
            .child(pending_op)
            .child(path_bar)
            .child(file_list)
            .child(status_row);
```

Replace with:

```rust
            .child(title_bar)
            .child(toolbar)
            .child(pending_op)
            .child(path_bar)
            .child(file_list)
            .child(blank_area)
            .child(status_row);
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds with no errors.

- [ ] **Step 4: Manual verification (ask the user)**

Ask the user to right-click the thin strip directly below the file list (above the item-count status bar) and confirm the menu shows 新建文件/新建文件夹/刷新/上传文件/上传文件夹/复制当前路径, and that each item works. Also ask them to right-click directly on a file/folder row and confirm only the row's own menu appears (not both menus at once) — this is the risk this task's design note called out.

- [ ] **Step 5: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: add right-click menu to the SFTP browser's blank list area

A thin strip below the file list, structurally separate from DataTable's
own row list (not a second context_menu stacked on top of it, which
would double-fire alongside a row's own menu). Offers new file/folder,
refresh, upload file/folder, and copy path."
```

---

## Task 5: SFTP frontend — "clear completed transfers" icon

**Files:**
- Modify: `src/panels/sftp.rs` (`render_transfer_header`; new `clear_completed_transfers` method)
- Modify: `locales/app.yml` (new key)

**Interfaces:**
- Consumes: `TransferStatus` (from Task 3, including `DoneWithFailures`).
- Produces: nothing consumed by later tasks (last task in this plan).

- [ ] **Step 1: Write a failing test for the clear logic**

In `src/panels/sftp.rs`, add to the `transfer_progress_tests` module written in Task 3:

```rust
    #[test]
    fn clear_completed_transfers_keeps_only_in_flight_rows() {
        let panel_transfers = vec![
            transfer_with(TransferStatus::Queued, 100, 0),
            transfer_with(TransferStatus::Active, 100, 50),
            transfer_with(TransferStatus::Done, 100, 100),
            transfer_with(TransferStatus::DoneWithFailures(vec!["a".into()]), 100, 90),
            transfer_with(TransferStatus::Failed("boom".into()), 100, 0),
            transfer_with(TransferStatus::Cancelled, 100, 10),
        ];
        let mut transfers = panel_transfers;
        transfers.retain(|t| matches!(t.status, TransferStatus::Queued | TransferStatus::Active));
        assert_eq!(transfers.len(), 2);
    }
```

(This test exercises the exact `retain` predicate `clear_completed_transfers` will use, directly on a `Vec<Transfer>` — `SftpPanel` itself needs a live `Context`/window to construct, so the method's *logic* is tested this way rather than through the method itself, consistent with how `Transfer::progress()` is tested above.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib panels::sftp::transfer_progress_tests::clear_completed_transfers_keeps_only_in_flight_rows`
Expected: FAIL to compile — `transfer_with` isn't visible yet if Task 3 wasn't done first (it is, per this plan's ordering), or the test simply hasn't been added to a build yet. If Task 3 is already complete, this should actually compile and pass immediately since it doesn't call any not-yet-written method — in that case treat this step as a no-op confirmation and proceed directly to Step 3 (this test is here primarily to pin the `retain` predicate before wiring the real method to it).

- [ ] **Step 3: Add the `clear_completed_transfers` method**

In `src/panels/sftp.rs`, find the end of `cancel_transfer` (immediately before `open_transfer_file`):

```rust
    fn cancel_transfer(&self, id: u64) {
        self.session.sftp_cancel(id);
    }
```

Insert immediately after it:

```rust

    /// Drops every transfer whose status is terminal (`Done`,
    /// `DoneWithFailures`, `Failed`, or `Cancelled`) — success, partial
    /// failure, hard failure, and user-cancelled all count as "completed"
    /// for clearing purposes. In-flight transfers (`Queued`/`Active`) are
    /// untouched.
    fn clear_completed_transfers(&mut self, cx: &mut Context<Self>) {
        self.transfers
            .retain(|t| matches!(t.status, TransferStatus::Queued | TransferStatus::Active));
        cx.notify();
    }
```

- [ ] **Step 4: Add the icon button to `render_transfer_header`**

In `src/panels/sftp.rs`, find:

```rust
    fn render_transfer_header(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_color(cx.theme().foreground)
            .child(rust_i18n::t!("Sftp.file_transfers_header"))
    }
```

Replace with:

```rust
    fn render_transfer_header(&self, cx: &Context<Self>) -> impl IntoElement {
        let has_completed = self.transfers.iter().any(|t| {
            matches!(
                t.status,
                TransferStatus::Done
                    | TransferStatus::DoneWithFailures(_)
                    | TransferStatus::Failed(_)
                    | TransferStatus::Cancelled
            )
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_color(cx.theme().foreground)
            .child(div().flex_1().child(rust_i18n::t!("Sftp.file_transfers_header")))
            .child(
                Button::new("sftp-clear-completed-transfers")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Delete)
                    .tooltip(rust_i18n::t!("Sftp.clear_completed_transfers_tooltip"))
                    .disabled(!has_completed)
                    .on_click(cx.listener(|this, _, _w, cx| this.clear_completed_transfers(cx))),
            )
    }
```

- [ ] **Step 5: Add the locale key**

In `locales/app.yml`, find:

```yaml
  file_transfers_header:
    zh-CN: "文件传输"
    en: "File Transfers"
```

Add immediately after it:

```yaml
  clear_completed_transfers_tooltip:
    zh-CN: "清理已完成传输"
    en: "Clear Completed Transfers"
```

- [ ] **Step 6: Run all sftp.rs tests**

Run: `cargo test --lib panels::sftp`
Expected: PASS, including the new `clear_completed_transfers_keeps_only_in_flight_rows` test.

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: succeeds with no errors.

- [ ] **Step 8: Manual verification (ask the user)**

Ask the user to run a few transfers to completion (including at least one failure, e.g. downloading a nonexistent file), confirm the new icon at the top-right of the "文件传输" header is enabled once any transfer is done/failed/cancelled, and clicking it removes those rows while leaving any in-progress transfer alone. Also confirm the icon is disabled (dimmed, unclickable) when the transfer list is empty or has only in-flight transfers.

- [ ] **Step 9: Commit**

```bash
git add src/panels/sftp.rs locales/app.yml
git commit -m "feat: add a 'clear completed transfers' icon to the transfers header

Drops Done/DoneWithFailures/Failed/Cancelled rows in one click, leaving
in-flight transfers untouched. Disabled when there's nothing to clear."
```
