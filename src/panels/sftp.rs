//! `SftpPanel`: remote file browser over the SFTP subsystem of an existing
//! [`SshSession`] (the *same* connection as the terminal — CLAUDE.md §2: one
//! Session, one connection). Renders a layout driven by gpui-component
//! primitives:
//!
//! 1. Toolbar — `Button` icon buttons (new file / new folder / upload /
//!    download / delete / up / refresh) with tooltips.
//! 2. Path bar — current remote path + copy-path button.
//! 3. File list — `DataTable` with 4 columns (name / mtime / size / perms),
//!    sortable, keyboard-navigable, virtual-scrolled.
//! 4. Status row — total item count + total bytes.
//! 5. Transfers section — header + live list of background downloads/uploads
//!    with progress bars (or empty-state "无传输记录").
//! 6. Bottom path bar — full path echo.
//!
//! The top (file browser) and bottom (transfers) panes are split by a
//! draggable `v_resizable` handle.
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
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::notification::NotificationType;
use gpui_component::resizable::{resizable_panel, v_resizable, ResizableState};
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState};
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable, WindowExt};

use crate::panels::icons::{AppIcon, icon};
use crate::workspace::Workspace;

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
    /// Renaming the entry at this row index (in `FileTableDelegate::entries`).
    Rename(usize),
}

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

impl TableDelegate for FileTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.entries.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(entry) = self.entries.get(row_ix) else {
            return menu;
        };
        let is_dir = entry.is_dir;
        let name_for_open = entry.name.clone();

        let panel_open = self.panel.clone();
        let panel_download = self.panel.clone();
        let panel_rename = self.panel.clone();
        let panel_properties = self.panel.clone();
        let panel_copy = self.panel.clone();
        let panel_delete = self.panel.clone();

        menu.item(PopupMenuItem::new("打开").on_click(move |_ev, window, cx| {
            let _ = panel_open.update(cx, |panel, cx| {
                if is_dir {
                    panel.enter_dir(&name_for_open, window, cx);
                } else {
                    panel.download(&name_for_open, cx);
                }
            });
        }))
        .item(PopupMenuItem::new("下载").on_click(move |_ev, window, cx| {
            let _ = panel_download.update(cx, |panel, cx| panel.download_selected(window, cx));
        }))
        .item(PopupMenuItem::new("重命名").on_click(move |_ev, window, cx| {
            let _ = panel_rename.update(cx, |panel, cx| panel.rename_entry(row_ix, window, cx));
        }))
        .item(PopupMenuItem::new("属性").on_click(move |_ev, window, cx| {
            let _ = panel_properties
                .update(cx, |panel, cx| panel.show_properties(row_ix, window, cx));
        }))
        .item(PopupMenuItem::new("复制路径").on_click(move |_ev, _window, cx| {
            let _ = panel_copy.update(cx, |panel, cx| panel.copy_entry_path(row_ix, cx));
        }))
        .item(PopupMenuItem::new("删除").on_click(move |_ev, window, cx| {
            let _ = panel_delete.update(cx, |panel, cx| panel.delete_selected(window, cx));
        }))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let entry = &self.entries[row_ix];
        let col = &self.columns[col_ix];

        match col.key.as_ref() {
            "name" => {
                let display_name = if entry.is_dir {
                    format!("{}/", entry.name)
                } else {
                    entry.name.clone()
                };
                let file_icon = if entry.is_dir {
                    AppIcon::Folder
                } else {
                    AppIcon::NewFile
                };
                div()
                    .w_full()
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
                    )
                    .into_any_element()
            }
            "mtime" => div()
                .w_full()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(human_mtime(entry.mtime)))
                .into_any_element(),
            "size" => div()
                .w_full()
                .text_xs()
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(if entry.is_dir {
                    "—".to_string()
                } else {
                    human_size(entry.size)
                }))
                .into_any_element(),
            "perms" => div()
                .w_full()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(human_perms(entry.perms)))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        let key = self.columns[col_ix].key.as_ref();
        match (key, sort) {
            ("name", ColumnSort::Ascending) => self.entries.sort_by(|a, b| a.name.cmp(&b.name)),
            ("name", ColumnSort::Descending) => self.entries.sort_by(|a, b| b.name.cmp(&a.name)),
            ("size", ColumnSort::Ascending) => self.entries.sort_by_key(|e| e.size),
            ("size", ColumnSort::Descending) => {
                self.entries.sort_by_key(|e| std::cmp::Reverse(e.size))
            }
            ("mtime", ColumnSort::Ascending) => self.entries.sort_by_key(|e| e.mtime),
            ("mtime", ColumnSort::Descending) => {
                self.entries.sort_by_key(|e| std::cmp::Reverse(e.mtime))
            }
            _ => {}
        }
    }
}

pub struct SftpPanel {
    session: Arc<SshSession>,
    label: SharedString,
    focus_handle: FocusHandle,
    path: String,
    table_state: Entity<TableState<FileTableDelegate>>,
    status: String,
    transfers: Vec<Transfer>,
    path_input: Option<Entity<InputState>>,
    _path_sub: Option<Subscription>,
    resize_state: Entity<ResizableState>,
    /// Inline name-entry row: shown when the user clicks 新建文件 / 新建文件夹.
    pending_op: Option<(PendingOpKind, Entity<InputState>)>,
    /// Default local download directory. Editable in the bottom bar.
    download_dir: PathBuf,
    /// Whether dotfiles (names starting with `.`) are shown. Toggled by the
    /// toolbar's hidden-files button; not persisted.
    show_hidden: bool,
    /// Most-recently-visited directories, oldest first, capped at 20. Shown
    /// by the path bar's history dropdown; not persisted.
    history: Vec<String>,
    /// Back-reference for cwd-sync — see `send_path_to_terminal`/
    /// `sync_cwd_from_terminal`.
    workspace: WeakEntity<Workspace>,
}

impl SftpPanel {
    pub fn new(
        session: Arc<SshSession>,
        label: impl Into<SharedString>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let download_dir = download_default_dir();
        let panel = cx.entity().downgrade();
        let delegate = FileTableDelegate::new(panel);
        let table_state = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_resizable(true)
                .sortable(true)
        });
        let resize_state = cx.new(|_| ResizableState::default());
        let mut this = Self {
            session,
            label: label.into(),
            focus_handle: cx.focus_handle(),
            path: ".".to_string(),
            table_state,
            status: "Loading…".to_string(),
            transfers: Vec::new(),
            path_input: None,
            _path_sub: None,
            resize_state,
            pending_op: None,
            download_dir,
            show_hidden: false,
            history: Vec::new(),
            workspace,
        };
        // Wire double-click → enter dir / download. Needs `window`, so we
        // call it here in `new()` before `refresh`.
        this.wire_table_events(window, cx);
        this.refresh(cx);

        // Resolve "." → absolute home dir so the bottom path bar and top
        // path input show e.g. /home/user immediately.
        let session = this.session.clone();
        let _table_state = this.table_state.clone();
        cx.spawn(async move |this, cx| {
            let rx = session.sftp_realpath(".".to_string());
            if let Ok(Ok(resolved)) = rx.recv_async().await {
                let fin = resolved.trim_end_matches('/').to_string();
                let fin = if fin.is_empty() { "/".to_string() } else { fin };
                let _ = this.update(cx, |this, cx| {
                    if fin != this.path {
                        this.path = fin;
                    }
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
        let sub = cx.subscribe_in(&entity, window, |this, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_path(window, cx);
            }
        });
        self._path_sub = Some(sub);
        self.path_input = Some(entity.clone());
        entity
    }

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

    /// Re-list the current directory.
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

    fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let show_hidden = self.show_hidden;
        self.table_state.update(cx, |state, cx| {
            state.delegate_mut().apply_hidden_filter(show_hidden);
            state.refresh(cx);
        });
        cx.notify();
    }

    fn enter_dir(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let path = remote_join(&self.path, name);
        self.navigate_to(path, window, cx);
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = remote_parent(&self.path);
        self.navigate_to(path, window, cx);
    }

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

    fn download_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, is_dir) = {
            let state = self.table_state.read(cx);
            let Some(ix) = state.selected_row() else {
                window.push_notification(
                    (NotificationType::Warning, "请先选中一个文件再点下载"),
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
                (NotificationType::Warning, "暂不支持下载文件夹"),
                cx,
            );
            return;
        }
        self.download(&name, cx);
    }

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
                // Dialog was cancelled (or errored) before any file was picked —
                // drop the placeholder row instead of showing a spurious failure.
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

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.new(|cx| InputState::new(window, cx).submit_on_enter(true));
        cx.subscribe_in(&entity, window, |this: &mut Self, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_pending_op(window, cx);
            }
        })
        .detach();
        self.pending_op = Some((PendingOpKind::NewFile, entity));
        cx.notify();
    }

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

    fn rename_entry(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(old_name) = self
            .table_state
            .read(cx)
            .delegate()
            .entries
            .get(ix)
            .map(|e| e.name.clone())
        else {
            return;
        };
        let entity = cx.new(|cx| {
            InputState::new(window, cx).submit_on_enter(true).default_value(old_name)
        });
        cx.subscribe_in(&entity, window, |this: &mut Self, _state, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.commit_pending_op(window, cx);
            }
        })
        .detach();
        self.pending_op = Some((PendingOpKind::Rename(ix), entity));
        cx.notify();
    }

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
        if let PendingOpKind::Rename(_) = kind {
            if name.contains('/') {
                self.status = "重命名失败: 名称不能包含 \"/\"".to_string();
                cx.notify();
                return;
            }
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
            PendingOpKind::Rename(ix) => {
                let Some(old_name) = self
                    .table_state
                    .read(cx)
                    .delegate()
                    .entries
                    .get(ix)
                    .map(|e| e.name.clone())
                else {
                    return;
                };
                let old_remote = remote_join(&self.path, &old_name);
                let new_remote = remote;
                let table_state = self.table_state.clone();
                let new_name = name.clone();
                cx.spawn(async move |this, cx| {
                    let rx = session.sftp_rename(old_remote, new_remote);
                    match rx.recv_async().await {
                        Ok(Ok(())) => {
                            this.update(cx, |_this, cx| {
                                table_state.update(cx, |state, cx| {
                                    if let Some(entry) = state.delegate_mut().entries.get_mut(ix) {
                                        entry.name = new_name.clone();
                                    }
                                    state.refresh(cx);
                                });
                                cx.notify();
                            })
                            .ok();
                        }
                        Ok(Err(e)) => {
                            this.update(cx, |this, cx| {
                                this.status = format!("重命名失败: {e}");
                                cx.notify();
                            })
                            .ok();
                        }
                        Err(_) => {
                            this.update(cx, |this, cx| {
                                this.status = "重命名失败: session closed".to_string();
                                cx.notify();
                            })
                            .ok();
                        }
                    }
                })
                .detach();
            }
        }
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, is_dir) = {
            let state = self.table_state.read(cx);
            let Some(ix) = state.selected_row() else {
                window.push_notification(
                    (NotificationType::Warning, "请先选中要删除的条目"),
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

        let session = self.session.clone();
        let path = self.path.clone();
        let table_state = self.table_state.clone();
        let weak_panel = cx.entity().downgrade();
        let delete_name = name.clone();

        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            // `open_alert_dialog`'s builder is an `Fn` (invoked on every render of
            // the dialog), so it must not move its captures out. Clone them into
            // locals here and let the `on_ok` closure move the locals instead.
            let weak_panel = weak_panel.clone();
            let session = session.clone();
            let table_state = table_state.clone();
            let delete_name = delete_name.clone();
            let path = path.clone();
            alert
                .title("确认删除")
                .description(format!(
                    "确定要删除「{name}」吗？{}此操作不可撤销。",
                    if is_dir { "文件夹及其所有内容都会被删除，" } else { "" }
                ))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    let weak_panel = weak_panel.clone();
                    let session = session.clone();
                    let table_state = table_state.clone();
                    let name = delete_name.clone();
                    let remote = remote_join(&path, &name);
                    cx.spawn(async move |cx| {
                        weak_panel.update(cx, |this, cx| {
                            this.status = "删除中…".to_string();
                            cx.notify();
                        }).ok();
                        let rx = session.sftp_remove(remote, is_dir);
                        match rx.recv_async().await {
                            Ok(Ok(())) => {
                                weak_panel.update(cx, |_this, cx| {
                                    table_state.update(cx, |state, cx| {
                                        state.delegate_mut().entries.retain(|e| e.name != name);
                                        state.refresh(cx);
                                    });
                                    cx.notify();
                                }).ok();
                            }
                            Ok(Err(e)) => {
                                weak_panel.update(cx, |this, cx| {
                                    this.status = format!("删除失败: {e}");
                                    cx.notify();
                                }).ok();
                            }
                            Err(_) => {
                                weak_panel.update(cx, |this, cx| {
                                    this.status = "删除失败: session closed".to_string();
                                    cx.notify();
                                }).ok();
                            }
                        }
                    })
                    .detach();
                    true
                })
        });
    }

    fn cancel_transfer(&self, id: u64) {
        self.session.sftp_cancel(id);
    }

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

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.path.clone()));
    }

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

    fn copy_entry_path(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(name) = self.table_state.read(cx).delegate().entries.get(ix).map(|e| e.name.clone())
        else {
            return;
        };
        let remote = remote_join(&self.path, &name);
        cx.write_to_clipboard(ClipboardItem::new_string(remote));
    }

    fn show_properties(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.table_state.read(cx).delegate().entries.get(ix).cloned() else {
            return;
        };
        let path = remote_join(&self.path, &entry.name);
        let kind = if entry.is_dir { "文件夹" } else { "文件" }.to_string();
        let size = if entry.is_dir { "—".to_string() } else { human_size(entry.size) };
        let mtime = human_mtime(entry.mtime);
        let perms = human_perms(entry.perms);
        let name = entry.name;

        window.open_alert_dialog(cx, move |alert, _window, cx| {
            let grid = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(properties_row("名称", &name, cx))
                .child(properties_row("路径", &path, cx))
                .child(properties_row("类型", &kind, cx))
                .child(properties_row("大小", &size, cx))
                .child(properties_row("修改时间", &mtime, cx))
                .child(properties_row("权限", &perms, cx));
            alert.title("属性").description(grid)
        });
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
            .child("连接会话以浏览文件")
    }
}

// --- main render ----------------------------------------------------------

impl Render for SftpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path_input = self.ensure_path_input(window, cx);
        self.sync_path_input(window, cx);

        let title_bar = self.render_title_bar(cx);
        let toolbar = self.render_toolbar(window, cx);
        let pending_op = self.render_pending_op_row(cx);
        let path_bar = self.render_path_bar(&path_input, cx);
        let file_list = self.render_file_list(cx);
        let status_row = self.render_status_row(cx);

        let transfer_header = self.render_transfer_header(cx);
        let transfer_body = self.render_transfer_body(cx);
        let download_dir_bar = self.render_download_dir_bar(cx);

        let top_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .child(title_bar)
            .child(toolbar)
            .child(pending_op)
            .child(path_bar)
            .child(file_list)
            .child(status_row);

        let bottom_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .child(transfer_header)
            .child(transfer_body)
            .child(download_dir_bar);

        v_resizable("sftp-layout")
            .with_state(&self.resize_state)
            .child(resizable_panel().child(top_pane))
            .child(
                resizable_panel()
                    .size(px(200.))
                    .size_range(px(80.)..px(600.))
                    .child(bottom_pane),
            )
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
            PendingOpKind::NewFile => "新建文件:".to_string(),
            PendingOpKind::NewFolder => "新建文件夹:".to_string(),
            PendingOpKind::Rename(ix) => {
                let old_name = self
                    .table_state
                    .read(cx)
                    .delegate()
                    .entries
                    .get(*ix)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                format!("重命名「{old_name}」为:")
            }
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
        let has_selection = self.table_state.read(cx).selected_row().is_some();

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
                Button::new("sftp-new-file")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::NewFile))
                    .tooltip("新建文件")
                    .on_click(cx.listener(|this, _, window, cx| this.new_file(window, cx))),
            )
            .child(
                Button::new("sftp-new-folder")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::NewFolder))
                    .tooltip("新建文件夹")
                    .on_click(cx.listener(|this, _, window, cx| this.new_folder(window, cx))),
            )
            .child(
                Button::new("sftp-upload")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Upload))
                    .tooltip("上传")
                    .on_click(cx.listener(|this, _, _w, cx| this.upload(cx))),
            )
            .child(
                Button::new("sftp-download")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Download))
                    .tooltip("下载")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, window, cx| this.download_selected(window, cx))),
            )
            .child(
                div().w(px(1.)).h(px(20.)).bg(cx.theme().border),
            )
            .child(
                Button::new("sftp-delete")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Delete))
                    .tooltip("删除")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, window, cx| this.delete_selected(window, cx))),
            )
            .child(
                Button::new("sftp-up")
                    .xsmall()
                    .ghost()
                    .icon(icon(AppIcon::Up))
                    .tooltip("向上一级")
                    .on_click(cx.listener(|this, _, window, cx| this.go_up(window, cx))),
            )
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

    fn render_file_list(&self, _cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .child(DataTable::new(&self.table_state))
    }

    /// Wire up table event handlers (double-click to enter/download). Called
    /// once from `new()` where `window` is available for `subscribe_in`.
    fn wire_table_events(&self, window: &mut Window, cx: &mut Context<Self>) {
        let table_state = self.table_state.clone();
        cx.subscribe_in(&self.table_state, window, move |this, _, event, window, cx| {
            match event {
                TableEvent::DoubleClickedRow(ix) => {
                    let name = {
                        let state = table_state.read(cx);
                        state.delegate().entries.get(*ix).map(|e| e.name.clone())
                    };
                    if let Some(name) = name {
                        let is_dir = {
                            let state = table_state.read(cx);
                            state.delegate().entries.get(*ix).map(|e| e.is_dir).unwrap_or(false)
                        };
                        if is_dir {
                            this.enter_dir(&name, window, cx);
                        } else {
                            this.download(&name, cx);
                        }
                    }
                }
                TableEvent::RightClickedRow(Some(ix)) => {
                    table_state.update(cx, |state, cx| {
                        state.set_selected_row(*ix, cx);
                    });
                }
                _ => {}
            }
        })
        .detach();
    }

    fn render_status_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let (count, total_bytes) = {
            let state = self.table_state.read(cx);
            let entries = &state.delegate().entries;
            let count = entries.len();
            let total_bytes: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            (count, total_bytes)
        };
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
                                    Button::new(format!("sftp-cancel-{transfer_id}"))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Delete)
                                        .tooltip("取消传输")
                                        .on_click(cx.listener(
                                            move |this, _, _w, _cx| {
                                                this.cancel_transfer(transfer_id);
                                            },
                                        )),
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
            let list: gpui::Stateful<gpui::Div> = div()
                .id("sftp-transfer-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll();
            list.children(rows).into_any_element()
        }
    }

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

/// One label/value row in the properties dialog's key/value grid.
fn properties_row(label: &str, value: &str, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .child(
            div()
                .min_w(px(64.0))
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(label.to_string())),
        )
        .child(div().child(SharedString::from(value.to_string())))
}

fn human_mtime(mtime: u32) -> String {
    if mtime == 0 {
        return "—".to_string();
    }
    let secs = mtime as i64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;

    // civil_from_days: convert days-since-1970-01-01 (with +719_468 offset to
    // align the civil-day epoch) to (y, m, d). Handles leap years / month
    // lengths correctly, unlike a fixed 365/30 approximation.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}
