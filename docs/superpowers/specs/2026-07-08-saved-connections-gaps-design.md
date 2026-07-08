# Saved Connections gaps: in-group drag reorder, hover detail card, import/export

Date: 2026-07-08
Files under change: `src/config.rs`, `src/panels/saved_connections.rs`,
`src/panels/new_connection_window.rs`.

Item 4 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md). Three
independent gaps bundled under one roadmap item (matching how item 3 bundled "standalone
window + icon picker + SSH key auth") — each confirmed individually feasible during
investigation before this spec was written.

## Background

`src/panels/saved_connections.rs` already has a nested group tree, search, a cycling sort
mode (`SortMode::Default`/`NameAsc`/`NameDesc`), and drag-and-drop — but the drag-and-drop
only supports moving a connection *into* a different group (`move_connection_to_group`);
there's no way to reorder connections relative to each other within the same group or the
ungrouped section. `SortMode::Default` currently means "leave `Vec` insertion order alone"
(`sort_connections`, `saved_connections.rs:688-692`) — there's no explicit ordering field to
actually reorder by.

`SavedConnection::tooltip_lines()` (`config.rs:239-266`) already exists, returns
per-connection-type `Vec<(String, String)>` field pairs, and is marked `#[allow(dead_code)]`
— built at some point but never wired to any UI. `gpui-component`'s `Tooltip` type supports
both plain text (`Tooltip::new`) and arbitrary custom content
(`Tooltip::element<E>(impl Fn(&mut Window, &mut App) -> E)`, `crates/ui/src/tooltip.rs:47-59`)
— confirmed during investigation, so a real 2-column key/value grid hover card is directly
buildable, not a fallback to a flattened text tooltip.

There is no import/export of the connections list at all today.

## Decisions (confirmed with user)

### In-group drag reorder

- `SavedConnection` gains `#[serde(default)] pub sort_order: i32` (mirrors
  `SavedConnectionGroup.sort_order`, which already exists and follows the same
  `#[serde(default)]` backward-compat convention).
- Reordering is scoped to siblings sharing the same `group_id` (including `None` — the
  ungrouped section is its own sibling scope) — dragging a connection out of its current
  scope into a different group still goes through the existing `move_connection_to_group`
  path (unchanged); this spec only adds *within-scope* reordering.
- Drop position (before/after the hovered row) is computed from the cursor's Y position
  within the target row's bounds — above the row's vertical midpoint means "insert before,"
  below means "insert after" (the same idea nyaterm's `computeDropPosition` uses, simplified
  to two cases since caracal doesn't need a third "drop inside" case for connection rows —
  only folder rows are drop-into targets, and that's the existing, unchanged
  `move_connection_to_group` behavior).
- `SortMode::Default` changes from "leave `Vec` order alone" to "sort by `sort_order`
  ascending" within each scope — `NameAsc`/`NameDesc` are unaffected (they already
  override with an explicit comparator) and remain the way to temporarily view alphabetical
  order without disturbing the manually-arranged `sort_order` values.
- New connections (via `NewConnectionWindow`) get `sort_order` set to the count of existing
  connections already in the same `group_id` scope at creation time (append-to-end),
  mirroring `create_folder`'s existing `sort_order: self.groups.len() as i32` convention for
  groups.

### Hover detail card

- Reuses `tooltip_lines()` as-is (drop its `#[allow(dead_code)]`) — no changes to that
  method's per-type field selection.
- Rendered via `Tooltip::element(...)` on `ConnectionItem`'s row (attached the same way
  `activity_bar.rs`'s buttons already attach a `Tooltip::new(...)`, just with `::element`
  instead of `::new` and a 2-column `label: value` grid built from the `Vec<(String,
  String)>` instead of a single string) — a real key/value table, not flattened text.
- No new hover-delay tuning — use whatever default `gpui-component`'s tooltip
  attachment already applies elsewhere in this codebase.

### Import / export

- **Format: TOML**, reusing `AppConfig`'s existing `Serialize`/`Deserialize` derive directly
  — no new serialization format, no JSON conversion layer.
- **Export**: writes the *entire* current `connections` + `groups` list (not a selection) to
  a user-chosen path via `cx.prompt_for_new_path` (gpui's native "save as" dialog,
  `app.rs:1378-1383` — confirmed to exist and mirror `prompt_for_paths`'s async-receiver
  shape already used for the private-key browse button in the previous feature).
- **Import**: reads a user-chosen TOML file via the existing `cx.prompt_for_paths` (already
  used for the private-key file picker), parses it as `AppConfig`, and **appends** every
  connection and group from it to the current lists — no dedup/merge-by-identity logic,
  because `SavedConnection` has no stable id to merge on (confirmed during the previous
  roadmap item: connections are identified by `Vec` index, not a UUID, unlike
  `SavedConnectionGroup` which does have one). Appending is the simplest behavior that can't
  silently discard data; a user who imports the same file twice gets duplicates, which they
  can then delete manually — acceptable for a first pass.
- Both actions live in the existing "更多" (More) dropdown in the panel's toolbar (nyaterm's
  own placement per the earlier UI analysis) — no new toolbar buttons.

## Component structure

- `src/config.rs` — `SavedConnection.sort_order: i32` (new field); no changes to
  `tooltip_lines()` itself beyond removing its now-live `#[allow(dead_code)]`.
- `src/panels/saved_connections.rs` — extends the existing drag machinery
  (`DragConnection`, `on_drag`/`drag_over`/`on_drop`) with a same-scope reorder path
  alongside the existing cross-group move path; `sort_connections` reads `sort_order` for
  `SortMode::Default`; `ConnectionItem`'s row (`render_connection`) gains a
  `Tooltip::element(...)` attachment; the "更多" dropdown gains "导出配置"/"导入配置" items
  wired to new `export_connections`/`import_connections` methods.
- `src/panels/new_connection_window.rs` — `save()`'s `SavedConnection` construction sites
  compute and set `sort_order` for new connections (edits keep the existing value, read from
  the connection being edited).

## Testing

- `src/config.rs`: `sort_order`'s `#[serde(default)]` backward-compat covered by a test
  matching the existing `old_config_without_new_fields_still_deserializes` shape (an old
  connection entry with no `sort_order` key still deserializes, defaulting to `0`).
- Drop-position computation (before/after from cursor Y) is a pure function once extracted
  from the drag-drop event handler — worth a unit test if it's written as a standalone
  `fn(row_bounds: Bounds<Pixels>, cursor_y: Pixels) -> DropPosition`-shaped function; if the
  implementer finds it more natural to inline the comparison directly in the `on_drop`
  closure (matching this file's existing style, which doesn't extract small comparisons into
  named functions elsewhere), that's acceptable too — not a hard requirement to extract
  purely for testability.
- Export/import file I/O isn't unit-tested (matches `config::load`/`save` having no tests of
  their own — thin `std::fs`/`toml` wrappers); the *parsing* of an imported `AppConfig` is
  already covered by existing `toml::from_str` round-trip tests in `config.rs`, which is the
  actual risk surface (a hand-edited or foreign-tool-exported TOML file failing to parse) —
  no new test needed since import literally reuses `config::load`'s existing parse path.
- UI (drag visuals, tooltip rendering, dropdown items) isn't unit-tested, consistent with
  the rest of `panels/*.rs`.
- Manual smoke test must cover: drag-reorder within a group, drag-reorder in the ungrouped
  section, confirm `NameAsc`/`NameDesc` still work and don't corrupt `sort_order`, hover a
  connection of each of the 4 types and confirm the right fields show, export to a file and
  inspect its contents, import that file back in (confirm connections/groups appended, not
  replaced), import a second time (confirm duplicates appear rather than data loss or a
  crash).

## Non-goals

- No cross-group drag reorder combined with repositioning in one gesture — moving into a
  different group still appends at that group's end (existing `move_connection_to_group`
  behavior, unchanged); only same-group reordering gets position control.
- No import merge/dedup by identity — see Decisions above.
- No import from other tools' formats (WindTerm/MobaXterm/Xshell) — TOML only.
- No selective/partial export (e.g. checkbox-select specific connections) — always the full
  list.
- No jump-host chain display in the hover card — caracal has no jump-host concept yet
  (tracked as a separate, larger, explicitly-deferred item from the previous roadmap entry).
