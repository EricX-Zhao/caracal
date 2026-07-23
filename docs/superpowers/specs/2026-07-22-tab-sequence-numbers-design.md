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
same numbering, in a single tab strip (no split panes — see "Known
limitation" below for why that's the boundary). Out of scope: any change
to the shortcuts themselves, which already work.

## Revision history

- **2026-07-22, initial version:** numbers assigned once at creation,
  reused (lowest-free) on close. Shipped, then found by the user to
  produce confusing behavior: closing a non-last tab left the remaining
  tabs' printed numbers out of sync with their real position, causing
  `secondary-N` to appear to "not work" for a tab whose printed label no
  longer matched any real slot.
- **2026-07-22, revised:** numbers are recomputed from live open order on
  every open/close, eliminating that whole class of confusion. Manual
  drag-reorder remained an accepted, unfixable gap at this point.
- **2026-07-23, revised (this version):** while fixing an unrelated bug
  (drag-reorder spuriously firing tab-close cleanup — see
  [2026-07-22-drag-reorder-false-close-design.md](2026-07-22-drag-reorder-false-close-design.md)),
  found that `gpui-component` always makes a dropped tab the active one in
  its `TabPanel`, in every drag-drop path — a fact reachable through
  `TabPanel::active_ix()`, a public method. That closes the previously
  "unfixable" drag-reorder gap too, without forking anything, for the
  single-tab-strip case this feature already assumes.

## Numbering scheme

- `Workspace` keeps its currently-open `TerminalPanel`s in a single,
  workspace-wide list, in visual order (append on open, remove on close,
  repositioned on a drag-reorder — see "Drag-reorder keeps numbers in
  sync" below — every tab kind shares the one list, not scoped per SSH
  host).
- Every time the open-tab set changes, every open tab's displayed number
  is recomputed as its 1-indexed position in that list — always exactly
  `1..=(currently open tab count)`, with no gaps and no stale labels.
- A newly-opened tab is always appended at the visual right end of the
  tab strip (confirmed: `gpui-component`'s `DockArea::add_panel(Center)`
  always appends into the existing `Tabs` group rather than creating a
  new one, absent a manual drag), and a closed tab is removed from the
  same list it was pushed into — so this tracked list stays in sync with
  the real visual order continuously, including through drags.

## Known limitation (accepted)

`secondary-N` targets whatever tab is currently in the Nth **visual**
slot of the tab strip — that's existing, tested behavior
(`goto_tab_index`, [workspace.rs:650](../../../src/workspace.rs#L650)) and
is correct even after the tab strip's visual order changes, because it
operates through `DockItem::active_index`, which reads/writes the live
`TabPanel` entity directly. This part was never broken, in any version
of this feature.

The printed number is derived from `Workspace`'s own tracked open-order
list, kept in sync with the real visual order (see "Drag-reorder keeps
numbers in sync" below) for the one `TabPanel` group this feature has
ever assumed exists. **Remaining gap:** if tabs are ever split into
multiple panes (side-by-side, via dragging a tab to a strip edge —
`gpui-component`'s `TabPanel::split_panel`), this feature has no notion
of a single, global order across groups — `Workspace::active_tabs_item`
([workspace.rs:1363](../../../src/workspace.rs#L1363)) only ever resolves
the *first* `Tabs` group it finds in the dock tree, so `secondary-N`
navigation itself is already scoped to one group, independent of tab
numbering. This is a pre-existing boundary of the whole tab-navigation
feature, not something newly introduced here, and remains out of scope.

## Drag-reorder keeps numbers in sync

Reading `gpui-component`'s `TabPanel::on_drop` (and the three paths it can
call — `insert_panel_at` for a same-group reorder, `add_panel_with_active`
for a cross-group drop, and `split_panel`'s `TabPanel::add_panel` for an
edge-drop that creates a new group) shows that a dropped tab is *always*
left as the active panel in its (possibly new) `TabPanel` — every call
site passes `active: true`, or (`insert_panel_at`) sets it unconditionally.
`TabPanel::active_ix()` is a public method, already used elsewhere in this
codebase (`Workspace::active_tabs_item`/`set_active_tab_index`).

So `TerminalPanel::on_added_to` — which fires every time gpui-component
(re)attaches a panel to a `TabPanel`, including the panel's very first
attach *and* every drag-driven reattach — reads that `TabPanel`'s
`active_ix()` and asks `Workspace` to reposition this tab to that index in
its own tracked order, then renumbers. On a tab's first attach this is a
no-op (it's already at the end, where `register_tab_panel` just put it,
matching a fresh `TabPanel`'s `active_ix` of "last"); on a drag reattach,
it's exactly the correction needed. No `gpui-component` fork required —
this was missed on the previous revision's investigation because it was
framed as "can we read the whole live order," when only this tab's own
new index was ever needed.

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

### `TerminalPanel` repositions itself in `Workspace`'s order on every (re)attach

New field, mirroring the existing `WeakEntity<Workspace>` back-reference
pattern already used by `SftpPanel`/`MonitorPanel` (`TerminalPanel` has
otherwise stayed decoupled from `Workspace` — this is the one place it
needs to call back):

```rust
workspace: WeakEntity<Workspace>,
```

Passed into `TerminalPanel::new` (a new parameter) by all four `open_*`
methods, using the same `let workspace_handle = cx.entity().downgrade();`
idiom `Workspace::new` already uses for `QuickCommandsPanel`
([workspace.rs:448](../../../src/workspace.rs#L448)).

`on_added_to` becomes:

```rust
fn on_added_to(
    &mut self,
    tab_panel: WeakEntity<TabPanel>,
    _window: &mut Window,
    cx: &mut Context<Self>,
) {
    self.tab_panel = Some(tab_panel.clone());
    self.attached = true;
    let Some(active_ix) = tab_panel.upgrade().map(|tp| tp.read(cx).active_ix()) else {
        return;
    };
    let Some(workspace) = self.workspace.upgrade() else {
        return;
    };
    let this = cx.entity();
    workspace.update(cx, |workspace, cx| {
        workspace.reposition_tab_panel(this, active_ix, cx);
    });
}
```

(`_cx` becomes `cx`, now used.)

`Workspace` gains one method, alongside `register_tab_panel`/
`unregister_tab_panel`:

```rust
/// Move `panel` to `new_ix` in the open-tab list (removing it from
/// wherever it currently sits first) and recompute every open tab's
/// displayed sequence number. Called by `TerminalPanel::on_added_to`
/// every time gpui-component (re)attaches a panel — including a manual
/// drag-reorder, whose new position this reads via the dropped panel's
/// own `TabPanel::active_ix()` (always left pointing at it — see
/// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
fn reposition_tab_panel(&mut self, panel: Entity<TerminalPanel>, new_ix: usize, cx: &mut Context<Self>) {
    self.tab_panels.retain(|p| p.entity_id() != panel.entity_id());
    let ix = new_ix.min(self.tab_panels.len());
    self.tab_panels.insert(ix, panel);
    self.renumber_tabs(cx);
}
```

`register_tab_panel` is unchanged and still called from all four
`open_*` methods — `reposition_tab_panel` firing again moments later (via
the panel's own first `on_added_to`) is a harmless, cheap no-op reshuffle
in the common case, not a redundancy worth engineering away at this scale
(tab counts are realistically single digits to a few dozen).

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
- `reposition_tab_panel`'s `new_ix.min(self.tab_panels.len())` clamp is
  defensive: in the single-tab-strip case this feature assumes,
  `active_ix` should never exceed the group's real panel count, but the
  clamp keeps `Vec::insert` from panicking even if that assumption is
  ever violated (e.g. a future multi-group scenario).
- `on_added_to`'s reposition call is a no-op if `self.workspace` has gone
  away (`upgrade()` returns `None`) — can't happen while the workspace
  itself is still running, since it's the one holding every `TerminalPanel`
  alive in the first place, but degrades safely regardless.

## Testing

No new automated tests: unlike the previous version's `lowest_free_number`
gap-filling logic (a pure function, unit-tested), this version's entire
mechanism is live `Entity` manipulation (push/retain/update on real
`TerminalPanel` entities) plus GPUI lifecycle-callback timing
(`on_added_to`), which — per this codebase's established precedent
throughout its keyboard-shortcut and tab-lifecycle features — can't be
unit-tested without a live `Window`/`DockArea` driving real drag input.
Verified instead by manual testing in the running app (per prior guidance
in this project: no screenshot-driven GUI checks): open several tabs and
confirm they read `1-`, `2-`, `3-...`; close a middle tab and confirm the
remaining tabs' numbers immediately shift down to stay packed; open a new
tab and confirm it takes the next number; drag-reorder a tab within the
strip and confirm every tab's number correctly reflects the new order
immediately (no more staleness); confirm a fresh tab's own first attach
still lands at the correct next number (the reposition-on-first-attach
no-op case).
