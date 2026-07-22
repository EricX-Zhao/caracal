# Tab sequence numbers

## Problem

`secondary-1`..`secondary-9` (`GotoTab1`..`GotoTab9`,
[main.rs:168-176](../../../src/main.rs#L168),
[workspace.rs:66-77](../../../src/workspace.rs#L66)) already jump to the
Nth tab in the center dock's tab group, but nothing on screen tells the
user which number a given tab currently is — they have to count tabs
visually every time. This adds a visible `N-` prefix to every terminal
tab's title, matching the position the shortcut would jump to.

## Scope

All four terminal tab kinds — local shell, SSH, Telnet, Serial — get the
same numbering. Out of scope: renumbering after a manual drag-reorder of
tabs (see "Known limitation" below) and any change to the shortcuts
themselves, which already work.

## Numbering scheme

- Numbers are 1-indexed and assigned once, when a tab is opened.
- A closed tab's number is freed and reused for the next tab opened,
  taking the lowest currently-free positive integer — the same scheme
  `Workspace` already uses for per-host SSH tab dedup numbering
  (`allocate_ssh_tab_number`/`release_ssh_tab_number`/`lowest_free_number`,
  [workspace.rs:614-665](../../../src/workspace.rs#L614)), generalized to a
  single workspace-wide pool instead of one pool per SSH host key.
- This keeps numbers packed into `1..=(currently open tab count)` in the
  common case (no drag-reorder), which is what makes them line up with
  `secondary-1..9`.

## Known limitation (accepted)

`secondary-N` targets whatever tab is currently in the Nth **visual**
slot of the tab strip — that's existing, tested behavior
(`goto_tab_index`, [workspace.rs:650](../../../src/workspace.rs#L650)) and
is correct even after a manual drag-to-reorder, because it operates
through `DockItem::active_index`, which reads/writes the live `TabPanel`
entity directly.

The printed number on a tab, by contrast, is decided once at creation and
never recomputed from the tab's live visual position. After a manual
drag, a tab's printed number may therefore no longer match the slot
`secondary-N` would need to target it — e.g. drag the 3rd tab to the
front and it still displays `3-`, even though it's now visually first.

This is accepted as-is: doing it properly would require reading
`gpui-component`'s `TabPanel::panels` field, which is private
(`pub(crate)`, confirmed by reading
`~/.cargo/git/checkouts/gpui-component-*/crates/ui/src/dock/tab_panel.rs`),
and dropping a tab elsewhere only emits a generic `PanelEvent::LayoutChanged`
with no position data. The only way to fix this would be forking
`gpui-component` to add a public accessor — a permanent third-party-fork
maintenance cost the user explicitly declined for this feature.

## Architecture

### `Workspace` gains a workspace-wide number pool

New field, alongside the existing `ssh_tab_numbers`:

```rust
/// Sequence numbers ("N-" tab title prefix) currently in use, workspace-wide.
tab_numbers: HashSet<u32>,
```

New methods mirroring the SSH ones, both built on the existing pure
`lowest_free_number` helper (unchanged, reused as-is):

```rust
fn allocate_tab_number(&mut self) -> u32 {
    let n = Self::lowest_free_number(&self.tab_numbers);
    self.tab_numbers.insert(n);
    n
}

fn release_tab_number(&mut self, n: u32) {
    self.tab_numbers.remove(&n);
}
```

### Wired into all four tab-opening methods

`open_local_with` ([workspace.rs:572](../../../src/workspace.rs#L572)),
`open_ssh` ([workspace.rs:697](../../../src/workspace.rs#L697)),
`open_telnet` ([workspace.rs:944](../../../src/workspace.rs#L944)), and
`open_serial` ([workspace.rs:969](../../../src/workspace.rs#L969)) each:

1. Call `self.allocate_tab_number()` right before constructing the
   `TerminalPanel`, and pass the result into `TerminalPanel::new`.
2. Capture that same number by move into the existing
   `cx.subscribe_in(&panel, ...)` closure that already resets
   `tab_count` on `TerminalPanelEvent::Closed`
   ([workspace.rs:604-608](../../../src/workspace.rs#L604) and the
   equivalent block in the other three methods), adding
   `this.release_tab_number(tab_number);` alongside the existing
   `tab_count` decrement. No change to `TerminalPanelEvent` itself needed —
   each open method already owns its own per-tab closure.

### `TerminalPanel` renders the prefix

[panels/terminal.rs](../../../src/panels/terminal.rs):

- New field `tab_number: u32` on the `TerminalPanel` struct
  ([terminal.rs:95-105](../../../src/panels/terminal.rs#L95)).
- `TerminalPanel::new(terminal: Entity<TerminalView>, tab_number: u32) -> Self`
  ([terminal.rs:108](../../../src/panels/terminal.rs#L108)) — new parameter.
- `title()` ([terminal.rs:241-263](../../../src/panels/terminal.rs#L241))
  prepends a `format!("{}-", self.tab_number)` text child before the
  existing title text child, so a tab renders e.g. `1-Local Shell` or
  `2-myhost:1` (the `:1` there being the unrelated, pre-existing SSH
  per-host dedup suffix — both numbers can appear together, since they
  answer different questions).

## Error handling / edge cases

- No behavior change to any shortcut — this is purely a rendering
  addition.
- Number pool is workspace-wide (not per-window); if the app ever supports
  multiple `Workspace` instances, each gets its own independent pool
  (matches how `ssh_tab_numbers` already scopes per-`Workspace` today).

## Testing

Unit tests for `allocate_tab_number`/`release_tab_number`'s reuse-lowest-
free-number behavior, following the existing precedent of the
`lowest_free_number`/`allocate_ssh_tab_number` tests already in
`workspace.rs`'s test module. Manual verification in the running app for
the rendered prefix itself (per prior guidance in this project: no
screenshot-driven GUI checks).
