# Workspace-owned center tabs

## Problem

The center terminal tabs have two owners:

1. `Workspace.tab_panels: Vec<Entity<TerminalPanel>>` — sequence numbers, close cleanup, SSH session teardown.
2. `gpui_component::dock::DockArea` — the visible tab strip, drag-to-reorder, drag-to-split, and a cached `DockItem::Tabs.items: Vec<Arc<dyn PanelView>>` that is append-only.

Those two lists drift. Known defects all come from (2):

- Closing the first-opened tab while others remain leaves its `Arc` in `Tabs.items`, so the PTY / serial `TIOCEXCL` handle stays alive until the whole group is empty (`prune_stale_center` only rebuilds when the group is empty). See the 2026-08-17 serial-reopen-busy work and the later review of `workspace.rs:1472`.
- Dragging a tab to a strip edge (`TabPanel::split_panel`) then closing the original group makes `center_tab_group_is_stale` treat *any* empty child as "the whole center is dead" and `set_center`s an empty split — the other pane vanishes while PTYs keep running (`workspace.rs:1486`).
- Drag-reorder inside the strip calls `on_removed` then `on_added_to`. A whole design (`2026-07-22-drag-reorder-false-close-design.md`) exists only to tell those two apart. `ClosePanel` is a gpui-component dock action, so keymap reload cannot `clear_key_bindings()`.

`tab_panels` is already the list we trust for `"N-"` titles. The dock is a second, worse list that the UI happens to render.

## Goal

The center region is a single tab strip owned by `Workspace`. `tab_panels` is the only strong owner of open `TerminalPanel`s. Closing a tab drops it; the backend (including a serial port lock) is released on that close, not on the next tab open.

## Non-goals

- Removing `gpui-component` from the app. Side panels, `Input`, `DataTable`, dialogs, theme, terminal scrollbar, and context menus stay.
- Drag-a-tab-to-the-edge split panes. The sequence-numbers spec already treated split as out of scope; this migration deletes that path instead of reimplementing it. A future split, if wanted, is a separate spec on top of this list.
- Replacing the left/right side regions or activity bars.
- SSH reconnect swapping SFTP/monitor onto the new session (related lifecycle, different bug).

## Data model

`Workspace` fields after the change:

```rust
tab_panels: Vec<Entity<TerminalPanel>>, // visual order, unique strong owner
active_tab: Option<usize>,              // index into tab_panels; None iff empty
```

Delete:

- `dock_area: Entity<DockArea>`
- `tab_count: usize` (use `tab_panels.len()`)
- `prune_stale_center` / `center_tab_group_is_stale` / `active_tabs_item`
- `TerminalPanel.tab_panel`, `attached`, `on_added_to`, `on_removed`, `impl Panel for TerminalPanel`

Keep:

- `register_tab_panel` / `unregister_tab_panel` / `renumber_tabs` / `reposition_tab_panel` (they already operate on `tab_panels`)
- `ssh_reconnect_configs` / `ssh_tab_numbers` / `handle_ssh_tab_closed` (still keyed by `TerminalView` entity id)
- A small `ssh_tab_n_by_term: HashMap<EntityId, (String, u32)>` so `close_tab` can call `release_ssh_tab_number` without the old `Closed` subscriber capturing `tab_number` at open time.
- `TerminalPanel` as the adapter that embeds `TerminalView`, the scrollbar, and the copy/paste context menu (CLAUDE.md §1: `terminal/view.rs` stays `gpui_component`-free)

`active_tab` invariants:

- Empty list ⇒ `active_tab == None`.
- Non-empty ⇒ `active_tab` is `Some(i)` with `i < tab_panels.len()`.
- Opening a tab appends and sets `active_tab` to the new last index.
- Closing the active tab keeps the same visual slot (the next tab slides in); if it was the last tab, land on the new last. Closing a tab left of the active one decrements `active_tab`. Closing a tab right of it leaves `active_tab` unchanged.

Pure helpers for those rules live in `src/panels/center_tabs.rs` so they can be unit-tested without a `Window`.

## UI

Center of `render_body` (today: `self.dock_area.clone()`) becomes:

```
v_flex
  tab strip     (h_flex, ~32px, overflow-x)
  body          (flex_1: the active TerminalPanel, or empty placeholder)
```

Tab chip, left to right: `"N-"` + `TerminalView::title()` + X. Active chip uses `cx.theme().accent` / `list_active` (same tokens the sessions list already uses for selection). Inactive chips are muted. Clicking the chip (not X) sets `active_tab` and focuses that terminal. Clicking X closes that tab (`cx.stop_propagation()` so it does not also select).

Empty body: muted `Terminal.empty_tabs` copy, no dock chrome.

The strip is not a `DockArea`. It may still use `gpui-component` `ActiveTheme` / `div` hover, like `activity_bar.rs`.

Header `active_title` continues to come from `set_active_title_from` on focus.

## Keyboard

| Action | Binding (unchanged strings) | Handler |
|--------|-----------------------------|---------|
| Next / prev tab | `secondary-tab` / `secondary-shift-tab` | `set_active_tab` on `tab_panels` |
| Goto tab 1–9 | `secondary-1` … `secondary-9` | same `goto_tab_index` math as today |
| Close tab | `secondary-shift-w` | new `CloseTab` on `Workspace`, **not** `gpui_component::dock::ClosePanel` |
| New tab | `secondary-shift-t` | unchanged (`on_new_tab`) |

`CloseTab` closes `tab_panels[*active_tab]`. With no tabs it is a no-op.

`keybindings.rs` maps `"close_tab"` to `CloseTab` instead of `ClosePanel`. `ClosePanel` is no longer referenced.

Next/prev wrap; goto out of range is a no-op. Existing tests on `next_tab_index` / `prev_tab_index` / `goto_tab_index` stay.

## Drag reorder

Same pattern as `SessionsPanel`'s `DragConnection`:

- Payload: `DragTab { ix: usize }`.
- `on_drag` on the chip (not the X).
- `on_drag_move` records `drag_reorder_target: Option<(usize, bool)>` — `bool` is `insert_before`, from whether the pointer is left or right of the chip center (horizontal analogue of the sessions list's y-center test).
- `on_drop` calls `reposition_tab_panel` and sets `active_tab` to the dropped tab's new index.

Because reorder never detaches a panel from a dock, `on_removed` is gone and the false-close design is obsolete for this path.

## Close / drop

`Workspace::close_tab(panel, window, cx)`:

1. If this is an SSH tab, `handle_ssh_tab_closed` (unchanged).
2. `unregister_tab_panel` (drops it from `tab_panels`, `renumber_tabs`).
3. Recompute `active_tab` via `active_index_after_close`.
4. `cx.notify()`. Focus the new active terminal if any.

No `TabPanel::remove_panel`. No `prune_stale_center`. After step 2 the `Entity<TerminalPanel>` is held only by locals that are about to drop, so `TerminalView` / `PtyBackend` drop on this stack. That is the serial-reopen fix, without the empty-group rebuild.

`TerminalPanel::close` becomes: `workspace.close_tab(cx.entity(), window, cx)` (still deferred one tick if it runs from a click listener on itself, same nested-update reason as today).

## What stays on `TerminalPanel`

- Embed `TerminalView`.
- Vertical scrollbar (`ScrollbarHandle` adapter — still gpui-component).
- Right-click copy/paste menu (still gpui-component, still not in `terminal/view.rs`).
- `tab_number` + `set_tab_number` for the `"N-"` prefix.
- `EventEmitter<TerminalPanelEvent::Closed>` can stay for SSH cleanup subscribers, **or** close can call `handle_ssh_tab_closed` directly from `Workspace::close_tab`. Prefer the latter so close is one function; drop the per-`open_*` `subscribe_in` Closed handlers that today only decrement `tab_count` and call `unregister_tab_panel`.

## Files

| File | Role |
|------|------|
| `src/panels/center_tabs.rs` | Pure index helpers, `DragTab`, strip + empty-state render |
| `src/workspace.rs` | Own the list; render center via `center_tabs`; delete dock |
| `src/panels/terminal.rs` | Drop `Panel` / `TabPanel` / false-close; close via workspace |
| `src/panels/keybindings.rs` | `CloseTab` instead of `ClosePanel` |
| `src/panels/mod.rs` | `pub mod center_tabs` |
| `locales/app.yml` | `Terminal.empty_tabs` |

## Testing

Unit tests (no `Window`):

- `active_index_after_close` — close left of active, close active middle, close last, close only tab, out of range.
- `reorder_indices` — move right, move left, no-op, out of range.
- Existing `next_tab_index` / `prev_tab_index` / `goto_tab_index` / `lowest_free_number` unchanged.

Manual (native GPUI, no browser):

- Open local + SSH + serial tabs; `"N-"` matches `secondary-N`.
- Close the first-opened tab while others remain; serial port can be reopened immediately (the 2026-08-17 scenario, now without waiting for an empty group).
- Drag reorder; numbers update; SSH session for that host stays up.
- Close last tab; center shows empty placeholder; New Tab still works.
- `secondary-shift-w` closes the active tab.
- Split-by-dragging-to-the-edge is gone: dropping on the strip only reorders.
