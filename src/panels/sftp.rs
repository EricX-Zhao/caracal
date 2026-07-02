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
    InteractiveElement, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement,
    Pixels, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::panels::icons::{AppIcon, icon};


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
    Cancelled,
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

/// Kind of pending new-fs-item creation. When set, an inline name-entry
/// row is rendered below the toolbar; Enter commits the operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingOpKind {
    NewFile,
    NewFolder,
}

pub struct SftpPanel {
    session: Arc<SshSession>,
    label: SharedString,
    focus_handle: FocusHandle,
    path: String,
    entries: Vec<SftpEntry>,
    status: String,
    transfers: Vec<Transfer>,
    path_input: Option<Entity<InputState>>,
    _path_sub: Option<Subscription>,
    selected: Option<usize>,
    transfers_height: Pixels,
    drag_start: Option<(Pixels, Pixels)>,
    file_list_scroll_handle: ScrollHandle,
    /// 4-column widths (name, mtime, size, perms). Initialised to sensible
    /// defaults; updated when the user drags a column divider.
    col_widths: [Pixels; 4],
    /// While the user drags a column divider: (index of the column to the
    /// left of the divider, the column's width when the drag started, the
    /// mouse x when the drag started).
    col_drag: Option<(usize, Pixels, Pixels)>,
    /// Inline name-entry row: shown when the user clicks 新建文件 / 新建文件夹.
    pending_op: Option<(PendingOpKind, Entity<InputState>)>,
    /// Default local download directory. Editable in the bottom bar.
    download_dir: PathBuf,
}

impl SftpPanel {
    pub fn new(
        session: Arc<SshSession>,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let download_dir = download_default_dir();
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
            transfers_height: px(200.0),
            drag_start: None,
            file_list_scroll_handle: ScrollHandle::new(),
            col_widths: [px(150.0), px(110.0), px(64.0), px(72.0)],
            col_drag: None,
            pending_op: None,
            download_dir,
        };
        this.refresh(cx);

        // Resolve "." → absolute home dir so the bottom path bar and top
        // path input show e.g. /home/user immediately. The path input is
        // lazy-inited on first render and reads `self.path` at creation, so
        // just updating `self.path` here is enough — `ensure_path_input`
        // will pick up the resolved value before it ever paints ".".
        // Falls back silently to "." if the server can't canonicalize.
        let session = this.session.clone();
        cx.spawn(async move |this, cx| {
            let rx = session.sftp_realpath(".".to_string());
            if let Ok(Ok(resolved)) = rx.recv_async().await {
                let fin = resolved.trim_end_matches('/').to_string();
                let fin = if fin.is_empty() { "/".to_string() } else { fin };
                let _ = this.update(cx, |this, cx| {
                    if fin != this.path {
                        this.path = fin;
                    }
                    // Refresh now that the path is absolute — the initial
                    // refresh (using ".") is overwritten harmlessly.
                    this.refresh(cx);
                    cx.notify();
                });
            }
        })
        .detach();

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

    /// Download `name` (a file in `self.path`) directly into the default
    /// download directory. Inserts a `Queued` transfer row immediately and
    /// wires up a pump task that listens to the new background-transfer events.
    fn download(&mut self, name: &str, cx: &mut Context<Self>) {
        let remote = remote_join(&self.path, name);
        let display_name = name.to_string();
        let local = self.download_dir.join(name);
        let session = self.session.clone();

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
        cx.notify();

        // Ensure the download dir exists (best-effort, may fail on permission).
        let _ = std::fs::create_dir_all(&self.download_dir);

        cx.spawn(async move |this, cx| {
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
    /// if the user has a selection.
    fn download_selected(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.selected else {
            self.status = "请先选中一个文件再点下载".to_string();
            cx.notify();
            return;
        };
        let Some(entry) = self.entries.get(ix) else {
            self.selected = None;
            self.status = "选中的条目已不在列表里,请重新选择".to_string();
            cx.notify();
            return;
        };
        if entry.is_dir {
            self.status = "暂不支持下载文件夹".to_string();
            cx.notify();
            return;
        }
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

    /// Show the inline name-entry row for creating a new file.
    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.new(|cx| InputState::new(window, cx).submit_on_enter(true));
        // Subscribe to Enter on the pending-op input.
        cx.subscribe_in(&entity, window, |this: &mut Self, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_pending_op(window, cx);
            }
        })
        .detach();
        self.pending_op = Some((PendingOpKind::NewFile, entity));
        cx.notify();
    }

    /// Show the inline name-entry row for creating a new folder.
    fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.new(|cx| InputState::new(window, cx).submit_on_enter(true));
        cx.subscribe_in(&entity, window, |this: &mut Self, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_pending_op(window, cx);
            }
        })
        .detach();
        self.pending_op = Some((PendingOpKind::NewFolder, entity));
        cx.notify();
    }

    /// Commit the pending new-file or new-folder name entry.
    fn commit_pending_op(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, input)) = self.pending_op.as_ref() else {
            return;
        };
        let name = input.read(cx).value().trim().to_string();
        let kind = *kind;
        self.pending_op = None;
        cx.notify();
        if name.is_empty() {
            return;
        }
        let remote = remote_join(&self.path, &name);
        let session = self.session.clone();
        match kind {
            PendingOpKind::NewFile => {
                cx.spawn(async move |this, cx| {
                    let rx = session.sftp_create_file(remote);
                    let _ = rx.recv_async().await;
                    this.update(cx, |this, cx| {
                        this.refresh(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            PendingOpKind::NewFolder => {
                cx.spawn(async move |this, cx| {
                    let rx = session.sftp_mkdir(remote);
                    let _ = rx.recv_async().await;
                    this.update(cx, |this, cx| {
                        this.refresh(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Delete the currently selected remote file or directory (recursive).
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.selected else {
            self.status = "请先选中要删除的条目".to_string();
            cx.notify();
            return;
        };
        let Some(entry) = self.entries.get(ix) else {
            self.selected = None;
            cx.notify();
            return;
        };
        let name = entry.name.clone();
        let remote = remote_join(&self.path, &name);
        let recursive = entry.is_dir;
        let session = self.session.clone();

        self.selected = None;
        self.status = "删除中…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let rx = session.sftp_remove(remote, recursive);
            match rx.recv_async().await {
                Ok(Ok(())) => {
                    this.update(cx, |this, cx| {
                        this.refresh(cx);
                        cx.notify();
                    })
                }
                Ok(Err(e)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("删除失败: {e}");
                        cx.notify();
                    })
                }
                Err(_) => {
                    this.update(cx, |this, cx| {
                        this.status = "删除失败: session closed".to_string();
                        cx.notify();
                    })
                }
            }
            .ok();
        })
        .detach();
    }

    /// Cancel an in-progress transfer by id.
    fn cancel_transfer(&self, id: u64) {
        self.session.sftp_cancel(id);
    }

    /// Background pump: drains `TransferEvent`s from the handle's channel
    /// and updates the matching `Transfer` row. Runs until the channel
    /// closes (which the session thread does after the final
    /// Done/Failed/Cancelled).
    async fn pump_events(
        this: gpui::WeakEntity<Self>,
        id: u64,
        events: flume::Receiver<TransferEvent>,
        cx: &mut AsyncApp,
    ) {
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

/// Layout (vertical):
///   Root: flex-col, pinned `size_full`.
///     ├─ TOP pane (flex_1 fill): title bar + toolbar + path bar + column
///     │    header + file list (internal scroll) + status row.
///     ├─ Draggable splitter (4px).
///     ├─ BOTTOM pane (height driven by `self.transfers_height`): transfer
///     │    section header + list + default download dir.
impl Render for SftpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path_input = self.ensure_path_input(window, cx);
        self.sync_path_input(window, cx);

        let title_bar = self.render_title_bar(cx);
        let toolbar = self.render_toolbar(window, cx);
        let pending_op = self.render_pending_op_row(cx);
        let path_bar = self.render_path_bar(&path_input, cx);
        let column_header = self.render_column_header(cx);
        let file_list = self.render_file_list(cx);
        let status_row = self.render_status_row(cx);

        let transfer_header = self.render_transfer_header(cx);
        let transfer_body = self.render_transfer_body(cx);
        let download_dir_bar = self.render_download_dir_bar(cx);

        let splitter = div()
            .id("sftp-splitter")
            .h(px(4.0))
            .w_full()
            .bg(cx.theme().border)
            .hover(|s| s.bg(cx.theme().accent))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, _window, _cx| {
                this.drag_start = Some((this.transfers_height, event.position.y));
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let Some((initial_height, initial_y)) = this.drag_start else { return };
                let dy = event.position.y - initial_y;
                let new_h = initial_height + dy;
                let clamped = new_h.clamp(px(80.0), px(600.0));
                if clamped != this.transfers_height {
                    this.transfers_height = clamped;
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                if this.drag_start.is_some() {
                    this.drag_start = None;
                    cx.notify();
                }
            }));

        let top_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .child(title_bar)
            .child(toolbar)
            .child(pending_op)
            .child(path_bar)
            .child(column_header)
            .child(file_list)
            .child(status_row);

        let bottom_pane = div()
            .h(self.transfers_height)
            .flex()
            .flex_col()
            .child(transfer_header)
            .child(transfer_body)
            .child(download_dir_bar);

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .text_sm()
            .child(top_pane)
            .child(splitter)
            .child(bottom_pane)
    }
}

// --- render sub-sections --------------------------------------------------

impl SftpPanel {
    fn render_title_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .text_sm()
            .text_color(cx.theme().foreground)
            .child("文件浏览器")
    }

    fn render_pending_op_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let Some((kind, input)) = self.pending_op.as_ref() else {
            return div().into_any_element();
        };
        let label = match kind {
            PendingOpKind::NewFile => "新建文件:",
            PendingOpKind::NewFolder => "新建文件夹:",
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().child(Input::new(input).xsmall()))
            .into_any_element()
    }

    fn render_toolbar(&self, _window: &mut Window, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                // 新建文件
                div()
                    .id("sftp-new-file")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::NewFile).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, window, cx| this.new_file(window, cx))),
            )
            .child(
                // 新建文件夹
                div()
                    .id("sftp-new-folder")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::NewFolder).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, window, cx| this.new_folder(window, cx))),
            )
            .child(
                // 上传
                div()
                    .id("sftp-upload")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::Upload).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, _w, cx| this.upload(cx))),
            )
            .child(
                // 下载
                div()
                    .id("sftp-download")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::Download).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, _w, cx| this.download_selected(cx))),
            )
            .child(
                div().w(px(1.)).h(px(20.)).bg(cx.theme().border),
            )
            .child(
                // 删除
                div()
                    .id("sftp-delete")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                    .child(icon(AppIcon::Delete).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, _w, cx| this.delete_selected(cx))),
            )
            .child(
                // 向上一级
                div()
                    .id("sftp-up")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::Up).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, window, cx| this.go_up(window, cx))),
            )
            .child(
                // 刷新
                div()
                    .id("sftp-refresh")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(icon(AppIcon::Refresh).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, _w, cx| this.refresh(cx))),
            )
    }

    fn render_path_bar(
        &self,
        path_input: &Entity<InputState>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(div().flex_1().child(Input::new(path_input).xsmall()))
            .child(
                div()
                    .id("sftp-copy-path")
                    .p_1()
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(Icon::new(IconName::Copy).size(px(14.0)))
                    .on_click(cx.listener(|this, _e, _w, cx| this.copy_path(cx))),
            )
    }

    fn render_column_header(&self, cx: &Context<Self>) -> impl IntoElement {
        static HEADERS: [&str; 4] = ["名称", "修改时间", "大小", "权限"];
        let mut cells: Vec<gpui::AnyElement> = Vec::new();
        for (i, &header_label) in HEADERS.iter().enumerate() {
            cells.push(
                div()
                    .w(self.col_widths[i])
                    .min_w(px(32.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .when(i == 2, |d| d.text_right())
                    .child(header_label)
                    .into_any_element(),
            );
            if i < 3 {
                let i_divider = i;
                cells.push(
                    div()
                        .h_full()
                        .w(px(4.0))
                        .hover(|s| s.bg(cx.theme().accent))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _window, _cx| {
                                this.col_drag = Some((
                                    i_divider,
                                    this.col_widths[i_divider],
                                    event.position.x,
                                ));
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                            let Some((idx, initial_w, initial_x)) = this.col_drag else {
                                return;
                            };
                            let dx = event.position.x - initial_x;
                            let new_w = (initial_w + dx).clamp(px(32.0), px(400.0));
                            if new_w != this.col_widths[idx] {
                                this.col_widths[idx] = new_w;
                                cx.notify();
                            }
                        }))
                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                            if this.col_drag.is_some() {
                                this.col_drag = None;
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                );
            }
        }

        div()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_1()
            .gap_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .children(cells)
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
            let file_icon = if is_dir { AppIcon::Folder } else { AppIcon::NewFile };

            div()
                .id(("sftp-row", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_0()
                .px_3()
                .py_0p5()
                .text_color(cx.theme().foreground)
                .when(self.selected == Some(ix), |d| d.bg(cx.theme().list_active))
                .hover(|s| s.bg(cx.theme().list_hover))
                .child(
                    div()
                        .w(self.col_widths[0])
                        .min_w(px(32.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .overflow_hidden()
                        .child(
                            icon(file_icon)
                                .size(px(12.0))
                                .text_color(cx.theme().muted_foreground),
                        )
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
                        .w(self.col_widths[1])
                        .min_w(px(32.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(mtime_text)),
                )
                .child(
                    div()
                        .w(self.col_widths[2])
                        .min_w(px(32.0))
                        .text_xs()
                        .text_right()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(size_text)),
                )
                .child(
                    div()
                        .w(self.col_widths[3])
                        .min_w(px(32.0))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(perms_text)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
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
            .id("sftp-file-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.file_list_scroll_handle)
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
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(summary)),
            )
    }

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
            .child("文件传输")
    }

    fn render_transfer_body(&self, cx: &Context<Self>) -> impl IntoElement {
        if self.transfers.is_empty() {
            div()
                .flex_1()
                .min_h(px(0.0))
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
                    TransferStatus::Cancelled => "已取消".to_string(),
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
                let transfer_id = t.id;
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_3()
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
                            )
                            .when(is_running, |d| {
                                d.child(
                                    div()
                                        .px_1()
                                        .rounded_sm()
                                        .text_xs()
                                        .text_color(cx.theme().foreground)
                                        .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                                        .child(icon(AppIcon::Delete).size(px(14.0)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |this, _event: &gpui::MouseDownEvent, _w, _cx| {
                                                    this.cancel_transfer(transfer_id);
                                                },
                                            ),
                                        ),
                                )
                            }),
                    )
                    .child(
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
            div().id("sftp-transfer-list").flex().flex_col().flex_1().min_h(px(0.0)).overflow_y_scroll().children(rows).into_any_element()
        }
    }

    /// Render the bottom bar showing the default download directory.
    fn render_download_dir_bar(
        &self,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let path_str = self.download_dir.to_string_lossy().to_string();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("下载到:"),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(path_str)),
            )
    }
}

// --- helpers --------------------------------------------------------------

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn download_default_dir() -> PathBuf {
    let p = home_dir();
    let downloads = p.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        p
    }
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