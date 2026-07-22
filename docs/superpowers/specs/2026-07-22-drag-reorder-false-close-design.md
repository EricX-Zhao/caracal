# Drag-reorder spuriously triggers tab-close cleanup

## Problem

Discovered while investigating a user-reported tab-numbering bug: dragging a
tab to reorder it (even within the same tab strip, no split involved) makes
`gpui-component`'s internal `TabPanel::detach_panel`
(`~/.cargo/git/checkouts/gpui-component-*/crates/ui/src/dock/tab_panel.rs:344-354`)
call `panel.on_removed(window, cx)` **unconditionally**, before immediately
re-inserting the panel elsewhere. `detach_panel` is the shared internal
helper behind both a genuine removal (`TabPanel::remove_panel`, used by our
own "X" close button and by `gpui_component::dock::ClosePanel`) and every
drag-driven reposition (`on_drop` → `detach_panel` then
`insert_panel_at`/`add_panel_with_active`/`split_panel`). `on_removed` has no
way to tell these two callers apart.

Caracal's `TerminalPanel::on_removed`
([panels/terminal.rs:289-291](../../../src/panels/terminal.rs#L289)) treats
every call as "this tab is really gone" and unconditionally emits
`TerminalPanelEvent::Closed`. `Workspace` subscribes to that event in all
four `open_*` methods to decrement `tab_count`, remove the tab from the
`tab_panels` renumbering list (see
[2026-07-22-tab-sequence-numbers-design.md](2026-07-22-tab-sequence-numbers-design.md)),
and — for SSH tabs specifically — call `handle_ssh_tab_closed`
([workspace.rs:956-979](../../../src/workspace.rs#L956)), which tears down
the shared `SshSession`/SFTP panel/Monitor panel once no tab references that
host anymore.

**Net effect:** every drag-reorder of a tab currently fires this entire
close-cleanup chain, even though the tab is still open and visually present
right after the drag completes:

- The tab silently drops out of `tab_panels`, so its `"N-"` sequence number
  freezes forever (never recomputed again) — this is what surfaced the bug
  to the user (a dragged tab's number visibly went stale, and shortcuts
  pointing at the true visual position of *other*, unaffected tabs appeared
  to "not work" because `tab_count` had silently under-counted).
- For an SSH tab: if the dragged tab was the last one open for its host,
  the shared `ssh_session`/SFTP panel/Monitor panel for that host get torn
  down while the terminal tab is still sitting there, open and (from the
  user's perspective) unaffected — a real data/session-lifecycle bug, not
  just a cosmetic one.

This predates today's tab-numbering feature entirely — it's been present
since the SSH async-connect tab-lifecycle work introduced
`TerminalPanelEvent::Closed`. It was only surfaced now because the
numbering feature made the frozen-number side effect visible.

## Scope

Fix the false-positive close detection for every drag scenario `gpui-component`
supports: same-group reorder, cross-group drop, and edge-drop-creates-a-split.
Out of scope: any other `gpui-component` behavior, and the already-accepted
"drag doesn't renumber a moved tab immediately" limitation (see the tab
sequence numbers spec) — that limitation is about the *number*, this fix is
about the *false close cleanup*, a different and more serious problem.

## Fix design: self-healing close detection

Confirmed by reading `gpui-component`'s source: every path that re-attaches
a detached panel — `insert_panel_at` (same-group reorder,
[tab_panel.rs:309-325](https://github.com/longbridge/gpui-component)),
`add_panel_with_active` (cross-group drop, `tab_panel.rs:254-281`), and
`split_panel`'s call into a new `TabPanel::add_panel` (edge-drop-creates-a-
split, `tab_panel.rs:994-1007`) — calls `panel.on_added_to(...)` on the way
back in. `TerminalPanel::on_added_to` already exists
([terminal.rs:280-287](../../../src/panels/terminal.rs#L280)) and is the one
reliable signal available that says "I've just been (re)attached somewhere."

So: `on_removed` no longer emits `Closed` immediately. It marks itself
detached and defers the actual "is this really gone" check by one update
cycle (`window.defer`, the same reentrancy-avoidance pattern already used by
`TerminalPanel::close()` a few lines below, for the same underlying reason —
GPUI panics on nested self-updates, so anything reacting to a panel-lifecycle
callback from inside that callback must defer). If `on_added_to` fires again
before the deferred check runs (a drag: detach then immediately reinsert, all
within the same synchronous call chain, well before the deferred closure gets
scheduled to run), the tab is still attached and nothing happens. If it
doesn't (a genuine close), `Closed` fires as originally designed, just one
tick later than before — imperceptible, and `Workspace`'s four subscription
closures don't need to change at all.

### `TerminalPanel` changes ([panels/terminal.rs](../../../src/panels/terminal.rs))

New field:

```rust
/// Whether this panel is currently attached to a `TabPanel`. Flipped to
/// `false` by `on_removed`, back to `true` by `on_added_to` — the signal
/// `on_removed` uses to tell a genuine close apart from gpui-component's
/// internal detach-then-reinsert dance for a drag-reorder (both call
/// `on_removed` identically; only a real close never calls `on_added_to`
/// again afterward). See docs/superpowers/specs/2026-07-22-drag-reorder-false-close-design.md.
attached: bool,
```

`new()` initializes it `true`. `on_added_to` additionally sets it `true`
(alongside its existing `self.tab_panel = Some(tab_panel)`). `on_removed`
becomes:

```rust
fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.attached = false;
    let weak = cx.entity().downgrade();
    window.defer(cx, move |window, cx| {
        let _ = weak.update(cx, |this, cx| {
            if !this.attached {
                cx.emit(TerminalPanelEvent::Closed);
            }
        });
    });
}
```

(Signature changes from `_window` to `window`, now used.)

## Error handling / edge cases

- The deferred `weak.update(...)` is guaranteed to still resolve for both
  the real-close and the drag case: `Workspace`'s `tab_panels` list (see the
  tab-sequence-numbers spec) holds a *strong* `Entity<TerminalPanel>` for
  every registered tab and only drops it once `Closed` actually fires —
  which, after this fix, only happens *inside* this very deferred callback.
  So the entity is always still alive when the check runs, real close or
  not; the `let _ =` is defensive, not load-bearing.
- No change to `Workspace`'s four `open_*` methods or their
  `cx.subscribe_in(&panel, ...)` closures — they still just react to
  `Closed` exactly as before.
- No change to the already-accepted "printed number goes stale after a
  drag" limitation — that remains, documented separately. This fix only
  stops the *false close cleanup*; it does not make drag-reorder
  renumber-aware (still out of `Workspace`'s reach without observing
  gpui-component's private tab order, per the existing spec).

## Testing

No new automated test: this is live `Entity`/GPUI-lifecycle-callback
behavior (`on_removed`/`on_added_to`/`window.defer`), which — per this
codebase's established precedent — can't be exercised without a live
`Window`/`DockArea` driving real drag-and-drop input. Verified manually in
the running app: open 2+ tabs, drag one to reorder within the strip, and
confirm (a) its number does *not* freeze/disappear from later renumbering
the next time any tab opens or closes, and (b) for an SSH tab that was the
only one open for its host, dragging it does *not* disconnect/reset its
SFTP or Monitor panel. Also confirm a genuine tab close (the "X" button,
`Ctrl+Shift+W`) still decrements `tab_count`/renumbers/tears down SSH state
exactly as before — just one update-cycle tick later, imperceptibly.
