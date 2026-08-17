# Serial tab reopen reports "device busy" after closing the last tab

## Problem

User-reported: close a serial terminal's last open tab, then immediately
reopen the same port — the open fails with "device busy". Retrying once more
(a second, separate connect attempt) always succeeds.

## Investigation

Two hypotheses were tried and ruled out with real evidence before the actual
root cause was found (see conversation history for the full trail):

1. **Timing race in `SerialBackend`'s own thread teardown** — its reader
   thread only notices a `shutdown` flag on its next blocking-`read()`
   timeout (`serial.rs`), so releasing the OS-level port handle lags tab
   close by up to that timeout. Fixed by shrinking the timeout and adding a
   bounded retry-with-backoff in `SerialBackend::open` on
   `serialport::ErrorKind::NoDevice` (Unix: `serialport` opens ports
   exclusively via `TIOCEXCL` + `flock`, so a "busy" open surfaces as this
   error kind specifically — see `serial.rs:120-131`,
   [terminal/serial.rs:213-219](../../../src/terminal/serial.rs#L213)).
   **This didn't fix the user's report**: they confirmed the busy error
   happened "immediately or after several seconds" — i.e. not time-bounded
   at all, which falsified the race theory outright (a real teardown race
   resolves with elapsed time; this one didn't).
2. Reproduced `serial.rs`'s open/drop/reopen cycle in isolation against a
   `socat`-created virtual tty pair (no physical hardware needed — Unix
   `TIOCEXCL`/`flock` apply to any tty, including pseudo-ttys). Confirmed
   that once `SerialBackend::drop` genuinely runs and its threads exit, a
   reopen always succeeds, with no dependency on elapsed time. This proved
   `serial.rs` itself was not the culprit — something upstream was not
   dropping the previous tab's `SerialBackend` at all.

Temporary `[CARACAL-DEBUG]` `eprintln!` instrumentation was added across the
whole close→reopen path (`SerialBackend::open`/`Drop`/thread-exit,
`TerminalView::drop`, `TerminalPanel::close`/`on_removed`/`Drop`,
`Workspace::unregister_tab_panel`/`add_center`), and the user reproduced the
bug with a debug build. The trace showed the actual `TerminalPanel`/
`TerminalView`/`SerialBackend` chain was **not dropped at tab-close time at
all** — `Workspace.tab_panels` was correctly pruned, but nothing else was.
It was only dropped partway through the *next* `open_serial` call, during
`add_center`, specifically inside `dock_area.set_center(fresh_center, ...)`
— by which point that same `open_serial` call's own
`SerialBackend::open` attempt had already failed.

## Root cause

`gpui_component::dock::DockArea.center: DockItem` is a cached tree, separate
from the live rendering tree each `TabPanel` entity maintains internally.
`DockItem::Tabs` carries its own `items: Vec<Arc<dyn PanelView>>` — a
snapshot gpui-component only ever appends to via `add_panel`, never prunes
on `remove_panel`. So when the last tab of a `TabPanel` closes:

- The *live* `TabPanel.panels` list is correctly emptied
  (`TabPanel::detach_panel`'s `self.panels.retain(...)`), so the UI
  correctly shows the tab gone.
- The *stale* `DockItem::Tabs.items` entry inside `DockArea.center` still
  holds a strong `Arc<dyn PanelView>` pointing at the just-closed
  `TerminalPanel` — keeping its `TerminalView`, and hence its
  `Arc<dyn PtyBackend>` (`SerialBackend`), alive.

This was already a known bug (`add_center`'s original comment, predating
this investigation) for its cosmetic symptom: double-clicking a saved
connection right after closing every tab silently did nothing, because
`add_panel` found and reused the stale, now-invisible `Tabs` entry instead
of creating a new visible one. The fix already in place for that
(`center_tab_group_is_stale` + rebuild) only ran lazily, inside
`add_center`, i.e. on the *next* tab open — one `open_*` call too late to
help a backend holding an OS-level exclusive resource. For `SerialBackend`
specifically, "too late" means: the stale `Arc<dyn PanelView>` — and the
live reader/writer threads holding the port open with `TIOCEXCL`/`flock` —
were still fully alive throughout the *entire* first reopen attempt
(including this fix's own 3-attempt retry loop), and were only released
moments later, as a side effect of that same failed attempt's own
`add_center` call. The next, second reopen then succeeds because the
release already happened.

Not a timing race: this is a strict one-cycle leak. It never resolves by
waiting; it only resolves when another tab is opened.

## Fix

Prune the stale `center` tree **eagerly, the moment a tab actually closes**,
not lazily on the next open. Extracted the existing stale-detect-and-rebuild
logic out of `add_center` into
[`Workspace::prune_stale_center`](../../../src/workspace.rs#L1484), called
from two places:

- [`Workspace::unregister_tab_panel`](../../../src/workspace.rs#L720) — the
  real fix, run synchronously as part of every tab's close cleanup (all four
  `open_*` methods' `TerminalPanelEvent::Closed` handlers already call this).
  Required threading a live `&mut Window` through (previously unused
  `_window` in three of those four closures).
- [`Workspace::add_center`](../../../src/workspace.rs#L1438) — kept as a
  defensive fallback for any future close path that doesn't route through
  `unregister_tab_panel`; should normally be a no-op by the time it runs.

`center_tab_group_is_stale` itself is unchanged.

## Testing

No new automated test: like the drag-reorder false-close bug
([2026-07-22-drag-reorder-false-close-design.md](2026-07-22-drag-reorder-false-close-design.md)),
this is live `Entity`/GPUI dock-tree lifecycle behavior that can't be
exercised without a real `Window`/`DockArea`. Verified via:

- A standalone, `serial.rs`-only reproduction against a `socat` virtual tty
  pair, confirming `serial.rs`'s own release logic is correct in isolation.
- A debug build with temporary `[CARACAL-DEBUG]` tracing (since removed),
  run by the user against real hardware, which pinpointed the exact drop
  ordering above.
- The user re-verified manually after this fix that closing the last serial
  tab and immediately reopening the same port no longer reports "busy".
