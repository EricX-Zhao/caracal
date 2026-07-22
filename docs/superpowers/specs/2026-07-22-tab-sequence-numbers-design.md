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

## Revision history

- **2026-07-22, initial version:** numbers assigned once at creation,
  reused (lowest-free) on close. Shipped, then found by the user to
  produce confusing behavior: closing a non-last tab left the remaining
  tabs' printed numbers out of sync with their real position, causing
  `secondary-N` to appear to "not work" for a tab whose printed label no
  longer matched any real slot.
- **2026-07-22, revised (this version):** numbers are recomputed from
  live open order on every open/close, eliminating that whole class of
  confusion. Manual drag-reorder remains the one unfixable case (see
  "Known limitation").

## Numbering scheme

- `Workspace` keeps its currently-open `TerminalPanel`s in a single,
  workspace-wide list, in open order (append on open, remove on close —
  every tab kind shares the one list, not scoped per SSH host).
- Every time a tab opens or closes, every remaining open tab's displayed
  number is recomputed as its 1-indexed position in that list — always
  exactly `1..=(currently open tab count)`, with no gaps and no stale
  labels.
- Because a newly-opened tab is always appended at the visual right end
  of the tab strip (confirmed: `gpui-component`'s
  `DockArea::add_panel(Center)` always appends into the existing `Tabs`
  group rather than creating a new one, absent a manual drag), and a
  closed tab is removed from the same list it was pushed into, this
  tracked "open order" always matches the real visual order — as long as
  no drag has happened.

## Known limitation (accepted)

`secondary-N` targets whatever tab is currently in the Nth **visual**
slot of the tab strip — that's existing, tested behavior
(`goto_tab_index`, [workspace.rs:650](../../../src/workspace.rs#L650)) and
is correct even after the tab strip's visual order changes, because it
operates through `DockItem::active_index`, which reads/writes the live
`TabPanel` entity directly. This part was never broken, in either version
of this feature.

The printed number, by contrast, is derived from `Workspace`'s own
tracked open-order list — a list `Workspace` maintains itself, not
something read live off `gpui-component`. Opening and closing tabs update
that list correctly (see "Numbering scheme" above), but a **manual
drag-reorder** changes the tab strip's real visual order through
`gpui-component`'s own internal, private state, which `Workspace` has no
way to observe: dropping a tab elsewhere only emits a generic
`PanelEvent::LayoutChanged` with no position data, and the
`TabPanel::panels` field that holds the true order is private
(`pub(crate)`, confirmed by reading
`~/.cargo/git/checkouts/gpui-component-*/crates/ui/src/dock/tab_panel.rs`).
So after a drag, a tab's printed number can be wrong until the next open
or close event recomputes everything from `Workspace`'s (now stale)
tracked order.

This is accepted as-is: the only way to fix it would be forking
`gpui-component` to add a public accessor for the live order — a
permanent third-party-fork maintenance cost the user explicitly declined
for this feature. It is a materially smaller gap than the previous
version's limitation, though: it now takes an actual drag to desync the
numbers, rather than the ordinary act of closing any non-last tab.

## Architecture

### `Workspace` tracks open tabs in order instead of allocating sticky ids

Replaces the previous `tab_numbers: HashSet<u32>` /
`allocate_tab_number` / `release_tab_number` mechanism entirely (removed,
not layered under this one).

New field:

```rust
/// Every currently open `TerminalPanel`, in open order — the source of
/// truth for the workspace-wide "N-" tab-title sequence number, recomputed
/// by live position (not a sticky per-tab id) every time a tab opens or
/// closes. See docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md
/// for why a manual drag-reorder is the one case this can't track.
tab_panels: Vec<Entity<TerminalPanel>>,
```

New methods:

```rust
/// Append `panel` to the end of the open-tab list and recompute every
/// open tab's displayed sequence number from its new position.
fn register_tab_panel(&mut self, panel: Entity<TerminalPanel>, cx: &mut Context<Self>) {
    self.tab_panels.push(panel);
    self.renumber_tabs(cx);
}

/// Remove `panel` from the open-tab list (by entity identity) and
/// recompute every remaining tab's displayed sequence number.
fn unregister_tab_panel(&mut self, panel: &Entity<TerminalPanel>, cx: &mut Context<Self>) {
    self.tab_panels.retain(|p| p.entity_id() != panel.entity_id());
    self.renumber_tabs(cx);
}

/// Set every open tab's displayed "N-" prefix to its 1-indexed position
/// in `tab_panels`.
fn renumber_tabs(&mut self, cx: &mut Context<Self>) {
    for (i, panel) in self.tab_panels.iter().enumerate() {
        panel.update(cx, |panel, cx| panel.set_tab_number((i + 1) as u32, cx));
    }
}
```

### Wired into all four tab-opening methods (and their close handlers)

`open_local_with` ([workspace.rs:582](../../../src/workspace.rs#L582)),
`open_ssh` ([workspace.rs:725](../../../src/workspace.rs#L725)),
`open_telnet` ([workspace.rs:974](../../../src/workspace.rs#L974)), and
`open_serial` ([workspace.rs:1001](../../../src/workspace.rs#L1001)) each:

1. Construct the `TerminalPanel` exactly as before (`TerminalPanel::new`'s
   signature is unchanged — the `tab_number: u32` it's constructed with is
   a throwaway placeholder, `0`, immediately overwritten by step 2's
   `renumber_tabs` before anything renders).
2. Call `self.register_tab_panel(panel.clone(), cx)` right before
   `self.add_center(Arc::new(panel), window, cx)`.
3. In the existing `cx.subscribe_in(&panel, ...)` closure that already
   resets `tab_count` on `TerminalPanelEvent::Closed`, replace the old
   `this.release_tab_number(tab_seq)` call with
   `this.unregister_tab_panel(panel, cx)`, using the closure's own
   `panel: &Entity<TerminalPanel>` parameter (previously named `_panel`
   and unused) — no captured local needed, so the `move` keyword added by
   the previous version of this feature is no longer necessary on the
   three closures that only needed it for that capture (`open_local_with`,
   `open_telnet`, `open_serial` revert to non-`move` closures; `open_ssh`'s
   closure keeps `move`, needed for its own unrelated captures).

### `TerminalPanel` gains a setter, `title()` is unchanged

[panels/terminal.rs](../../../src/panels/terminal.rs):

- `TerminalPanel`'s existing `tab_number: u32` field and `title()`
  rendering (the `format!("{}-{}", self.tab_number, ...)` prefix) are
  unchanged from the previous version of this feature.
- New method:
  ```rust
  /// Overwrite this tab's displayed "N-" prefix and repaint. Called by
  /// `Workspace::renumber_tabs` whenever the open-tab set changes.
  pub(crate) fn set_tab_number(&mut self, n: u32, cx: &mut Context<Self>) {
      self.tab_number = n;
      cx.notify();
  }
  ```

## Error handling / edge cases

- No behavior change to any shortcut — this is purely a rendering
  mechanism.
- The tracked list is workspace-wide (not per-window); if the app ever
  supports multiple `Workspace` instances, each gets its own independent
  list (matches how the previous `tab_numbers` pool already scoped per
  `Workspace`).
- `unregister_tab_panel` is a no-op if the panel isn't found in the list
  (can't happen in practice — every registered panel is unregistered
  exactly once, via the same `TerminalPanelEvent::Closed` that already
  drives the `tab_count` decrement — but `retain` degrades safely to a
  no-op rather than panicking either way).

## Testing

No new automated tests: unlike the previous version's `lowest_free_number`
gap-filling logic (a pure function, unit-tested), this version's entire
mechanism is live `Entity` manipulation (push/retain/update on real
`TerminalPanel` entities), which — per this codebase's established
precedent throughout its keyboard-shortcut and tab-lifecycle features —
can't be unit-tested without a live `Window`/`DockArea`. Verified instead
by manual testing in the running app (per prior guidance in this project:
no screenshot-driven GUI checks): open several tabs and confirm they read
`1-`, `2-`, `3-...`; close a middle tab and confirm the remaining tabs'
numbers immediately shift down to stay packed; open a new tab and confirm
it takes the next number; drag-reorder a tab and confirm numbers go stale
until the next open/close (the one remaining, accepted gap).
