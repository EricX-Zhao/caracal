//! `SftpPanel`: remote file browser over the SFTP subsystem of an existing
//! [`SshSession`] (the *same* connection as the terminal — CLAUDE.md §2: one
//! Session, one connection). Renders a 7-section layout matching the
//! reference screenshot:
//!
//! 1. Toolbar — icon buttons (open / upload / download / delete / up /
//!    refresh) + search input slot + fullscreen toggle.
//! 2. Path bar — current remote path + copy-path button.
//! 3. Column header — 名称 / 修改时间 / 大小 / 权.
//! 4. File list — scrollable 4-column table with 📁/📄 icon per row.
//! 5. Status row — total item count + total bytes.
//! 6. Transfers section — header + live list of background downloads/uploads
//!    with progress bars (or empty-state "无传输记录").
//! 7. Bottom path bar — full path echo.
//!
//! Background transfers (CLAUDE.md §2 + the SshSession refactor): every
//! download/upload runs as a tokio task spawned by the session thread, so the
//! SSH shell channel stays responsive during transfers. The panel listens to
//! `TransferEvent`s via `cx.spawn`-ed pump tasks that update the matching
//! `Transfer` row and call `cx.notify()` to trigger a repaint.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, AppContext, AsyncApp, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;

use crate::terminal::ssh::{
    SftpEntry, SshSession, TransferDirection, TransferEvent, TransferHandle,
};

/// Status of a single transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TransferStatus {
    Queued,
    Active,
    Done,
    Failed(String),
}

/// One row in the transfer list (the bottom section of the panel).
struct Transfer {
    id: u64,
    name: String,
    direction: TransferDirection,
    total: u64,
    transferred: u64,
    status: TransferStatus,
    #[allow(dead_code)]
    started_at: Instant,
}

impl Transfer {
    /// 0.0..=1.0 — safe when `total == 0` (treats as fully done if status
    /// says so).
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
}

pub struct SftpPanel {
    session: Arc<SshSession>,
    label: SharedString,
    focus_handle: FocusHandle,
    /// Current remote directory (starts at the login dir, ".").
    path: String,
    entries: Vec<SftpEntry>,
    status: String,
    /// Live + completed transfers shown in the bottom section.
    transfers: Vec<Transfer>,
    /// Editable path-bar input. Built lazily on the first render because
    /// `InputState::new` needs a `&mut Window`, and `Entity::new`'s
    /// closure only gives us `&mut Context`. Render always has `Window`.
    path_input: Option<Entity<InputState>>,
    /// Subscription for `InputEvent::PressEnter` on `path_input`. Kept
    /// alive for the panel's lifetime; the input is created once on first
    /// render and never replaced, so the subscription never needs to be
    /// re-issued.
    _path_sub: Option<Subscription>,
    /// Index into `entries` of the currently selected row (single-select).
    /// Driven by single-click; double-click fires the row action and keeps
    /// the selection.
    selected: Option<usize>,
}

impl SftpPanel {
    pub fn new(
        session: Arc<SshSession>,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            session,
            label: label.into(),
            focus_handle: cx.focus_handle(),
            path: ".".to_string(),
            entries: Vec::new(),
            status: "Loading…".to_string(),
            transfers: Vec::new(),
            path_input: None,
            _path_sub: None,
            selected: None,
        };
        this.refresh(cx);
        this
    }

    /// Lazy initializer for the path-bar `InputState`. Called from
    /// `Render::render` (which has `&mut Window`). Returns a clone of the
    /// `Entity` so callers can pass it straight into `Input::new(...)`.
    fn ensure_path_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(e) = &self.path_input {
            return e.clone();
        }
        let initial = self.path.clone();
        let entity = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("~")
                .submit_on_enter(true)
        });
        entity.update(cx, |s, cx| {
            s.set_value(initial, window, cx);
        });
        // Subscribe to Enter on the new input. `subscribe_in` needs a
        // `Window` (the input internally reads window focus state), so we
        // use it here at create time.
        let sub = cx.subscribe_in(&entity, window, |this, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_path(window, cx);
            }
        });
        self._path_sub = Some(sub);
        self.path_input = Some(entity.clone());
        entity
    }

    /// Read the path-bar input, normalize `~` → `.`, and re-list that
    /// directory. Called on Enter in the path input. The input is
    /// guaranteed to exist by the time we get the Enter event (it can
    /// only be sent after the first render, which created the entity).
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
        self.selected = None;
        self.path = new;
        self.status = "Loading…".to_string();
        // Sync the input back (covers the case where the user typed `~`
        // and we mapped it to `.`). Skip the write if the value already
        // matches — avoids moving the caret on every Enter.
        let synced = self.path.clone();
        if input.read(cx).value().as_ref() != synced.as_str() {
            input.update(cx, |s, cx| {
                s.set_value(synced, window, cx);
            });
        }
        cx.notify();
        self.refresh(cx);
    }

    /// Re-list the current directory.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let rx = self.session.sftp_read_dir(self.path.clone());
        cx.spawn(async move |this, cx| {
            let result = rx.recv_async().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(entries)) => {
                        this.status = format!("{} item(s)", entries.len());
                        this.entries = entries;
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

    fn enter_dir(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.path = remote_join(&self.path, name);
        self.status = "Loading…".to_string();
        self.selected = None;
        self.sync_path_input(window, cx);
        self.refresh(cx);
        cx.notify();
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.path = remote_parent(&self.path);
        self.status = "Loading…".to_string();
        self.selected = None;
        self.sync_path_input(window, cx);
        self.refresh(cx);
        cx.notify();
    }

    /// Push the current `self.path` into the path-bar input if they
    /// differ. Called whenever navigation changes `self.path` outside
    /// of the input itself (so the editable field stays in sync with
    /// the canonical state). Skipped when the values already match so
    /// we don't move the caret mid-edit. The early-return for None
    /// handles the brief window before the first render creates the
    /// entity.
    fn sync_path_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.path_input.as_ref() else {
            return;
        };
        let v = self.path.clone();
        let current = input.read(cx).value().to_string();
        if current != v {
            input.update(cx, |s, cx| {
                s.set_value(v, window, cx);
            });
        }
    }

    /// Download `name` (a file in `self.path`) to a user-chosen local path.
    /// Inserts a `Queued` transfer row immediately and wires up a pump task
    /// that listens to the new background-transfer events.
    fn download(&mut self, name: &str, cx: &mut Context<Self>) {
        let remote = remote_join(&self.path, name);
        let display_name = name.to_string();
        let dir = home_dir();
        let rx = cx.prompt_for_new_path(&dir, Some(name));
        let session = self.session.clone();

        // Insert a placeholder row so the UI shows the transfer immediately.
        // `id = 0` is a sentinel meaning "the real id hasn't arrived yet" —
        // it gets patched in once the handle is received.
        let placeholder_ix = self.transfers.len();
        self.transfers.push(Transfer {
            id: 0,
            name: display_name.clone(),
            direction: TransferDirection::Download,
            total: 0,
            transferred: 0,
            status: TransferStatus::Queued,
            started_at: Instant::now(),
        });
        log::info!(
            "sftp.download: pushed placeholder ix={placeholder_ix} for {display_name:?}, \
             waiting on save dialog"
        );
        cx.notify();

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(local))) = rx.await else {
                // User cancelled the save dialog.
                this.update(cx, |this, cx| {
                    if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                        t.status = TransferStatus::Failed("cancelled".into());
                    }
                    cx.notify();
                })
                .ok();
                return;
            };
            let hrx = session.sftp_download(remote, local.clone());
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
            // Patch in the real id and update display name (now includes the
            // chosen local filename).
            let final_name = local
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or(display_name);
            let patched = this
                .update(cx, |this, cx| {
                    if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                        t.id = id;
                        t.name = final_name.clone();
                        t.status = TransferStatus::Active;
                        log::info!(
                            "sftp.download: patched placeholder ix={placeholder_ix} -> id={id} \
                             name={final_name:?}, starting pump"
                        );
                    } else {
                        // Out-of-bounds placeholder_ix would mean transfers
                        // got reshuffled between the push and the patch —
                        // shouldn't happen since we only `push`. Log so
                        // we notice if it does.
                        log::warn!(
                            "sftp.download: placeholder ix={placeholder_ix} out of bounds \
                             (transfers len={}); events will not update UI",
                            this.transfers.len()
                        );
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

    /// Toolbar Download button handler. Downloads `self.entries[self.selected]`
    /// if the user has a selection. Folders are skipped for v1 (recursive
    /// folder download isn't supported yet — would require walking the
    /// remote tree via SFTP). The row-click double-click flow handles
    /// direct file downloads and stays as-is; this is the toolbar
    /// equivalent for keyboard-style "select then act" workflows.
    fn download_selected(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.selected else {
            // No selection — surface a hint via `status` so the user
            // knows the click registered but had no target.
            self.status = "请先选中一个文件再点下载".to_string();
            cx.notify();
            return;
        };
        let Some(entry) = self.entries.get(ix) else {
            // Selection is stale (e.g. entries reloaded after the user
            // clicked something). Clear it and hint.
            self.selected = None;
            self.status = "选中的条目已不在列表里,请重新选择".to_string();
            cx.notify();
            return;
        };
        if entry.is_dir {
            // Folder download would need recursive SFTP walk — out of
            // scope for v1. Drop a hint in `status` instead of
            // silently doing nothing.
            self.status = "暂不支持下载文件夹".to_string();
            cx.notify();
            return;
        }
        // Re-use the existing `download` flow (same placeholder + pump
        // wiring). `download` reads `self.selected`-agnostic state, so
        // passing the name is all that's needed.
        let name = entry.name.clone();
        self.download(&name, cx);
    }

    /// Upload the user-chosen local file to `self.path` on the remote.
    /// Mirror of `download` but with reversed direction.
    fn upload(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
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
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                this.update(cx, |this, cx| {
                    if let Some(t) = this.transfers.get_mut(placeholder_ix) {
                        t.status = TransferStatus::Failed("cancelled".into());
                    }
                    cx.notify();
                })
                .ok();
                return;
            };
            let Some(local) = paths.into_iter().next() else {
                return;
            };
            let name = local
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "upload.bin".to_string());
            let remote = remote_join(&path, &name);
            let hrx = session.sftp_upload(local, remote);
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
                }
                cx.notify();
            })
            .ok();
            Self::pump_events(this, id, events, cx).await;
        })
        .detach();
    }

    /// Background pump: drains `TransferEvent`s from the handle's channel
    /// and updates the matching `Transfer` row. Runs until the channel
    /// closes (which the session thread does after the final Done/Failed).
    async fn pump_events(
        this: gpui::WeakEntity<Self>,
        id: u64,
        events: flume::Receiver<TransferEvent>,
        cx: &mut AsyncApp,
    ) {
        while let Ok(event) = events.recv_async().await {
            let done = matches!(
                event,
                TransferEvent::Done { .. } | TransferEvent::Failed { .. }
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
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !updated || done {
                break;
            }
        }
    }

    /// Copy the current remote path to the system clipboard. Wired to the
    /// small button in the path bar.
    fn copy_path(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.path.clone()));
    }
}

impl Focusable for SftpPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<PanelEvent> for SftpPanel {}

impl Panel for SftpPanel {
    fn panel_name(&self) -> &'static str {
        "SftpPanel"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(format!("SFTP: {}", self.label))
    }
}

// --- placeholder for non-SSH focused terminals -----------------------------

/// Shown in the left "SFTP" dock when the focused terminal has no SFTP
/// browser to show — no terminal focused yet, or the focused one isn't SSH
/// (local shell / serial). `Workspace` swaps this in and out as focus moves
/// between terminal tabs, so the dock always holds exactly one tab.
pub struct SftpPlaceholder {
    focus_handle: FocusHandle,
}

impl SftpPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for SftpPlaceholder {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<PanelEvent> for SftpPlaceholder {}

impl Panel for SftpPlaceholder {
    fn panel_name(&self) -> &'static str {
        "SftpPlaceholder"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("SFTP")
    }
}

impl Render for SftpPlaceholder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("当前终端不是 SSH 会话 — 切换到一个 SSH 终端标签,或在「已保存的连接」中点击文件夹图标")
    }
}

// --- main render ----------------------------------------------------------

impl Render for SftpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Lazy-init the path-bar input (needs `Window`; constructor
        // only has `Context`). No-op after the first render.
        let path_input = self.ensure_path_input(window, cx);
        let toolbar = self.render_toolbar(cx);
        let path_bar = self.render_path_bar(&path_input, cx);
        let column_header = self.render_column_header(cx);
        let file_list = self.render_file_list(cx);
        let status_row = self.render_status_row(cx);
        let transfers = self.render_transfers_section(cx);
        let bottom = self.render_bottom(cx);

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .text_sm()
            .child(toolbar)
            .child(path_bar)
            .child(column_header)
            .child(file_list)
            .child(status_row)
            .child(transfers)
            .child(bottom)
    }
}

// --- render sub-sections --------------------------------------------------

impl SftpPanel {
    fn render_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        // Toolbar pattern: each icon is a small clickable div. Behavior
        // wiring:
        //   Open / Delete        — visual only for v1 (no multi-select
        //     state wired — delete would need selection + remote rm).
        //   Upload              — opens a system file picker.
        //   Download            — downloads `self.entries[self.selected]`.
        //     Single-click a file first to populate the selection;
        //     double-click works as before too (the row handler also
        //     triggers download on `click_count >= 2`).
        //   Up                  — `go_up` (parent directory).
        //   Refresh             — `refresh` (re-list current dir).
        //   Search / Fullscreen — visual only for v1.
        // The Save / floppy button from the reference is dropped: there is
        // no "save remote listing to disk" action we expose, and the
        // asset bundle has no save/floppy SVG to match the reference icon.
        //
        // We inline each button rather than extract a helper because Rust
        // closures don't allow `impl Trait` in parameters; the helper
        // would have to take a `Box<dyn Fn(...)>`, which adds an
        // allocation per toolbar render.

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_1()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .id("sftp-open")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::Folder)),
            )
            .child(
                div()
                    .id("sftp-upload")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    // Custom SVG (lucide `arrow-up-from-line`) — has the
                    // baseline that the basic `IconName::ArrowUp` lacks.
                    // Loaded from `assets/icons/sftp-upload.svg` via our
                    // `CaracalAssets` wrapper (see `src/assets.rs`).
                    .child(Icon::empty().path("icons/sftp-upload.svg"))
                    .on_click(cx.listener(|this, _e, _w, cx| this.upload(cx))),
            )
            .child(
                div()
                    .id("sftp-download")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    // Custom SVG (lucide `arrow-down-to-line`).
                    .child(Icon::empty().path("icons/sftp-download.svg"))
                    // Download whatever the user has currently selected.
                    // No-op if nothing is selected (the user is expected
                    // to single-click a file first to populate
                    // `self.selected`).
                    .on_click(cx.listener(|this, _e, _w, cx| this.download_selected(cx))),
            )
            .child(
                div()
                    .id("sftp-delete")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::Delete)),
            )
            .child(
                div()
                    .id("sftp-up")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::ChevronUp))
                    .on_click(cx.listener(|this, _e, window, cx| this.go_up(window, cx))),
            )
            .child(
                div()
                    .id("sftp-refresh")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::Redo))
                    .on_click(cx.listener(|this, _e, _w, cx| this.refresh(cx))),
            )
            // Spacer to push the trailing icons to the right edge
            // (matches the reference layout: action icons left,
            // search/fullscreen right).
            .child(div().flex_1())
            .child(
                div()
                    .id("sftp-search")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::Search)),
            )
            .child(
                div()
                    .id("sftp-fullscreen")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::Maximize)),
            )
    }

    fn render_path_bar(
        &self,
        path_input: &Entity<InputState>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        // Single-row bar: an editable path input on the left (Enter
        // commits — wired in `ensure_path_input`), a copy-path icon
        // button on the right. The input's value is kept in sync with
        // `self.path` by `enter_dir` / `go_up` / `commit_path`.
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().child(Input::new(path_input).xsmall()))
            .child(
                div()
                    .id("sftp-copy-path")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::Copy))
                    .on_click(cx.listener(|this, _e, _w, cx| this.copy_path(cx))),
            )
    }

    fn render_column_header(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .py_0p5()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .flex_grow(2.0)
                    .flex_basis(px(0.0))
                    .min_w(px(0.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("名称 ↓"),
            )
            .child(
                div()
                    .flex_grow(1.0)
                    .flex_basis(px(0.0))
                    .min_w(px(0.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("修改时间"),
            )
            .child(
                div()
                    .flex_grow(1.0)
                    .flex_basis(px(0.0))
                    .min_w(px(0.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .text_right()
                    .child("大小"),
            )
            .child(
                div()
                    .flex_grow(1.0)
                    .flex_basis(px(0.0))
                    .min_w(px(0.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("权"),
            )
    }

    fn render_file_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let rows = self.entries.iter().enumerate().map(|(ix, entry)| {
            let name = entry.name.clone();
            let is_dir = entry.is_dir;
            let display_name = if is_dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            };
            let size_text = if is_dir {
                "—".to_string()
            } else {
                human_size(entry.size)
            };
            let mtime_text = human_mtime(entry.mtime);
            let perms_text = human_perms(entry.perms);
            let icon_label = if is_dir { "📁" } else { "📄" };

            div()
                .id(("sftp-row", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py_0p5()
                .text_color(cx.theme().foreground)
                // Selected row: persistent accent background.
                // Hover: lighter accent (also kept for discoverability).
                // Both stack — selection wins over hover — but
                // `bg()` overrides, so we just apply whichever the
                // current state calls for.
                .when(self.selected == Some(ix), |d| d.bg(cx.theme().accent))
                .hover(|s| s.bg(cx.theme().accent))
                .child(
                    // Name column: icon + name. flex_grow(2) gives it
                    // priority over the three data columns when the
                    // dock is narrow (and `min_w(0)` lets the data
                    // columns shrink first if needed).
                    div()
                        .flex_grow(2.0)
                        .flex_basis(px(0.0))
                        .min_w(px(0.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .overflow_hidden()
                        .child(div().text_xs().child(icon_label))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(display_name)),
                        ),
                )
                .child(
                    div()
                        .flex_grow(1.0)
                        .flex_basis(px(0.0))
                        .min_w(px(0.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(mtime_text)),
                )
                .child(
                    div()
                        .flex_grow(1.0)
                        .flex_basis(px(0.0))
                        .min_w(px(0.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .text_right()
                        .child(SharedString::from(size_text)),
                )
                .child(
                    div()
                        .flex_grow(1.0)
                        .flex_basis(px(0.0))
                        .min_w(px(0.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(perms_text)),
                )
                // Single-click selects the row (stored as `selected` in
                // the panel so we can paint the accent background).
                // Double-click fires the row action (enter dir for
                // directories, download for files). We use `on_mouse_down`
                // instead of `on_click` so we can branch on
                // `event.click_count` directly — `on_click` fires on
                // every click including the second of a double, which
                // would force "select + immediately act" with no way
                // to observe the selection first.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        // Update selection on the first click of any
                        // sequence; harmless on the second click of a
                        // double (we re-select the same row).
                        this.selected = Some(ix);
                        if event.click_count >= 2 {
                            if is_dir {
                                this.enter_dir(&name, window, cx);
                            } else {
                                this.download(&name, cx);
                            }
                        }
                        cx.notify();
                    }),
                )
        });

        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scrollbar()
            .children(rows)
    }

    fn render_status_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let count = self.entries.len();
        let total_bytes: u64 = self
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.size)
            .sum();
        let summary = format!("共 {count} 项 | {}", human_size(total_bytes));
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(summary)),
            )
            .child(
                // Small action icon group on the right (visual only — these
                // mirror the transfer-section icons in the reference).
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id("sftp-status-pause")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("⏸"),
                    )
                    .child(
                        div()
                            .id("sftp-status-play")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("▶"),
                    )
                    .child(
                        div()
                            .id("sftp-status-clear")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("⟳"),
                    ),
            )
    }

    fn render_transfers_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .child("文件传输"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id("sftp-tr-pause")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("⏸"),
                    )
                    .child(
                        div()
                            .id("sftp-tr-play")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("▶"),
                    )
                    .child(
                        div()
                            .id("sftp-tr-clear")
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("✕"),
                    ),
            );

        let body: gpui::AnyElement = if self.transfers.is_empty() {
            div()
                .flex()
                .items_center()
                .justify_center()
                .py_4()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("无传输记录")
                .into_any_element()
        } else {
            let rows = self.transfers.iter().map(|t| {
                let label = match t.direction {
                    TransferDirection::Download => "↓",
                    TransferDirection::Upload => "↑",
                };
                let status_text = match &t.status {
                    TransferStatus::Queued => "排队中".to_string(),
                    TransferStatus::Active => format!(
                        "{} / {}",
                        human_size(t.transferred),
                        human_size(t.total)
                    ),
                    TransferStatus::Done => format!("完成 {}", human_size(t.transferred)),
                    TransferStatus::Failed(e) => format!("失败: {e}"),
                };
                let progress = t.progress();
                let bar_color = match t.status {
                    TransferStatus::Failed(_) => cx.theme().danger,
                    TransferStatus::Done => cx.theme().success,
                    _ => cx.theme().accent,
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(t.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(status_text)),
                            ),
                    )
                    .child(
                        // Thin progress bar: full-width track + filled portion.
                        div()
                            .h(px(3.0))
                            .w_full()
                            .rounded_sm()
                            .bg(cx.theme().border)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::DefiniteLength::Fraction(progress))
                                    .rounded_sm()
                                    .bg(bar_color),
                            ),
                    )
            });
            div().flex().flex_col().children(rows).into_any_element()
        };

        // The body wraps the live transfer list (or the empty-state
        // placeholder) in a `flex_1` slot so it fills the remaining
        // 140px - header_height space. `overflow_y_scrollbar` keeps
        // multiple transfers contained inside the fixed section
        // height rather than overflowing downward.
        let body_container = div()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scrollbar()
            .child(body);

        // Fixed-height transfers section: the file list (above) is the
        // only `flex_1` slot, so this section stays at a stable 140px
        // regardless of how many transfers exist. The body uses
        // `overflow_y_scrollbar` so multiple transfers scroll inside
        // rather than pushing the section taller (which would push
        // the bottom path bar off the dock).
        div()
            .flex()
            .flex_col()
            .h(px(140.0))
            .border_t_1()
            .border_color(cx.theme().border)
            .child(header)
            .child(body_container)
    }

    fn render_bottom(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(self.path.clone()))
    }
}

// --- helpers --------------------------------------------------------------

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Join a remote directory and a child name (POSIX paths).
fn remote_join(base: &str, name: &str) -> String {
    if base == "." || base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Parent of a remote path (stays within where browsing started).
fn remote_parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}

/// Render the 9 low-order permission bits as the classic `ls -l` 9-char
/// string (`rwxr-xr-x`). Accepts either a mode that includes the file-type
/// bits (`0o100644`) or one with just the 9 perm bits (`0o644`) — we mask
/// the low 9 bits either way.
fn human_perms(perms: u32) -> String {
    let p = perms & 0o777;
    let rwx = |bits: u32| -> String {
        let mut s = String::with_capacity(3);
        s.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        s.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        s.push(if bits & 0o1 != 0 { 'x' } else { '-' });
        s
    };
    format!("{}{}{}", rwx((p >> 6) & 0o7), rwx((p >> 3) & 0o7), rwx(p & 0o7))
}

/// Format `mtime` (unix seconds) as `YYYY-MM-DD HH:MM`. Returns `—` for 0
/// (the "server didn't tell us" sentinel). Uses Howard Hinnant's
/// `days_from_civil` / `civil_from_days` so we don't pull in `chrono`/`time`.
fn human_mtime(mtime: u32) -> String {
    if mtime == 0 {
        return "—".into();
    }
    let secs = mtime as i64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;

    // civil_from_days: convert days-since-1970-01-01 (with -2_719_468
    // offset to align the civil-day epoch) to (y, m, d).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, hour, minute)
}