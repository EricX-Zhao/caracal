# Saved Connections panel: double-click, context menus, drag-and-drop, edit/delete actions

Date: 2026-07-03
File under change: `src/panels/saved_connections.rs`

## Background

`SavedConnectionsPanel` renders the persisted connections/groups as a nyaterm-style
tree ([nyaterm reference](https://github.com/nyakang/nyaterm/tree/main),
`src/components/panel/saved-connections/`). Four gaps vs. that reference, reported
by the user:

1. A single left-click opens a connection immediately; it should require a
   double-click.
2. Connections and group ("folder") rows have no right-click context menu.
3. Connections cannot be drag-and-dropped into a folder.
4. The per-row hover actions are Copy/Rename/Delete (3 icons); this should become
   just Edit/Delete (2 icons), with Edit opening a full edit form.

No changes to the persisted data model (`src/config.rs`) are needed — `group_id`
and `parent_id` already exist and support everything below.

## Decisions (confirmed with user)

- **Edit** opens the full connection form (host/port/user/password/shell/etc.),
  pre-filled, saving in place. This replaces the existing inline-rename flow,
  which is dead code today (`editing_id` is set by `start_rename` but never read
  by any renderer — inline rename currently has no visible effect).
- **Delete** (from hover icon or context menu, for both connections and groups)
  shows a confirmation dialog via `window.open_alert_dialog`, matching the
  existing pattern in `src/panels/sftp.rs:649-666`.
- The folder context menu is full-featured: New Connection (preset to that
  group), New Subfolder (preset `parent_id`), Rename Folder, Delete Folder.
- Drag-and-drop supports both directions: dragging a connection onto a folder
  sets its `group_id`; dragging it onto the ungrouped-connections area clears
  `group_id` back to `None`.

## 1. Double-click to open

`ClickEvent::click_count()` is already used elsewhere in this codebase for the
same purpose (`src/terminal/selection.rs:37`, `selection_type_for_click`).
Apply the same check in the connection row's `on_click`:

```rust
.on_click(cx.listener(move |_this, ev: &ClickEvent, _w, cx| {
    if ev.click_count() >= 2 {
        cx.emit(SavedConnectionsEvent::Open(spec.clone())); // or OpenLocal
    }
}))
```

Single click has no effect (no row-selection feature is in scope).

## 2. Right-click context menus

Uses gpui-component's `ContextMenuExt::context_menu` (extension trait on any
`InteractiveElement + ParentElement + Styled`, `crates/ui/src/menu/context_menu.rs:18`),
which shows a `PopupMenu` at the mouse position on right mouse-down — no manual
`MouseButton::Right` handling required. Menu items are `Box<dyn Action>`
(`PopupMenu::menu(label, Box<dyn Action>)`, `crates/ui/src/menu/popup_menu.rs:392`),
so each item dispatches a `gpui::Action`.

### New actions

Field-carrying actions with `#[action(no_json)]` to skip the
`JsonSchema`/keybinding-JSON machinery (confirmed pattern:
`crates/ui/src/actions.rs:4-9`, `Confirm { secondary: bool }`):

```rust
use gpui::Action;
use serde::Deserialize;

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct OpenConnection { ix: usize }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct EditConnection { ix: usize }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct DuplicateConnection { ix: usize }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct DeleteConnection { ix: usize }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct NewConnectionInGroup { group_id: String }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct NewSubfolder { parent_id: String }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct RenameGroup { group_id: String }

#[derive(Clone, PartialEq, Action, Deserialize)]
#[action(namespace = saved_connections, no_json)]
struct DeleteGroup { group_id: String }
```

The panel's root `div` (in `Render::render`) registers one `.on_action(cx.listener(...))`
per action, following `crates/story/src/stories/tree_story.rs:203-205`.

### Menus

Connection row (built fresh per right-click, capturing `ix`):

```rust
.context_menu(move |menu, _window, _cx| {
    menu.menu("打开", Box::new(OpenConnection { ix }))
        .menu("编辑", Box::new(EditConnection { ix }))
        .menu("复制", Box::new(DuplicateConnection { ix }))
        .separator()
        .menu("删除", Box::new(DeleteConnection { ix }))
})
```

Group header row (capturing `group_id`):

```rust
.context_menu(move |menu, _window, _cx| {
    menu.menu("新建连接", Box::new(NewConnectionInGroup { group_id: gid.clone() }))
        .menu("新建子文件夹", Box::new(NewSubfolder { parent_id: gid.clone() }))
        .separator()
        .menu("重命名文件夹", Box::new(RenameGroup { group_id: gid.clone() }))
        .menu("删除文件夹", Box::new(DeleteGroup { group_id: gid.clone() }))
})
```

### Handlers (all `fn(&mut self, action: &X, window: &mut Window, cx: &mut Context<Self>)`)

- `on_action_open_connection` — same emit logic as double-click.
- `on_action_edit_connection` — calls `start_edit(ix, window, cx)` (§4).
- `on_action_duplicate_connection` — calls existing `duplicate(ix, cx)`.
- `on_action_delete_connection` — calls `confirm_delete_connection(ix, window, cx)` (§4).
- `on_action_new_connection_in_group` — calls `toggle_form` variant that presets `form.group_id`.
- `on_action_new_subfolder` — opens folder form with `FolderFormTarget::New(Some(parent_id))`.
- `on_action_rename_group` — opens folder form with `FolderFormTarget::Rename(group_id)`, prefilled with the current name.
- `on_action_delete_group` — confirms via alert dialog, then calls existing `delete_group` (drops its `#[allow(dead_code)]`, since it's now reachable).

## 3. Drag and drop

No dedicated tree drag-drop helper in gpui-component; use raw `gpui::div` methods
(`crates/gpui/src/elements/div.rs`): `on_drag`, `drag_over::<T>()`, `on_drop::<T>()`.
Pattern to follow: gpui-component's own dock tab reordering
(`crates/ui/src/dock/tab_panel.rs:36-65,774-794`, `DragPanel`).

```rust
#[derive(Clone)]
struct DragConnection {
    ix: usize,
    name: SharedString,
}

impl Render for DragConnection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2().py_1().rounded_md()
            .bg(cx.theme().accent)
            .text_sm()
            .child(self.name.clone())
    }
}
```

Connection row:

```rust
.on_drag(DragConnection { ix, name: name.clone().into() }, |drag, _offset, _window, cx| {
    cx.new(|_| drag.clone())
})
```

Group header row (drop target — sets `group_id`):

```rust
.drag_over::<DragConnection>(|style, _drag, _window, cx| {
    style.border_l_2().border_color(cx.theme().drag_border)
})
.on_drop(cx.listener(move |this, drag: &DragConnection, _window, cx| {
    this.move_connection_to_group(drag.ix, Some(group_id.clone()), cx);
}))
```

Ungrouped-connections section container (drop target — clears `group_id`):
same `.drag_over::<DragConnection>()` + `.on_drop` calling
`move_connection_to_group(drag.ix, None, cx)`.

New method:

```rust
fn move_connection_to_group(&mut self, ix: usize, group_id: Option<String>, cx: &mut Context<Self>) {
    if let Some(conn) = self.connections.get_mut(ix) {
        conn.group_id = group_id;
        self.persist();
        cx.notify();
    }
}
```

Groups themselves are not draggable (out of scope — only connections move).

## 4. Hover icons: Edit + Delete (replaces Copy/Rename/Delete)

`action_bar` (currently 3 buttons) becomes 2:

- **Edit** (`AppIcon::Pencil`) → `start_edit(ix, window, cx)`.
- **Delete** (`AppIcon::Delete`) → `confirm_delete_connection(ix, window, cx)`.

Duplicate remains reachable only via the context menu's "复制" item (its handler,
`duplicate`, is unchanged).

### Form becomes edit-capable

`ConnForm` gains `edit_ix: Option<usize>`.

```rust
fn start_edit(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(conn) = self.connections.get(ix) else { return };
    self.form = Some(ConnForm {
        name: cx.new(|cx| InputState::new(window, cx).default_value(&conn.name)),
        conn_type: conn.conn_type.clone(),
        group_id: conn.group_id.clone(),
        host: cx.new(|cx| InputState::new(window, cx).default_value(&conn.host)),
        port: cx.new(|cx| InputState::new(window, cx).default_value(&conn.port.to_string())),
        user: cx.new(|cx| InputState::new(window, cx).default_value(&conn.user)),
        password: cx.new(|cx| InputState::new(window, cx).masked(true).default_value(&conn.password)),
        shell_path: cx.new(|cx| InputState::new(window, cx).default_value(conn.shell_path.as_deref().unwrap_or(""))),
        working_dir: cx.new(|cx| InputState::new(window, cx).default_value(conn.working_dir.as_deref().unwrap_or(""))),
        edit_ix: Some(ix),
    });
    cx.notify();
}
```

`save_form` branches on `form.edit_ix`: `Some(ix)` overwrites
`self.connections[ix]` in place (preserving fields the form doesn't expose,
e.g. `icon`, `description`); `None` pushes a new connection (current behavior).

`toggle_form` (toolbar "新建连接" button) keeps building a form with
`edit_ix: None`. A new small helper builds a form preset with a `group_id`
(for the "新建连接" group-context-menu action) — same shape, also `edit_ix: None`.

### Delete confirmation

```rust
fn confirm_delete_connection(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(conn) = self.connections.get(ix) else { return };
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
            })
    });
}
```

Same shape for `confirm_delete_group(group_id, ...)`, calling `delete_group`.

## Cleanup (dead code removed as part of this change)

- `editing_id: Option<String>` field, `rename_input: Entity<InputState>` field,
  `start_rename`, `save_rename`, `cancel_rename` — the inline-rename UI they
  supported was never wired into any renderer, so removing them is a pure
  dead-code deletion, not a behavior change. Renaming now happens through Edit.
- `delete_group`'s `#[allow(dead_code)]` is removed since the group context
  menu makes it reachable.

## Out of scope

- Groups are not draggable, only connections.
- No multi-select / bulk operations (nyaterm supports this; not requested).
- No "Open All Connections" folder action (nyaterm has it; not requested — can
  be added later following the same `Action`-dispatch pattern).
- No change to `src/config.rs` (data model already sufficient).

## Testing approach

Manual verification only (`superpowers:verify` skill) — this is a GPUI UI panel
with no existing unit tests for interaction wiring; behavior must be observed by
running the app: double-click opens, right-click shows the right menu items on
both connections and folders, drag a connection into a folder and see it
persist across a restart (`connections.toml`), drag it back out to ungroup,
edit a connection and confirm changes persist, delete requires confirmation.
