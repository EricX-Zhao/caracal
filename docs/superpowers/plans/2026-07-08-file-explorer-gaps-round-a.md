# File Explorer Gaps Round A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a right-click context menu, rename, and a properties dialog to caracal's SFTP file browser (`SftpPanel`).

**Architecture:** `FileTableDelegate` (the `DataTable`'s data/rendering delegate) gains a `WeakEntity<SftpPanel>` so its new `TableDelegate::context_menu` override can route menu-item clicks back to `SftpPanel` methods — three of the six menu items (打开/下载/删除) reuse existing panel methods directly, the other three (重命名/属性/复制路径) are new. Rename reuses the existing `pending_op` inline-banner mechanism (already used by 新建文件/新建文件夹). Properties is an `AlertDialog` with a custom key/value grid, built entirely from data the table already has (no new SFTP round-trip). A new `SftpRequest::Rename` variant on the SSH session thread backs the rename call.

**Tech Stack:** Rust, GPUI (git `gpui`/`gpui_platform`), `gpui-component` (git), `russh-sftp` 2.3.0 (already a dependency).

## Global Constraints

- No new crate dependencies — `russh_sftp::client::session::SftpSession::rename` already exists.
- No cross-directory move — rename only rewrites the filename within the current directory (`self.path`); see spec's Decisions.
- No owner/group in the properties dialog — display-only, built from the already-fetched `SftpEntry` (name/is_dir/size/mtime/perms), no new `stat()` call.
- No unit tests for GPUI rendering/dialog/context-menu wiring, and no unit tests for the new `SftpRequest::Rename` backend path — matches the existing zero-test convention for `panels/*.rs` and for the other `SftpRequest` variants (`sftp_remove`/`sftp_download`/etc. have none either).
- Chinese UI copy for all new user-facing strings: exactly 打开, 下载, 重命名, 属性, 复制路径, 删除 for the six menu items; 名称/路径/类型/大小/修改时间/权限 for the properties grid's labels; 文件/文件夹 for the type value; 重命名失败: {e} for the rename failure status message (mirrors the existing 删除失败: {e} pattern).
- Build with `cargo build` and run `cargo test` after every task; both must be clean before moving to the next task.

---

### Task 1: Backend — `SftpRequest::Rename`

**Files:**
- Modify: `src/terminal/ssh.rs` (`SftpEntry`/`SftpRequest` region ~line 59-129, `SshSession` impl ~line 384-393, `service_sftp` ~line 596-826, `reply_sftp_error` ~line 893-914)

**Interfaces:**
- Produces: `SshSession::sftp_rename(&self, old: String, new: String) -> flume::Receiver<Result<()>>` — consumed by Task 2.
- Consumes: nothing from other tasks (foundation task).

- [ ] **Step 1: Add the `Rename` variant to `SftpRequest`**

In `src/terminal/ssh.rs`, add to the `SftpRequest` enum, right after the existing `Remove { ... }` variant (~line 128, the enum's last variant):

```rust
    /// Remove a file or directory. If `recursive` is true and `path` is a
    /// directory, walks and removes all descendants.
    Remove {
        path: String,
        recursive: bool,
        reply: flume::Sender<Result<()>>,
    },
    /// Rename (or move within the same directory — this round's UI only
    /// rewrites the filename, never the directory) a remote path.
    Rename {
        old: String,
        new: String,
        reply: flume::Sender<Result<()>>,
    },
}
```

- [ ] **Step 2: Add `SshSession::sftp_rename`**

In `src/terminal/ssh.rs`, add right after `sftp_remove` (~line 392, immediately after its closing `}` but still inside the `impl SshSession` block):

```rust
    /// Rename `old` to `new` (SFTP `rename`). This round's UI only calls
    /// this with `old`/`new` sharing the same parent directory (rename, not
    /// move) — the backend itself doesn't enforce that.
    pub fn sftp_rename(&self, old: String, new: String) -> flume::Receiver<Result<()>> {
        let (reply, rx) = flume::bounded(1);
        let _ = self.cmd_tx.send(SessionCmd::Sftp(SftpRequest::Rename {
            old,
            new,
            reply,
        }));
        rx
    }
```

- [ ] **Step 3: Handle `SftpRequest::Rename` in `service_sftp`**

In `src/terminal/ssh.rs`'s `service_sftp` function, add a new match arm right after the existing `SftpRequest::Remove { ... }` arm (~line 824, immediately before the `match request`'s closing `}`):

```rust
        SftpRequest::Rename { old, new, reply } => {
            let sftp = match clone_sftp(sftp_slot).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Err(e));
                    return;
                }
            };
            log::info!("sftp: rename {old:?} -> {new:?}");
            let result = sftp
                .rename(old.clone(), new.clone())
                .await
                .map_err(|e| anyhow!("rename {old:?} -> {new:?}: {e}"));
            if let Err(ref e) = result {
                log::error!("sftp: rename {old:?} -> {new:?} failed: {e:#}");
            }
            let _ = reply.send(result);
        }
```

- [ ] **Step 4: Handle `SftpRequest::Rename` in `reply_sftp_error`**

In `src/terminal/ssh.rs`'s `reply_sftp_error` function, extend the existing combined arm that handles `Mkdir`/`CreateFile`/`Remove` (~line 908-912) to also cover `Rename`:

```rust
        SftpRequest::Mkdir { reply, .. }
        | SftpRequest::CreateFile { reply, .. }
        | SftpRequest::Remove { reply, .. }
        | SftpRequest::Rename { reply, .. } => {
            let _ = reply.send(Err(err));
        }
```

- [ ] **Step 5: Build and test**

Run: `cargo build && cargo test --lib`
Expected: clean build (a `match` on `SftpRequest` that's missing a variant is a compile error in Rust, so both `service_sftp` and `reply_sftp_error` must be exhaustively updated for this to build at all — this step is the actual verification that Steps 3-4 were both done), all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/terminal/ssh.rs
git commit -m "feat: add SftpRequest::Rename backend"
```

---

### Task 2: Rename UI (inline banner)

**Files:**
- Modify: `src/panels/sftp.rs` (`PendingOpKind` ~line 88-91, `render_pending_op_row` ~line 899-925, `commit_pending_op` ~line 585-624, new `rename_entry` method near `new_folder` ~line 573-584)

**Interfaces:**
- Consumes: `SshSession::sftp_rename(&self, old: String, new: String) -> flume::Receiver<Result<()>>` (Task 1).
- Produces: `SftpPanel::rename_entry(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>)` — consumed by Task 4 (context menu). Marked `#[allow(dead_code)]` in this task since nothing calls it yet; Task 4 removes that attribute.

- [ ] **Step 1: Add the `Rename` variant to `PendingOpKind`**

In `src/panels/sftp.rs`, change (~line 87-91):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingOpKind {
    NewFile,
    NewFolder,
    /// Renaming the entry at this row index (in `FileTableDelegate::entries`).
    Rename(usize),
}
```

- [ ] **Step 2: Update `render_pending_op_row`'s label for the new variant**

In `src/panels/sftp.rs`'s `render_pending_op_row` (~line 899-925), the label match currently only covers `NewFile`/`NewFolder` and doesn't have access to the entry's old name. Change the function to compute the label from `self` (it currently only takes `cx`, add access via `self.table_state`):

```rust
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
```

- [ ] **Step 3: Add `rename_entry`**

In `src/panels/sftp.rs`, add right after `new_folder` (~line 584, before `commit_pending_op`):

```rust
    #[allow(dead_code)] // wired to the context menu in Task 4
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
```

- [ ] **Step 4: Add the `Rename` arm to `commit_pending_op`**

In `src/panels/sftp.rs`'s `commit_pending_op` (~line 585-624), add a new match arm after the existing `PendingOpKind::NewFolder` arm, right before the `match kind`'s closing `}`:

```rust
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
```

Note: `remote` (built from the typed new name, same directory as `self.path`) and `session` are already computed above the `match kind` block by the existing code — this arm reuses both, matching how `NewFile`/`NewFolder` already do.

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: clean build (one expected `dead_code` warning-suppression via the `#[allow(dead_code)]` on `rename_entry` — no warning should actually print for it because of that attribute; no other new warnings).

- [ ] **Step 6: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: rename via the existing inline pending-op banner"
```

---

### Task 3: Properties dialog

**Files:**
- Modify: `src/panels/sftp.rs` (new `show_properties` method near `copy_path` ~line 760, new `properties_row` free function near `human_perms` ~line 1315-1325)

**Interfaces:**
- Consumes: `SftpEntry { name, is_dir, size, mtime, perms }` (existing, `terminal/ssh.rs`), `human_size`/`human_mtime`/`human_perms` (existing, `sftp.rs`), `remote_join` (existing, `sftp.rs`).
- Produces: `SftpPanel::show_properties(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>)` — consumed by Task 4. Marked `#[allow(dead_code)]` in this task since nothing calls it yet; Task 4 removes that attribute.

- [ ] **Step 1: Add the `properties_row` helper**

In `src/panels/sftp.rs`, add right after `human_perms` (~line 1325, before `human_mtime`):

```rust
/// One label/value row in the properties dialog's key/value grid.
#[allow(dead_code)] // called from show_properties, wired to the context menu in Task 4
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
```

- [ ] **Step 2: Add `show_properties`**

In `src/panels/sftp.rs`, add right after `copy_path` (~line 762, still inside the same `impl SftpPanel` block that ends at line 763):

```rust
    #[allow(dead_code)] // wired to the context menu in Task 4
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
```

- [ ] **Step 3: Build**

No new import is needed: `window.open_alert_dialog(cx, move |alert, _window, cx| ...)`'s closure parameter type is inferred from `WindowExt::open_alert_dialog`'s signature (`F: Fn(AlertDialog, &mut Window, &mut App) -> AlertDialog`), so calling `.title(...)`/`.description(...)` on `alert` resolves without `AlertDialog` needing to be a named import — the existing `delete_selected` (`sftp.rs:649-664`) uses this identical shape today without importing `AlertDialog` by name.

Run: `cargo build`
Expected: clean build. Both `#[allow(dead_code)]` attributes (on `show_properties` from Step 2 and on `properties_row` from Step 1) suppress what would otherwise be two unused-code warnings — `properties_row` needs its own attribute even though it's called from `show_properties`, because `show_properties` itself isn't reachable from any real entry point yet, so rustc's reachability analysis doesn't count that call as "used" either.

- [ ] **Step 4: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: add properties dialog for SFTP entries"
```

---

### Task 4: Context menu wiring

**Files:**
- Modify: `src/panels/sftp.rs` (imports ~line 29-40, `FileTableDelegate` struct + `::new` ~line 94-111, `impl TableDelegate for FileTableDelegate` ~line 113-219, `SftpPanel::new` ~line 239-294, new `copy_entry_path` method near `copy_path` ~line 762)

**Interfaces:**
- Consumes: `SftpPanel::rename_entry(&mut self, ix, window, cx)` (Task 2), `SftpPanel::show_properties(&mut self, ix, window, cx)` (Task 3), `SftpPanel::download_selected(&mut self, window, cx)`, `SftpPanel::delete_selected(&mut self, window, cx)`, `SftpPanel::enter_dir(&mut self, name: &str, window, cx)`, `SftpPanel::download(&mut self, name: &str, cx)` (all pre-existing).
- Produces: `FileTableDelegate::new(panel: WeakEntity<SftpPanel>) -> Self` (signature change — was `FileTableDelegate::new()`), `SftpPanel::copy_entry_path(&mut self, ix: usize, cx: &mut Context<Self>)`. Nothing consumed by later tasks — this is the integration point that ties Tasks 1-3 together.

- [ ] **Step 1: Add `WeakEntity` and menu imports**

In `src/panels/sftp.rs`'s `use gpui::{...}` block (~line 29-33), add `WeakEntity`:

```rust
use gpui::{
    App, AppContext, AsyncApp, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div,
    prelude::FluentBuilder, px,
};
```

Add the menu types (~line 34-40, alongside the existing `gpui_component` imports):

```rust
use gpui_component::menu::{PopupMenu, PopupMenuItem};
```

- [ ] **Step 2: Give `FileTableDelegate` a `WeakEntity<SftpPanel>`**

In `src/panels/sftp.rs`, change the struct and constructor (~line 94-111):

```rust
/// Delegate that feeds the `DataTable` from the panel's `entries` vec.
struct FileTableDelegate {
    entries: Vec<SftpEntry>,
    columns: Vec<Column>,
    panel: WeakEntity<SftpPanel>,
}

impl FileTableDelegate {
    fn new(panel: WeakEntity<SftpPanel>) -> Self {
        Self {
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
}
```

- [ ] **Step 3: Override `TableDelegate::context_menu`**

In `src/panels/sftp.rs`'s `impl TableDelegate for FileTableDelegate` block (~line 113-219), add this method (position doesn't matter within the `impl` block; add it after `columns_count`/`rows_count`/`column`, before `render_td`, ~line 125):

```rust
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
```

- [ ] **Step 4: Add `copy_entry_path`**

In `src/panels/sftp.rs`, add right after `copy_path` (~line 762, inside the same `impl SftpPanel` block):

```rust
    fn copy_entry_path(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(name) = self.table_state.read(cx).delegate().entries.get(ix).map(|e| e.name.clone())
        else {
            return;
        };
        let remote = remote_join(&self.path, &name);
        cx.write_to_clipboard(ClipboardItem::new_string(remote));
    }
```

- [ ] **Step 5: Remove the `#[allow(dead_code)]` attributes from Tasks 2 and 3**

In `src/panels/sftp.rs`, remove the `#[allow(dead_code)] // wired to the context menu in Task 4` line immediately above both `fn rename_entry(...)` and `fn show_properties(...)` (and above `fn properties_row(...)` if Task 3 Step 4 added one there). They're both genuinely called now, from Step 3 above.

- [ ] **Step 6: Wire `WeakEntity<SftpPanel>` into `FileTableDelegate::new`'s call site**

In `src/panels/sftp.rs`'s `SftpPanel::new` (~line 239-294), change:

```rust
        let download_dir = download_default_dir();
        let delegate = FileTableDelegate::new();
```

to:

```rust
        let download_dir = download_default_dir();
        let panel = cx.entity().downgrade();
        let delegate = FileTableDelegate::new(panel);
```

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: clean build, no warnings introduced by this task (the `#[allow(dead_code)]` removals in Step 5 must not produce new dead-code warnings — if they do, Step 3's wiring is incomplete; re-check that all six `PopupMenuItem`s in `context_menu` are present and that `copy_entry_path`/`rename_entry`/`show_properties` are all referenced from it).

- [ ] **Step 8: Manual smoke test**

Run: `cargo run`, connect to an SSH host, open its SFTP panel, and verify:
- Right-click a file: menu shows all 6 items (打开/下载/重命名/属性/复制路径/删除).
- Right-click a folder: same 6 items.
- 打开 on a folder enters it; 打开 on a file starts a download (same as double-click).
- 下载 on a folder shows the existing "暂不支持下载文件夹" warning; 下载 on a file downloads it.
- 重命名 on a file shows the inline banner pre-filled with its current name; press Enter with a new name — the row updates without a full list flicker/refresh.
- 重命名 with an empty name silently does nothing (banner just closes).
- 重命名 to a name that already exists in the directory — confirm the panel shows a "重命名失败: ..." status message rather than crashing or silently succeeding.
- 属性 on a file and a folder — confirm 名称/路径/类型/大小/修改时间/权限 all render with plausible values, and 大小 shows "—" for folders (matching the existing size column's own folder behavior).
- 复制路径 from the context menu, then paste — confirm it's the specific entry's path (one segment longer than the toolbar's own 复制路径, which copies the current directory).
- 删除 from the context menu shows the existing delete-confirmation dialog and works as before.

- [ ] **Step 9: Commit**

```bash
git add src/panels/sftp.rs
git commit -m "feat: wire context menu into SftpPanel (open/download/rename/properties/copy-path/delete)"
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
Expected: no new warnings in `src/terminal/ssh.rs` or `src/panels/sftp.rs` (the only files this plan touches) compared to `main`. Pre-existing warnings elsewhere are out of scope.

- [ ] **Step 3: End-to-end manual smoke test**

Run: `cargo run` and walk through Task 4 Step 8's full checklist in one pass, plus: confirm the existing toolbar actions (new file/folder/upload/download/delete/refresh, multi-select download/delete) still work unchanged — this task touched `FileTableDelegate::new`'s signature and the `impl TableDelegate` block, so a regression there would silently break the whole file browser, not just the new context menu.

- [ ] **Step 4: No commit needed for this task** — it's verification only. If Step 2/3 surface a bug, fix it in the relevant task's file, re-run that task's own build/test steps, then re-run this task's steps.
