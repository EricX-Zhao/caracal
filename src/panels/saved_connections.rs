//! `SavedConnectionsPanel`: the right-dock "已保存的连接" list. Renders the
//! persisted connections ([`crate::config`]) as nyaterm-style tree with groups,
//! search, sort, and hover actions. Double-clicking a connection emits
//! [`SavedConnectionsEvent::Open`] (workspace opens the terminal); right-click
//! opens a context menu on both connections and folders; connections can be
//! dragged into (or out of) a folder to change their group.
//!
//! NyaTerm-style layout:
//! ```
//! ┌─ PanelHeader ─────────────────────────────────────────┐
//! │  Saved Connections                              [count]│
//! ├───────────────────────────────────────────────────────┤
//! │ [🔍 search input                    ] [sort] [⚡]     │
//! │ [📁 New Folder] [➕ New Connection] [⋯ More]          │
//! ├───────────────────────────────────────────────────────┤
//! │ ▾ Production (3)                                      │
//! │   ├─ 🖥️ prod-db-01 (double-click to open)  [✏️] [🗑️]   │
//! │   ├─ 📁 Staging ▾                                     │
//! │   │    └─ 🖥️ staging-web                                │
//! │   └─ 🖥️ prod-web-01                                    │
//! │ ▾ Personal (2)                                        │
//! │   ├─ 💻 my-laptop                                     │
//! │   └─ 🖥️ home-server                                    │
//! │ ───────────────────────────────────────────────────── │
//! │   🖥️ quick-test                                        │
//! │   💻 local-shell                                       │
//! └───────────────────────────────────────────────────────┘
//! ```
//!
//! No connection logic here — it only describes intent and persists the list
//! (CLAUDE.md §1 boundary).

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    Action, App, AppContext, ClickEvent, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    WindowHandle, div, px,
};
use gpui_component::Root;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::ContextMenuExt;
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, StyledExt, WindowExt};
use serde::Deserialize;

use crate::panels::icons::{AppIcon, icon};

use crate::config::{self, AppConfig, ConnectionType, SavedConnection, SavedConnectionGroup};
use crate::panels::new_connection_window::NewConnectionWindow;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::SshConfig;
use crate::terminal::telnet::TelnetConfig;

/// Actions dispatched from the connection/folder right-click context menus
/// (`ContextMenuExt::context_menu`, `gpui_component::menu`). Each carries the
/// row identity it was built for so the panel's `on_action` handlers can act
/// without any extra "currently selected row" state.
#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct OpenConnection {
    ix: usize,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct EditConnection {
    ix: usize,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct DuplicateConnection {
    ix: usize,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct DeleteConnection {
    ix: usize,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct NewConnectionInGroup {
    group_id: String,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct NewSubfolder {
    parent_id: String,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct RenameGroup {
    group_id: String,
}

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct DeleteGroup {
    group_id: String,
}

/// Blank-area context menu (right-clicking empty space in the panel, not on
/// any connection/folder row): root-level "新建连接" / "新建文件夹".
#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct NewRootConnection;

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct NewRootFolder;

/// Drag payload for dragging a connection row onto a folder (or onto the
/// ungrouped-connections area to un-group it). Rendered as a small pill
/// following the cursor, mirroring gpui-component's own `DragPanel`
/// (`crates/ui/src/dock/tab_panel.rs`).
#[derive(Clone)]
struct DragConnection {
    ix: usize,
    name: SharedString,
}

impl Render for DragConnection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(cx.theme().accent)
            .text_sm()
            .text_color(cx.theme().accent_foreground)
            .child(self.name.clone())
    }
}

/// Target for the folder-name form: creating a new (sub)folder, or renaming
/// an existing one. Both cases share the same name-input UI
/// (`render_folder_form`).
enum FolderFormTarget {
    New(Option<String>),
    Rename(String),
}

/// Emitted when the user picks a saved connection to open.
pub enum SavedConnectionsEvent {
    /// Open an SSH shell terminal.
    Open(SshConfig),
    /// Open an SFTP browser (routed to the bottom "SFTP" dock).
    #[allow(dead_code)]
    OpenSftp(SshConfig),
    /// Open a local terminal.
    OpenLocal(String, String),
    /// Open a raw Telnet terminal.
    OpenTelnet(TelnetConfig),
    /// Open a serial-port terminal.
    OpenSerial(SerialConfig),
}

/// Build the event that opens `conn`, dispatching on its connection type.
/// Shared by the row's double-click handler and the context menu's "打开"
/// action so the two call sites can't drift.
fn open_event(conn: &SavedConnection) -> SavedConnectionsEvent {
    match conn.conn_type {
        ConnectionType::Ssh => SavedConnectionsEvent::Open(conn.to_ssh_config()),
        ConnectionType::Local => SavedConnectionsEvent::OpenLocal(
            conn.shell_path.clone().unwrap_or_default(),
            conn.working_dir.clone().unwrap_or_default(),
        ),
        ConnectionType::Telnet => SavedConnectionsEvent::OpenTelnet(conn.to_telnet_config()),
        ConnectionType::Serial => SavedConnectionsEvent::OpenSerial(conn.to_serial_config()),
    }
}

/// Sort mode for the connection list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Default,
    NameAsc,
    NameDesc,
}

impl SortMode {
    fn cycle(self) -> Self {
        match self {
            SortMode::Default => SortMode::NameAsc,
            SortMode::NameAsc => SortMode::NameDesc,
            SortMode::NameDesc => SortMode::Default,
        }
    }

    #[allow(dead_code)]
    fn label(self) -> &'static str {
        match self {
            SortMode::Default => "默认",
            SortMode::NameAsc => "名称 ↑",
            SortMode::NameDesc => "名称 ↓",
        }
    }
}

pub struct SavedConnectionsPanel {
    focus_handle: FocusHandle,
    connections: Vec<SavedConnection>,
    groups: Vec<SavedConnectionGroup>,
    // UI state
    search_query: Entity<InputState>,
    sort_mode: SortMode,
    expanded_groups: HashSet<String>,
    // Form state
    folder_form_target: Option<FolderFormTarget>,
    new_folder_name: Entity<InputState>,
    /// Kept alive so `new_folder_name`'s `InputEvent::PressEnter` subscription
    /// (submit-on-Enter, see `open_folder_form`) keeps firing.
    _folder_enter_sub: Option<Subscription>,
    /// The open new-connection/edit window, if any — re-triggering any of
    /// the "新建连接"/"编辑" entry points focuses this instead of opening a
    /// duplicate (see `open_new_connection_window`).
    new_connection_window: Option<WindowHandle<Root>>,
    /// `Some((target_ix, insert_before))` while a `DragConnection` is
    /// hovering over connection row `target_ix` — set by that row's
    /// `on_drag_move`, consumed and cleared by its `on_drop`. `None` when no
    /// drag is in progress or the drag isn't currently over a row.
    drag_reorder_target: Option<(usize, bool)>,
}

impl SavedConnectionsPanel {
    pub fn new(
        connections: Vec<SavedConnection>,
        groups: Vec<SavedConnectionGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索连接..."));
        let new_folder_name = cx.new(|cx| InputState::new(window, cx).placeholder("文件夹名称"));

        // Auto-expand all groups by default
        let expanded_groups: HashSet<String> = groups.iter().map(|g| g.id.clone()).collect();

        Self {
            focus_handle: cx.focus_handle(),
            connections,
            groups,
            search_query,
            sort_mode: SortMode::Default,
            expanded_groups,
            folder_form_target: None,
            new_folder_name,
            _folder_enter_sub: None,
            new_connection_window: None,
            drag_reorder_target: None,
        }
    }

    /// Generate a unique ID for new groups/connections.
    fn generate_id() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("id-{}", nanos)
    }

    /// Toggle the new-folder form (toolbar "新建文件夹" button — a root-level
    /// folder, no parent preset).
    fn toggle_folder_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder_form_target.is_some() {
            self.folder_form_target = None;
            cx.notify();
        } else {
            self.open_folder_form(FolderFormTarget::New(None), "", window, cx);
        }
    }

    /// Open the folder-name form for either creating a new (sub)folder or
    /// renaming an existing one, pre-filling `initial_name` for renames.
    fn open_folder_form(
        &mut self,
        target: FolderFormTarget,
        initial_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("文件夹名称")
                .default_value(initial_name)
                .submit_on_enter(true)
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self._folder_enter_sub = Some(cx.subscribe_in(&input, window, |this, _state, event, _window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.create_folder(cx);
            }
        }));
        self.new_folder_name = input;
        self.folder_form_target = Some(target);
        cx.notify();
    }

    /// Save the folder form: creates a new group or renames an existing one,
    /// depending on `folder_form_target`.
    fn create_folder(&mut self, cx: &mut Context<Self>) {
        let name = self.new_folder_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        match self.folder_form_target.take() {
            Some(FolderFormTarget::New(parent_id)) => {
                let group = SavedConnectionGroup {
                    id: Self::generate_id(),
                    name,
                    parent_id,
                    sort_order: self.groups.len() as i32,
                };
                self.expanded_groups.insert(group.id.clone());
                self.groups.push(group);
            }
            Some(FolderFormTarget::Rename(group_id)) => {
                if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
                    group.name = name;
                }
            }
            None => return,
        }
        self.persist();
        cx.notify();
    }

    /// Push a new connection or replace an existing one at `edit_ix`, persist,
    /// and repaint. Called by
    /// [`crate::panels::new_connection_window::NewConnectionWindow`] on Save.
    pub(crate) fn upsert_connection(
        &mut self,
        conn: SavedConnection,
        edit_ix: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        match edit_ix {
            Some(ix) if ix < self.connections.len() => self.connections[ix] = conn,
            _ => self.connections.push(conn),
        }
        self.persist();
        cx.notify();
    }

    /// Open the new-connection/edit window, or focus it if one is already
    /// open. `edit_ix` pre-fills the form from `self.connections[edit_ix]`
    /// for editing; `None` opens a blank form, optionally preset to
    /// `group_id` (used by the folder context menu's "新建连接").
    pub(crate) fn open_new_connection_window(
        &mut self,
        group_id: Option<String>,
        edit_ix: Option<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = &self.new_connection_window {
            if handle
                .update(cx, |_root, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
        }

        let existing = edit_ix.and_then(|ix| self.connections.get(ix).map(|c| (ix, c.clone())));
        let new_sort_order = self
            .connections
            .iter()
            .filter(|c| c.group_id == group_id)
            .count() as i32;
        let panel = cx.entity().downgrade();
        let bounds = gpui::Bounds::centered(None, gpui::size(px(480.0), px(560.0)), cx);
        let result = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            move |window, cx| {
                let new_window = cx.new(|cx| {
                    NewConnectionWindow::new(
                        panel.clone(),
                        existing,
                        group_id.clone(),
                        new_sort_order,
                        window,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(new_window, window, cx).bg(cx.theme().background))
            },
        );
        match result {
            Ok(handle) => self.new_connection_window = Some(handle),
            Err(e) => log::error!("failed to open new-connection window: {e}"),
        }
    }

    /// Delete a connection by index.
    fn delete(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.connections.len() {
            self.connections.remove(ix);
            self.persist();
            cx.notify();
        }
    }

    /// Show a confirmation dialog, then delete the connection on confirm.
    /// Mirrors `src/panels/sftp.rs`'s `delete_selected` alert-dialog pattern.
    fn confirm_delete_connection(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.connections.get(ix) else {
            return;
        };
        let name = conn.display_name();
        let weak_panel = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let weak_panel = weak_panel.clone();
            let name = name.clone();
            alert
                .title("确认删除")
                .description(format!("确定要删除连接「{name}」吗？此操作不可撤销。"))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    weak_panel.update(cx, |this, cx| this.delete(ix, cx)).ok();
                    true
                })
        });
    }

    /// Delete a group by ID. Connections that were in it become ungrouped
    /// rather than being deleted.
    fn delete_group(&mut self, group_id: &str, cx: &mut Context<Self>) {
        self.groups.retain(|g| g.id != group_id);
        for conn in &mut self.connections {
            if conn.group_id.as_deref() == Some(group_id) {
                conn.group_id = None;
            }
        }
        self.expanded_groups.remove(group_id);
        self.persist();
        cx.notify();
    }

    /// Show a confirmation dialog, then delete the folder on confirm.
    fn confirm_delete_group(&self, group_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.groups.iter().find(|g| g.id == group_id) else {
            return;
        };
        let name = group.name.clone();
        let weak_panel = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let weak_panel = weak_panel.clone();
            let name = name.clone();
            let group_id = group_id.clone();
            alert
                .title("确认删除")
                .description(format!(
                    "确定要删除文件夹「{name}」吗？其中的连接会被移到未分组。"
                ))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    weak_panel
                        .update(cx, |this, cx| this.delete_group(&group_id, cx))
                        .ok();
                    true
                })
        });
    }

    /// Duplicate a connection.
    fn duplicate(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.connections.len() {
            let mut new_conn = self.connections[ix].clone();
            if new_conn.name.is_empty() {
                new_conn.name = format!("copy-of-{}", new_conn.display_name());
            } else {
                new_conn.name = format!("{}-copy", new_conn.name);
            }
            // Clear credentials for security (matches the existing
            // password-clearing behavior, extended to the newer key-auth
            // fields so a duplicated connection never silently carries a
            // copy of a decryption passphrase).
            new_conn.password = String::new();
            new_conn.private_key_passphrase = None;
            new_conn.sort_order = self
                .connections
                .iter()
                .filter(|c| c.group_id == new_conn.group_id)
                .count() as i32;
            self.connections.push(new_conn);
            self.persist();
            cx.notify();
        }
    }

    /// Move a connection into a group, or to the root (`None`) to un-group
    /// it. Called from both drag-and-drop drop targets.
    fn move_connection_to_group(
        &mut self,
        ix: usize,
        group_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.connections.get(ix) else {
            return;
        };
        if conn.group_id == group_id {
            return;
        }
        let new_sort_order = self
            .connections
            .iter()
            .filter(|c| c.group_id == group_id)
            .count() as i32;
        if let Some(conn) = self.connections.get_mut(ix) {
            conn.group_id = group_id;
            conn.sort_order = new_sort_order;
            self.persist();
            cx.notify();
        }
    }

    /// Reorder `dragged_ix` to just before/after `target_ix` within their
    /// shared `group_id` scope. If the two connections are in different
    /// scopes, this isn't a reorder — fall back to the existing
    /// append-at-end cross-group move instead (matches the header/blank-area
    /// drop behavior; only same-scope drops get position control).
    fn reorder_connection(
        &mut self,
        dragged_ix: usize,
        target_ix: usize,
        insert_before: bool,
        cx: &mut Context<Self>,
    ) {
        if dragged_ix == target_ix
            || dragged_ix >= self.connections.len()
            || target_ix >= self.connections.len()
        {
            return;
        }
        let group_id = self.connections[target_ix].group_id.clone();
        if self.connections[dragged_ix].group_id != group_id {
            self.move_connection_to_group(dragged_ix, group_id, cx);
            return;
        }

        let mut siblings: Vec<usize> = self
            .connections
            .iter()
            .enumerate()
            .filter(|(_, c)| c.group_id == group_id)
            .map(|(ix, _)| ix)
            .collect();
        siblings.sort_by_key(|&ix| (self.connections[ix].sort_order, ix));
        siblings.retain(|&ix| ix != dragged_ix);

        let target_pos = siblings
            .iter()
            .position(|&ix| ix == target_ix)
            .unwrap_or(siblings.len());
        let insert_pos = if insert_before { target_pos } else { target_pos + 1 };
        siblings.insert(insert_pos, dragged_ix);

        for (order, ix) in siblings.into_iter().enumerate() {
            self.connections[ix].sort_order = order as i32;
        }

        self.persist();
        cx.notify();
    }

    /// Toggle group expansion.
    fn toggle_group(&mut self, group_id: &str, cx: &mut Context<Self>) {
        if self.expanded_groups.contains(group_id) {
            self.expanded_groups.remove(group_id);
        } else {
            self.expanded_groups.insert(group_id.to_string());
        }
        cx.notify();
    }

    // --- Context-menu action handlers -------------------------------------
    //
    // Registered on the panel's root div (see `Render::render`) and
    // dispatched by `gpui_component::menu::PopupMenu` when a context-menu
    // item is clicked (`window.dispatch_action`, bubbling up the element
    // tree to whichever ancestor registered `.on_action` for that type).

    fn on_action_open_connection(
        &mut self,
        action: &OpenConnection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.connections.get(action.ix) else {
            return;
        };
        cx.emit(open_event(conn));
    }

    fn on_action_edit_connection(
        &mut self,
        action: &EditConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_window(None, Some(action.ix), window, cx);
    }

    fn on_action_duplicate_connection(
        &mut self,
        action: &DuplicateConnection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(action.ix, cx);
    }

    fn on_action_delete_connection(
        &mut self,
        action: &DeleteConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_delete_connection(action.ix, window, cx);
    }

    fn on_action_new_connection_in_group(
        &mut self,
        action: &NewConnectionInGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_window(Some(action.group_id.clone()), None, window, cx);
    }

    fn on_action_new_subfolder(
        &mut self,
        action: &NewSubfolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_folder_form(
            FolderFormTarget::New(Some(action.parent_id.clone())),
            "",
            window,
            cx,
        );
    }

    fn on_action_rename_group(
        &mut self,
        action: &RenameGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_name = self
            .groups
            .iter()
            .find(|g| g.id == action.group_id)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        self.open_folder_form(
            FolderFormTarget::Rename(action.group_id.clone()),
            &current_name,
            window,
            cx,
        );
    }

    fn on_action_delete_group(
        &mut self,
        action: &DeleteGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_delete_group(action.group_id.clone(), window, cx);
    }

    fn on_action_new_root_connection(
        &mut self,
        _action: &NewRootConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_new_connection_window(None, None, window, cx);
    }

    fn on_action_new_root_folder(
        &mut self,
        _action: &NewRootFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_folder_form(FolderFormTarget::New(None), "", window, cx);
    }

    /// Cycle the sort mode.
    fn cycle_sort(&mut self, cx: &mut Context<Self>) {
        self.sort_mode = self.sort_mode.cycle();
        cx.notify();
    }

    /// Get filtered connections based on search query.
    fn filtered_connections(&self, cx: &App) -> Vec<(usize, &SavedConnection)> {
        let query = self.search_query.read(cx).value().trim().to_lowercase();
        if query.is_empty() {
            return self.connections.iter().enumerate().collect();
        }
        self.connections
            .iter()
            .enumerate()
            .filter(|(_, conn)| {
                let name = conn.display_name().to_lowercase();
                let subtitle = conn.subtitle().to_lowercase();
                name.contains(&query) || subtitle.contains(&query)
            })
            .collect()
    }

    /// Get root groups (parent_id is None).
    fn root_groups(&self) -> Vec<&SavedConnectionGroup> {
        let mut groups: Vec<_> = self
            .groups
            .iter()
            .filter(|g| g.parent_id.is_none())
            .collect();
        groups.sort_by_key(|g| g.sort_order);
        groups
    }

    /// Get child groups of a parent group.
    fn child_groups(&self, parent_id: &str) -> Vec<&SavedConnectionGroup> {
        let mut groups: Vec<_> = self
            .groups
            .iter()
            .filter(|g| g.parent_id.as_deref() == Some(parent_id))
            .collect();
        groups.sort_by_key(|g| g.sort_order);
        groups
    }

    /// Get connections in a group.
    fn connections_in_group(&self, group_id: &str, cx: &App) -> Vec<(usize, &SavedConnection)> {
        let filtered = self.filtered_connections(cx);
        filtered
            .into_iter()
            .filter(|(_, conn)| conn.group_id.as_deref() == Some(group_id))
            .collect()
    }

    /// Get ungrouped connections.
    fn ungrouped_connections(&self, cx: &App) -> Vec<(usize, &SavedConnection)> {
        let filtered = self.filtered_connections(cx);
        filtered
            .into_iter()
            .filter(|(_, conn)| conn.group_id.is_none())
            .collect()
    }

    /// Sort connections based on current sort mode.
    fn sort_connections(&self, conns: &mut [(usize, &SavedConnection)]) {
        match self.sort_mode {
            SortMode::Default => {
                conns.sort_by_key(|(_, c)| c.sort_order);
            }
            SortMode::NameAsc => {
                conns.sort_by(|a, b| a.1.display_name().cmp(&b.1.display_name()));
            }
            SortMode::NameDesc => {
                conns.sort_by(|a, b| b.1.display_name().cmp(&a.1.display_name()));
            }
        }
    }

    /// Persist the current state to disk.
    fn persist(&self) {
        let cfg = AppConfig {
            connections: self.connections.clone(),
            groups: self.groups.clone(),
        };
        if let Err(e) = config::save(&cfg) {
            log::error!("failed to save connections: {e}");
        }
    }
}

impl EventEmitter<SavedConnectionsEvent> for SavedConnectionsPanel {}
impl EventEmitter<PanelEvent> for SavedConnectionsPanel {}

impl Focusable for SavedConnectionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for SavedConnectionsPanel {
    fn panel_name(&self) -> &'static str {
        "SavedConnections"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("已保存的连接")
    }
}

impl SavedConnectionsPanel {
    /// Render the header with title and count.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.connections.len();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(cx.theme().foreground)
                    .child("已保存的连接"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("[{}]", count)),
            )
    }

    /// Render the toolbar with search, sort, and action buttons.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            // Search bar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        Input::new(&self.search_query)
                            .prefix(
                                icon(AppIcon::Search)
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .w_full(),
                    )
                    .child(
                        div()
                            .id("sort-btn")
                            .p(px(6.0))
                            .rounded_md()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(icon(AppIcon::Sort).text_color(cx.theme().muted_foreground))
                            .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.cycle_sort(cx)
                            })),
                    )
                    .child(
                        div()
                            .id("quick-ssh-btn")
                            .p(px(6.0))
                            .rounded_md()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(
                                icon(AppIcon::QuickConnect)
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .on_click(cx.listener(|_this, _ev: &ClickEvent, _w, _cx| {
                                // TODO: implement quick SSH
                            })),
                    ),
            )
            // Action buttons
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_0p5()
                    .child(
                        div()
                            .id("new-folder-btn")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(icon(AppIcon::NewFolder).text_color(cx.theme().muted_foreground))
                            .child("新建文件夹")
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.toggle_folder_form(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("new-conn-btn")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(icon(AppIcon::Plus).text_color(cx.theme().muted_foreground))
                            .child("新建连接")
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.open_new_connection_window(None, None, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("more-btn")
                            .p(px(6.0))
                            .rounded_md()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(
                                icon(AppIcon::MoreVertical)
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .on_click(cx.listener(|_this, _ev: &ClickEvent, _w, _cx| {
                                // TODO: show more menu
                            })),
                    ),
            )
    }

    /// Render the tree view of groups and connections.
    fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tree = div().flex().flex_col().gap_0p5();

        // Render root groups
        for group in self.root_groups() {
            tree = tree.child(self.render_group(group, 0, cx));
        }

        tree.child(self.render_ungrouped_section(cx))
    }

    /// The ungrouped-connections area at the bottom of the tree. Always
    /// rendered (with a minimum height) even when empty, so it stays a valid
    /// drop target for un-grouping a connection dragged out of a folder.
    fn render_ungrouped_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut ungrouped = self.ungrouped_connections(cx);
        self.sort_connections(&mut ungrouped);
        let has_groups = !self.root_groups().is_empty();
        let is_empty = ungrouped.is_empty();

        let mut section = div().flex().flex_col().min_h(px(24.0));

        if !is_empty && has_groups {
            section = section.child(div().h(px(1.0)).bg(cx.theme().border).my_1().mx_2());
        }

        for (ix, conn) in ungrouped {
            section = section.child(self.render_connection(ix, conn, 0, cx));
        }

        section
    }

    /// Fills the leftover space below all rendered rows in the scroll body.
    /// Deliberately a separate trailing sibling (not overlapping any row's
    /// hitbox) rather than a context menu on the scroll container itself:
    /// `ContextMenuExt::context_menu`'s right-click handler doesn't stop
    /// propagation, so stacking one on top of row-level context menus would
    /// pop both at once. Right-clicking here (truly blank space) offers the
    /// root-level "新建连接" / "新建文件夹" actions.
    fn render_blank_area(&self) -> impl IntoElement {
        div()
            .flex_1()
            .min_h(px(40.0))
            .context_menu(|menu, _window, _cx| {
                menu.menu("新建连接", Box::new(NewRootConnection))
                    .menu("新建文件夹", Box::new(NewRootFolder))
            })
    }

    /// Render a group row and its children.
    fn render_group(
        &self,
        group: &SavedConnectionGroup,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let group_id = group.id.clone();
        let is_expanded = self.expanded_groups.contains(&group.id);
        let child_count = self.child_groups(&group.id).len()
            + self.connections_in_group(&group.id, cx).len();

        let click_group_id = group_id.clone();
        let menu_group_id = group_id.clone();
        let drop_group_id = group_id.clone();

        let mut group_el = div()
            .flex()
            .flex_col()
            .child(
                // Group header row
                div()
                    .id(format!("group-{}", &group.id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .pl(px(depth as f32 * 16.0))
                    .rounded_md()
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .child(
                        // Expand/collapse chevron
                        if is_expanded {
                            icon(AppIcon::ChevronDown)
                                .text_color(cx.theme().muted_foreground)
                        } else {
                            icon(AppIcon::ChevronRight)
                                .text_color(cx.theme().muted_foreground)
                        },
                    )
                    .child(
                        icon(AppIcon::Folder)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(group.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("({})", child_count)),
                    )
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, _w, cx| {
                        this.toggle_group(&click_group_id, cx);
                    }))
                    .drag_over::<DragConnection>(|style, _drag, _window, cx| {
                        style.bg(cx.theme().list_active)
                    })
                    .on_drop(cx.listener(move |this, drag: &DragConnection, _window, cx| {
                        this.move_connection_to_group(drag.ix, Some(drop_group_id.clone()), cx);
                    }))
                    .context_menu(move |menu, _window, _cx| {
                        menu.menu(
                            "新建连接",
                            Box::new(NewConnectionInGroup {
                                group_id: menu_group_id.clone(),
                            }),
                        )
                        .menu(
                            "新建子文件夹",
                            Box::new(NewSubfolder {
                                parent_id: menu_group_id.clone(),
                            }),
                        )
                        .separator()
                        .menu(
                            "重命名文件夹",
                            Box::new(RenameGroup {
                                group_id: menu_group_id.clone(),
                            }),
                        )
                        .menu(
                            "删除文件夹",
                            Box::new(DeleteGroup {
                                group_id: menu_group_id.clone(),
                            }),
                        )
                    }),
            );

        // Render children if expanded
        if is_expanded {
            // Child groups
            for child_group in self.child_groups(&group.id) {
                group_el = group_el.child(self.render_group(child_group, depth + 1, cx));
            }
            // Connections in this group
            let mut conns = self.connections_in_group(&group.id, cx);
            self.sort_connections(&mut conns);
            for (ix, conn) in conns {
                group_el = group_el.child(self.render_connection(ix, conn, depth + 1, cx));
            }
        }

        group_el
    }

    /// Render a single connection row.
    fn render_connection(
        &self,
        ix: usize,
        conn: &SavedConnection,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let conn_icon = conn.resolve_icon();
        let name = conn.display_name();
        let name_for_drag = name.clone();
        let subtitle = conn.subtitle();

        // Action buttons (visible on hover): Edit + Delete.
        let action_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .opacity(0.3)
            .child(
                // Edit
                div()
                    .id(("conn-edit", ix))
                    .p(px(4.0))
                    .rounded_sm()
                    .hover(|s| s.bg(cx.theme().primary).text_color(cx.theme().primary_foreground))
                    .child(
                        icon(AppIcon::Pencil)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                        this.open_new_connection_window(None, Some(ix), window, cx);
                    })),
            )
            .child(
                // Delete
                div()
                    .id(("conn-del", ix))
                    .p(px(4.0))
                    .rounded_sm()
                    .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                    .child(
                        icon(AppIcon::Delete)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                        this.confirm_delete_connection(ix, window, cx);
                    })),
            );

        // The clickable part (opens the connection). One block for all four
        // connection types now — `open_event` dispatches on `conn_type`, so
        // this doesn't need per-type branching the way it used to.
        let clickable = {
            let conn_for_click = conn.clone();
            div()
                .id(("conn", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .flex_1()
                .child(icon(conn_icon).text_color(cx.theme().foreground))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(subtitle),
                        ),
                )
                .on_click(cx.listener(move |_this, ev: &ClickEvent, _w, cx| {
                    if ev.click_count() >= 2 {
                        cx.emit(open_event(&conn_for_click));
                    }
                }))
        };

        let drag_name: SharedString = name_for_drag.into();

        // Build the row
        div()
            .id(("conn-row", ix))
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .py_1()
            .pl(px(depth as f32 * 16.0 + 8.0))
            .rounded_md()
            .hover(|s| s.bg(cx.theme().list_hover))
            .tooltip({
                let tooltip_lines = conn.tooltip_lines();
                move |window, cx| {
                    let tooltip_lines = tooltip_lines.clone();
                    Tooltip::element(move |_window, cx| {
                        let mut grid = div().flex().flex_col().gap_1().p_2();
                        for (label, value) in &tooltip_lines {
                            grid = grid.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_3()
                                    .child(
                                        div()
                                            .min_w(px(72.0))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(label.clone()),
                                    )
                                    .child(div().child(value.clone())),
                            );
                        }
                        grid
                    })
                    .build(window, cx)
                }
            })
            .child(clickable)
            .child(action_bar)
            .on_drag(DragConnection { ix, name: drag_name }, |drag, _offset, _window, cx| {
                cx.new(|_| drag.clone())
            })
            .drag_over::<DragConnection>(|style, _drag, _window, cx| {
                style.bg(cx.theme().list_active)
            })
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DragConnection>, _window, _cx| {
                if event.bounds.contains(&event.event.position) {
                    let insert_before = event.event.position.y < event.bounds.center().y;
                    this.drag_reorder_target = Some((ix, insert_before));
                } else if this.drag_reorder_target.is_some_and(|(target, _)| target == ix) {
                    this.drag_reorder_target = None;
                }
            }))
            .on_drop(cx.listener(move |this, drag: &DragConnection, _window, cx| {
                let insert_before = this
                    .drag_reorder_target
                    .filter(|(target, _)| *target == ix)
                    .map(|(_, before)| before)
                    .unwrap_or(true);
                this.drag_reorder_target = None;
                this.reorder_connection(drag.ix, ix, insert_before, cx);
            }))
            .context_menu(move |menu, _window, _cx| {
                menu.menu("打开", Box::new(OpenConnection { ix }))
                    .menu("编辑", Box::new(EditConnection { ix }))
                    .menu("复制", Box::new(DuplicateConnection { ix }))
                    .separator()
                    .menu("删除", Box::new(DeleteConnection { ix }))
            })
    }

    /// Render the new-folder / rename-folder form.
    fn render_folder_form(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let save_label = match self.folder_form_target.as_ref()? {
            FolderFormTarget::New(_) => "创建",
            FolderFormTarget::Rename(_) => "保存",
        };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .m_2()
                .p_2()
                .rounded_md()
                .bg(cx.theme().secondary)
                .child(Input::new(&self.new_folder_name).flex_1())
                .child(
                    div()
                        .id("folder-cancel")
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .hover(|s| s.bg(cx.theme().accent))
                        .child("取消")
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.folder_form_target = None;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("folder-save")
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                        .child(save_label)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.create_folder(cx)
                        })),
                ),
        )
    }

    /// Render the empty state.
    fn render_empty(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.connections.is_empty() {
            Some(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("暂无保存的连接,点 + 新增"),
            )
        } else {
            None
        }
    }
}

impl Render for SavedConnectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .text_sm()
            .on_action(cx.listener(Self::on_action_open_connection))
            .on_action(cx.listener(Self::on_action_edit_connection))
            .on_action(cx.listener(Self::on_action_duplicate_connection))
            .on_action(cx.listener(Self::on_action_delete_connection))
            .on_action(cx.listener(Self::on_action_new_connection_in_group))
            .on_action(cx.listener(Self::on_action_new_subfolder))
            .on_action(cx.listener(Self::on_action_rename_group))
            .on_action(cx.listener(Self::on_action_delete_group))
            .on_action(cx.listener(Self::on_action_new_root_connection))
            .on_action(cx.listener(Self::on_action_new_root_folder))
            .child(self.render_header(cx))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_1()
                    .py_1()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .child(self.render_tree(cx))
                    .child(self.render_blank_area())
                    // Fallback ungroup drop target: any drop that isn't
                    // consumed by a more specific folder-header drop target
                    // (see `render_group`) bubbles up to here and ungroups
                    // the dragged connection. Covers the whole scroll body,
                    // not just the (possibly tiny) ungrouped-section strip,
                    // so dragging out of a folder is easy to hit.
                    .drag_over::<DragConnection>(|style, _drag, _window, cx| {
                        style.bg(cx.theme().list_active)
                    })
                    .on_drop(cx.listener(|this, drag: &DragConnection, _window, cx| {
                        this.move_connection_to_group(drag.ix, None, cx);
                    })),
            )
            .children(self.render_folder_form(cx))
            .children(self.render_empty(cx))
    }
}
