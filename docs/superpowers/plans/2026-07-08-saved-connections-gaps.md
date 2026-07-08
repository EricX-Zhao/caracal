# Saved Connections Gaps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the three remaining "已保存的连接" gaps from the roadmap: in-group drag reorder, a hover detail card, and TOML import/export — on top of the existing group tree / drag-into-group / cycling sort foundation.

**Architecture:** A new `SavedConnection.sort_order: i32` field (mirroring the existing `SavedConnectionGroup.sort_order`) becomes the single source of truth for manual ordering within a `group_id` scope. `SortMode::Default` reads it; drag-reorder writes it via a renumber-the-whole-scope algorithm. The hover card reuses the already-written `tooltip_lines()` through `gpui-component`'s `Tooltip::element`. Import/export round-trips the existing `AppConfig` TOML shape through native file dialogs (`cx.prompt_for_new_path` / `cx.prompt_for_paths`), wired into a new dropdown menu on the existing "更多" toolbar button.

**Tech Stack:** Rust, GPUI (git `gpui`/`gpui_platform`, Apache-2.0), `gpui-component` (git, longbridge fork), `serde`/`toml` (already a dependency via `config.rs`).

## Global Constraints

- No new crate dependencies — everything needed (TOML serde, native file dialogs, tooltip custom content, drag-move bounds) already exists in the current dependency set.
- Follow the existing `#[serde(default)]` backward-compat convention for every new persisted field (see `src/config.rs`'s existing fields) — old `connections.toml`/imported files without the field must still parse.
- No unit tests for GPUI rendering/drag visuals/dropdown wiring — matches the existing convention across `src/panels/*.rs` (zero tests in `saved_connections.rs` and `new_connection_window.rs` today). Only `src/config.rs`'s pure-data logic gets tests, matching its existing test module.
- Chinese UI copy for any new user-facing strings (menu items, etc.) — matches all existing strings in `saved_connections.rs` ("新建连接", "更多", "编辑", "删除", ...).
- Build with `cargo build` and run `cargo test` after every task; both must be clean before moving to the next task.

---

### Task 1: `sort_order` field + persistence plumbing

**Files:**
- Modify: `src/config.rs` (`SavedConnection` struct ~line 55-113, `tooltip_lines()` ~line 239, test module ~line 327-490)
- Modify: `src/panels/new_connection_window.rs` (`NewConnectionWindow` struct ~line 38-62, `::new()` ~line 65-183, `save()`'s 4 `SavedConnection{...}` literals ~line 195-319)
- Modify: `src/panels/saved_connections.rs` (`sort_connections` ~line 688-700, `duplicate` ~line 462-481, `move_connection_to_group` ~line 483-496, `open_new_connection_window` ~line 347-387)

**Interfaces:**
- Produces: `SavedConnection.sort_order: i32` (new field, `#[serde(default)]`, zero-value default is correct — "0" is a valid first-position order, no special default function needed).
- Produces: `NewConnectionWindow::new(panel, existing, group_id, new_sort_order: i32, window, cx)` — one new trailing-before-`window` parameter.
- Produces: `SavedConnectionsPanel::sort_connections` now orders `SortMode::Default` by `sort_order` ascending (was: leave `Vec` order alone).
- Consumes: nothing from other tasks (this is the foundation task).

- [ ] **Step 1: Add the `sort_order` field to `SavedConnection`**

In `src/config.rs`, add to the `SavedConnection` struct, immediately after the existing `private_key_passphrase` field (the last field, ~line 112):

```rust
    #[serde(default)]
    pub private_key_passphrase: Option<String>,
    /// Manual ordering within a `group_id` scope (including `None`, the
    /// ungrouped section, which is its own scope). Lower sorts first.
    /// `SortMode::Default` reads this; drag-reorder writes it. New
    /// connections get the count of existing siblings in their scope
    /// (append-to-end), mirroring `SavedConnectionGroup.sort_order`'s
    /// `create_folder` convention.
    #[serde(default)]
    pub sort_order: i32,
```

- [ ] **Step 2: Update `base_connection()` test helper**

In `src/config.rs`'s test module, add `sort_order: 0,` as the last field in `base_connection()` (~line 352, right after `private_key_passphrase: None,`):

```rust
            auth_method: "password".to_string(),
            private_key_path: None,
            private_key_passphrase: None,
            sort_order: 0,
        }
    }
```

- [ ] **Step 3: Write the backward-compat test**

Add to `src/config.rs`'s test module, right after `old_config_without_new_fields_still_deserializes` (~line 489):

```rust
    #[test]
    fn old_config_without_sort_order_still_deserializes() {
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig =
            toml::from_str(toml_text).expect("old-format config must still parse");
        assert_eq!(cfg.connections[0].sort_order, 0);
    }
```

- [ ] **Step 4: Run the new test to verify it passes**

Run: `cargo test --lib config::tests::old_config_without_sort_order_still_deserializes`
Expected: PASS (also confirms the struct compiles with the new field — this will fail to compile, not just fail the assertion, if Step 1/2 are wrong).

- [ ] **Step 5: Drop the now-live `#[allow(dead_code)]` on `tooltip_lines()`**

In `src/config.rs` (~line 239), remove the attribute line — Task 3 wires this method to the UI, but leaving the attribute until then would be a dead-code lint false-negative, not a build error, so it's safe to drop now:

```rust
    /// Lines shown in the tooltip. Each line is (label, value).
    pub fn tooltip_lines(&self) -> Vec<(String, String)> {
```

(Removes the `#[allow(dead_code)]` line that was directly above `pub fn tooltip_lines`.)

- [ ] **Step 6: Run `cargo build` — expect a dead-code warning, not an error**

Run: `cargo build`
Expected: builds successfully; `tooltip_lines` may still warn as unused until Task 3 calls it (warnings don't fail the build in this project). If it errors, re-add the attribute and move on — Task 3 will remove it again once the call site exists.

- [ ] **Step 7: Add `sort_order` field to `NewConnectionWindow` and compute it in `::new()`**

In `src/panels/new_connection_window.rs`, add to the struct (~line 61, after `flow_control: String,`):

```rust
    flow_control: String,
    /// The `sort_order` this connection will be saved with — for edits,
    /// the existing connection's value (position doesn't change on edit);
    /// for new connections, `new_sort_order` as computed by the caller
    /// (`SavedConnectionsPanel::open_new_connection_window`, which has
    /// access to the full connection list this window doesn't).
    sort_order: i32,
```

Change the constructor signature (~line 65-71) to take a new `new_sort_order: i32` parameter, right after `group_id`:

```rust
    pub fn new(
        panel: WeakEntity<SavedConnectionsPanel>,
        existing: Option<(usize, SavedConnection)>,
        group_id: Option<String>,
        new_sort_order: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

In the `Self { ... }` literal (~line 80-85, right after `group_id,`), add:

```rust
            group_id,
            sort_order: conn.as_ref().map(|c| c.sort_order).unwrap_or(new_sort_order),
```

- [ ] **Step 8: Set `sort_order` in all 4 `save()` construction sites**

In `src/panels/new_connection_window.rs`'s `save()` method, add `sort_order: self.sort_order,` right after the `icon,` line in each of the 4 `SavedConnection { ... }` literals (SSH ~line 213, Local ~line 248, Telnet ~line 276, Serial ~line 304):

```rust
                    icon,
                    sort_order: self.sort_order,
```

(Do this once per arm — 4 total edits, one per `conn_type` branch.)

- [ ] **Step 9: Compute `new_sort_order` at the call site and pass it through**

In `src/panels/saved_connections.rs`'s `open_new_connection_window` (~line 347-387), compute the value before building the closure that constructs `NewConnectionWindow`, and pass it in:

```rust
        let existing = edit_ix.and_then(|ix| self.connections.get(ix).map(|c| (ix, c.clone())));
        let new_sort_order = self
            .connections
            .iter()
            .filter(|c| c.group_id == group_id)
            .count() as i32;
        let panel = cx.entity().downgrade();
```

And update the `NewConnectionWindow::new(...)` call inside the `cx.open_window` closure:

```rust
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
```

- [ ] **Step 10: Make `SortMode::Default` sort by `sort_order`**

In `src/panels/saved_connections.rs`'s `sort_connections` (~line 688-700), replace the no-op `Default` arm:

```rust
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
```

- [ ] **Step 11: Give duplicated connections a fresh `sort_order`**

In `src/panels/saved_connections.rs`'s `duplicate` (~line 462-481), assign a fresh append-at-end `sort_order` instead of inheriting the original's (which would tie with it under the new `Default` sort):

```rust
    fn duplicate(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.connections.len() {
            let mut new_conn = self.connections[ix].clone();
            if new_conn.name.is_empty() {
                new_conn.name = format!("copy-of-{}", new_conn.display_name());
            } else {
                new_conn.name = format!("{}-copy", new_conn.name);
            }
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
```

- [ ] **Step 12: Make cross-group moves append at the target group's end**

In `src/panels/saved_connections.rs`'s `move_connection_to_group` (~line 483-496), compute and set a fresh `sort_order` in the destination scope — without this, a connection moved into a group via header/blank-area drop would land at `sort_order` 0 (or whatever it had before) and appear in an arbitrary position instead of at the end, now that `Default` sort actually reads the field:

```rust
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
```

- [ ] **Step 13: Build and test**

Run: `cargo build && cargo test --lib`
Expected: clean build, all tests pass (including the new Step 3 test).

- [ ] **Step 14: Commit**

```bash
git add src/config.rs src/panels/new_connection_window.rs src/panels/saved_connections.rs
git commit -m "feat: add SavedConnection.sort_order, wire through save/duplicate/move/sort"
```

---

### Task 2: In-group drag reorder

**Files:**
- Modify: `src/panels/saved_connections.rs` (imports ~line 36-41, `SavedConnectionsPanel` struct ~line 208-226, `::new()` ~line 229-253, `render_connection` ~line 1035-1147)

**Interfaces:**
- Consumes: `SavedConnection.sort_order: i32` (Task 1). `DragConnection { ix: usize, name: SharedString }` (existing, ~line 125).
- Produces: `SavedConnectionsPanel::reorder_connection(&mut self, dragged_ix: usize, target_ix: usize, insert_before: bool, cx: &mut Context<Self>)` — not consumed by any later task, but this is the deliverable.

- [ ] **Step 1: Add `DragMoveEvent` to the gpui import list**

In `src/panels/saved_connections.rs` (~line 36-41), add `DragMoveEvent` to the existing `use gpui::{...}` block:

```rust
use gpui::{
    Action, App, AppContext, ClickEvent, Context, DragMoveEvent, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    WindowHandle, div, px,
};
```

- [ ] **Step 2: Add a `drag_reorder_target` field to track drop position during a drag**

In the `SavedConnectionsPanel` struct (~line 208-226), add after `new_connection_window`:

```rust
    /// `Some((target_ix, insert_before))` while a `DragConnection` is
    /// hovering over connection row `target_ix` — set by that row's
    /// `on_drag_move`, consumed and cleared by its `on_drop`. `None` when no
    /// drag is in progress or the drag isn't currently over a row.
    drag_reorder_target: Option<(usize, bool)>,
```

In `::new()` (~line 241-253), initialize it in the `Self { ... }` literal, after `new_connection_window: None,`:

```rust
            new_connection_window: None,
            drag_reorder_target: None,
```

- [ ] **Step 3: Add `reorder_connection`**

Add this method to `SavedConnectionsPanel`'s `impl` block, near `move_connection_to_group` (~after line 496, following Task 1's edit to that method):

```rust
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
```

- [ ] **Step 4: Wire `on_drag_move`/`on_drop`/`drag_over` onto each connection row**

In `render_connection` (~line 1035-1147), the row div currently ends with `.on_drag(...)` and `.context_menu(...)` (no drop handling at all — this is the gap). Add drop handling right after the existing `.on_drag(...)` call:

```rust
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
```

(This slots between the existing `.on_drag(...)` block and the existing `.context_menu(...)` block — the rest of `render_connection` is unchanged.)

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: clean build. If `DragMoveEvent<DragConnection>`'s `.bounds.contains(...)` or `.bounds.center()` don't resolve, double check the `gpui::DragMoveEvent` import from Step 1 and that `Bounds<Pixels>::contains`/`center` are in scope via the existing `gpui` glob (they're inherent methods, no trait import needed).

- [ ] **Step 6: Manual smoke test**

Run: `cargo run`, open the app, and verify:
- Drag a connection within the same group to a position above another connection in that group — it should land before that connection.
- Drag a connection within the same group to a position below another connection — it should land after it.
- Drag a connection in the ungrouped section to reorder among other ungrouped connections.
- Switch sort to `NameAsc`/`NameDesc` and back to `Default` (click the sort toggle in the toolbar) — confirm the manually-arranged order survives the round trip (i.e. `sort_order` wasn't corrupted by the alphabetical views).
- Drag a connection onto a folder header (existing cross-group move) still works and the connection appears at the end of that folder's list.

- [ ] **Step 7: Commit**

```bash
git add src/panels/saved_connections.rs
git commit -m "feat: in-group drag reorder for saved connections"
```

---

### Task 3: Hover detail card

**Files:**
- Modify: `src/panels/saved_connections.rs` (imports ~line 42-47, `render_connection` ~line 1035-1147)

**Interfaces:**
- Consumes: `SavedConnection::tooltip_lines(&self) -> Vec<(String, String)>` (already exists in `src/config.rs`, `#[allow(dead_code)]` dropped in Task 1 Step 5).
- Produces: nothing consumed by later tasks — this is a leaf feature.

- [ ] **Step 1: Import `Tooltip`**

In `src/panels/saved_connections.rs` (~line 42-47), add:

```rust
use gpui_component::tooltip::Tooltip;
```

- [ ] **Step 2: Attach a `Tooltip::element` hover card to each connection row**

In `render_connection` (~line 1035-1147), the row is built as:

```rust
        div()
            .id(("conn-row", ix))
            .flex().flex_row().items_center().px_2().py_1()
            .pl(px(depth as f32 * 16.0 + 8.0))
            .rounded_md()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(clickable)
            .child(action_bar)
            .on_drag(...)
            .drag_over::<DragConnection>(...)   // added in Task 2
            .on_drag_move(...)                   // added in Task 2
            .on_drop(...)                        // added in Task 2
            .context_menu(...)
```

Add `.tooltip(...)` right after `.hover(|s| s.bg(cx.theme().list_hover))`:

```rust
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
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 4: Manual smoke test**

Run: `cargo run`, hover over one connection of each type (SSH, Local, Telnet, Serial) long enough for the tooltip delay, and confirm the fields shown match `tooltip_lines()`'s per-type selection (SSH: Host/Port/User; Local: Shell/Working Dir if set; Telnet: Host/Port; Serial: Port/Baud), plus Description when set.

- [ ] **Step 5: Commit**

```bash
git add src/panels/saved_connections.rs
git commit -m "feat: hover detail card for saved connections"
```

---

### Task 4: TOML import/export

**Files:**
- Modify: `src/panels/saved_connections.rs` (imports ~line 36-47, "更多" button ~line 853-866)

**Interfaces:**
- Consumes: `AppConfig { connections: Vec<SavedConnection>, groups: Vec<SavedConnectionGroup> }` and `config::config_path() -> PathBuf` (both already exist in `src/config.rs`, unchanged).
- Produces: `SavedConnectionsPanel::export_connections(&mut self, window: &mut Window, cx: &mut Context<Self>)` and `::import_connections(&mut self, window: &mut Window, cx: &mut Context<Self>)` — not consumed elsewhere, leaf feature.

- [ ] **Step 1: Add imports**

In `src/panels/saved_connections.rs`, add `WeakEntity` to the existing `use gpui::{...}` block (~line 36-41):

```rust
use gpui::{
    Action, App, AppContext, ClickEvent, Context, DragMoveEvent, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window,
    WindowHandle, div, px,
};
```

Add `Button` and the `DropdownMenu`/`PopupMenuItem` menu types (~line 42-47):

```rust
use gpui_component::Root;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, StyledExt, WindowExt};
```

- [ ] **Step 2: Replace the "更多" button with a `Button` that supports `.dropdown_menu(...)`**

`DropdownMenu` (the trait providing `.dropdown_menu(...)`) is only implemented for `gpui_component::button::Button`, not a raw `div()` — so the current plain-`div()` "更多" trigger (~line 853-866) needs to become a `Button`. Replace:

```rust
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
```

with:

```rust
                    .child({
                        let weak = cx.entity().downgrade();
                        Button::new("more-btn")
                            .ghost()
                            .small()
                            .icon(icon(AppIcon::MoreVertical))
                            .dropdown_menu(move |menu, _window, _cx| {
                                let weak_export = weak.clone();
                                let weak_import = weak.clone();
                                menu.item(PopupMenuItem::new("导出配置").on_click(
                                    move |_ev, window, cx| {
                                        let _ = weak_export.update(cx, |this, cx| {
                                            this.export_connections(window, cx);
                                        });
                                    },
                                ))
                                .item(PopupMenuItem::new("导入配置").on_click(
                                    move |_ev, window, cx| {
                                        let _ = weak_import.update(cx, |this, cx| {
                                            this.import_connections(window, cx);
                                        });
                                    },
                                ))
                            })
                    }),
```

- [ ] **Step 3: Implement `export_connections`**

Add to `SavedConnectionsPanel`'s `impl` block (e.g. near `persist`, ~line 703):

```rust
    /// Write the entire current connections + groups list to a
    /// user-chosen TOML file (native "save as" dialog). Does not touch
    /// the app's own `connections.toml` — this is a separate export file.
    fn export_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start_dir = config::config_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&start_dir, Some("connections.toml"));
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            let _ = weak.update(cx, |this, _cx| {
                let export = AppConfig {
                    connections: this.connections.clone(),
                    groups: this.groups.clone(),
                };
                let text = match toml::to_string_pretty(&export) {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("failed to serialize exported connections: {e}");
                        return;
                    }
                };
                if let Err(e) = std::fs::write(&path, text) {
                    log::error!("failed to write exported connections to {path:?}: {e}");
                }
            });
        })
        .detach();
    }

    /// Read a user-chosen TOML file (same shape as `connections.toml`) and
    /// append every connection and group from it to the current lists. No
    /// merge/dedup — `SavedConnection` has no stable id to merge on, so
    /// importing the same file twice produces duplicates (deletable
    /// manually), which is safer than silently dropping data.
    fn import_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("failed to read {path:?}: {e}");
                    return;
                }
            };
            let imported: AppConfig = match toml::from_str(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    log::error!("failed to parse {path:?} as connections TOML: {e}");
                    return;
                }
            };
            let _ = weak.update(cx, |this, cx| {
                this.connections.extend(imported.connections);
                this.groups.extend(imported.groups);
                this.persist();
                cx.notify();
            });
        })
        .detach();
    }
```

- [ ] **Step 4: Add the `PathBuf` import**

`export_connections` uses `PathBuf`. Check whether `std::path::PathBuf` is already imported in `src/panels/saved_connections.rs` — it isn't (the file currently only imports `std::collections::HashSet` and `std::time::{SystemTime, UNIX_EPOCH}`, ~line 33-34). Add it:

```rust
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 6: Manual smoke test**

Run: `cargo run`, then:
- Click "更多" → "导出配置", choose a path, confirm the file is written and `toml` fields match the current connections/groups (open it in a text editor).
- Click "更多" → "导入配置", pick that same exported file — confirm all connections/groups are appended (list grows, nothing is replaced).
- Import the same file a second time — confirm duplicates appear (not a crash, not silent data loss).

- [ ] **Step 7: Commit**

```bash
git add src/panels/saved_connections.rs
git commit -m "feat: TOML import/export for saved connections"
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
Expected: no new warnings in `src/config.rs`, `src/panels/saved_connections.rs`, or `src/panels/new_connection_window.rs` (the only files this plan touches) compared to `main`. Pre-existing warnings elsewhere in the codebase are out of scope.

- [ ] **Step 3: End-to-end manual smoke test**

Run: `cargo run` and walk through the full spec's smoke-test list in one pass:
- Drag-reorder within a group; drag-reorder in the ungrouped section.
- Confirm `NameAsc`/`NameDesc` still work and round-tripping back to `Default` doesn't corrupt the manually-arranged order.
- Hover a connection of each of the 4 types, confirm the right fields show in the tooltip.
- Export to a file, inspect its contents, re-import it (confirm append, not replace), import a second time (confirm duplicates, not data loss or a crash).
- Drag a connection into a different group via the folder header — confirm it lands at the end of that group's list.

- [ ] **Step 4: No commit needed for this task** — it's verification only. If Step 2/3 surface a bug, fix it in the relevant task's file, re-run that task's own build/test steps, then re-run this task's steps.
