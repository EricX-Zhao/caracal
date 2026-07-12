# SSH: instant terminal on double-click, in-terminal connect errors, session teardown on last-tab-close

Date: 2026-07-12

Files under change: `src/terminal/view.rs`, `src/panels/terminal.rs`, `src/workspace.rs`.

## Background

Double-clicking a saved SSH connection currently goes through
`Workspace::open_ssh` (`workspace.rs:284`), which calls `Workspace::ssh_session`
(`workspace.rs:207`). That method connects **synchronously, on the UI thread**
(`SshSession::connect`, a blocking handshake) *before* any terminal tab exists. Two
consequences:

- A slow or unreachable host freezes the whole UI for however long the connect takes.
- On failure, `ssh_session` returns `None`, `open_ssh` silently does nothing — no tab opens,
  the only trace is a `log::error!` line invisible to the user.

This is inconsistent with Telnet and Serial, which already open their tab unconditionally
and, on connect failure, print a colored error line into the terminal via a `DeadBackend`
placeholder (`view.rs:242-267`) — the pattern this spec extends to SSH.

Separately, `ssh_sessions: HashMap<String, Arc<SshSession>>` (the cache shared by a host's
shell tab, SFTP browser, and monitor panel — CLAUDE.md §2) is only ever evicted by
`reconnect_ssh_terminal` when redialing after a drop. Closing every terminal tab for a host
leaves its session (and, if open, its SFTP/monitor panels) alive indefinitely — a resource
leak with no user-visible way to actually "close the connection."

The existing manual-reconnect path (`reconnect_ssh_terminal`, `workspace.rs:332-368`,
triggered by `TerminalViewEvent::ReconnectRequested` when the user presses Enter on a dead
tab) already does a background-thread connect + `TerminalView::reconnect_with` swap. This
spec reuses that machinery for the *initial* connect too, rather than inventing a second
mechanism.

## Decisions (confirmed with user)

### Connecting/failed banner (`terminal/view.rs`)

- `disconnected: bool` is replaced by:
  ```rust
  enum ConnBanner {
      Connecting,      // dialing; Enter does nothing — nothing to retry yet
      Failed(String),  // dead, with a reason; Enter re-dials
  }
  ```
  stored as `banner: Option<ConnBanner>` (`None` = live, banner hidden). A new `host_label:
  String` field (`"user@host"`) is set once at construction and reused by both banner
  variants' text.
- Rendered text (same dimmed-overlay treatment as today's banner, `view.rs:858-873`):
  - `Connecting` → `"正在连接 {host_label}…"`
  - `Failed(reason)` → `"{host_label} {reason}，按 Enter 重连"`
  - The existing mid-session-drop path (`mark_disconnected`, a live shell's read side
    closing) becomes `Failed("连接已断开".to_string())` — same user-visible wording as today,
    now expressed through the shared enum instead of a separate bool.
  - A genuine connect failure becomes `Failed(format!("连接失败: {e}"))`, `e` being
    `SshSession::connect`'s real `anyhow::Error` — the same value already going to
    `log::error!` today, now also shown in the terminal.
- `on_key_down` (`view.rs:558-568`): all input stays swallowed while `banner.is_some()`;
  Enter only emits `TerminalViewEvent::ReconnectRequested` when the banner is `Failed(_)`
  (not `Connecting`) — this incidentally fixes a latent bug where mashing Enter during an
  in-flight reconnect could fire multiple concurrent redials, since today's plain `bool`
  never distinguishes "dead" from "currently redialing."
- New constructor `TerminalView::new_ssh_connecting(window, cx, host_label: String)`: same
  base setup as `with_backend` (focus handle, `Term`, drain task) but skips
  `spawn_generation` entirely — sets `backend: Arc::new(DeadBackend)` and `_disconnect_watch:
  Task::ready(())` directly, `banner: Some(ConnBanner::Connecting)`. Skipping the real
  feeder/disconnect-watch here matters: those are only wired up in `with_backend` since a
  spawned-then-immediately-dropped `bytes_tx` (the pattern telnet/serial's `DeadBackend`
  fallback already relies on) would otherwise fire `mark_disconnected` within the same tick
  and stomp the `Connecting` banner. `reconnect_with` (existing, unchanged) is what actually
  wires up the real generation once the dial resolves — same call already used by
  `reconnect_ssh_terminal`.
- `new_ssh_shell` gains the `host_label: String` param (its one caller, `open_ssh`, already
  has `config.user`/`config.host` in scope) so a session that connects via the cached fast
  path still gets correct banner text if it later drops mid-session.
- New `TerminalView::mark_connect_failed(&mut self, reason: String, cx)`: sets `banner =
  Some(ConnBanner::Failed(reason)); cx.notify();` — called by `Workspace` from both the
  initial-connect failure path and (for consistency) `reconnect_ssh_terminal`'s failure path.

### Async connect flow (`Workspace::open_ssh`)

- **Cached session** (`ssh_sessions.contains_key(&key)`): unchanged fast path — build the
  tab with `new_ssh_shell`, show SFTP/monitor immediately. No behavior change here.
- **Not cached**: build the tab with `new_ssh_connecting`, add it to the dock and focus it
  immediately — the tab appears instantly regardless of how long the dial takes. Then
  `cx.background_spawn(async move { SshSession::connect(dial_config) })` +
  `cx.spawn_in(window, ...)` to await it (same shape as `reconnect_ssh_terminal`):
  - **Success**: insert into `ssh_sessions`, swap the tab's backend via
    `terminal.update(cx, |view, cx| view.reconnect_with(...))`. Then, **only if this tab is
    still the focused terminal** (`self.focused_terminal`'s entity id matches), call
    `show_sftp`/`show_monitor` — if the user has since focused something else, we don't yank
    the side panels away from what they're looking at. (If they refocus this tab later, the
    on-focus handler below now correctly picks up the cached session and shows them then.)
  - **Failure**: `log::error!` (unchanged) + `view.mark_connect_failed(e.to_string(), cx)`.
- **On-focus handler guard**: the existing per-tab `cx.on_focus` closure
  (`workspace.rs:292-296`) unconditionally calls `show_sftp`/`show_monitor`, which
  internally does its *own* on-demand blocking connect via `ssh_session()` if the session
  isn't cached. Since the connecting tab is focused immediately on creation, that would fire
  a second, redundant synchronous connect racing the background one. Fix: the closure now
  checks `ssh_sessions.contains_key(&key)` first — cached → show them; not yet cached → call
  `show_sftp_placeholder`/`show_monitor_placeholder` instead (they'll be shown for real once
  either the connect's own success handler or a later focus event finds the session cached).
- **Accepted edge case, not fixed**: double-clicking the same not-yet-connected host twice in
  quick succession starts two independent background dials (nothing dedupes in-flight
  connects). Each tab still ends up correctly wired to *a* live session; `ssh_sessions` ends
  up holding whichever resolves last. Harmless and self-resolving (extra tabs close like any
  other), not worth a shared in-flight-dial map for something this rare — today's fully
  synchronous connect makes this literally impossible (the UI thread is blocked), so this is
  a new-but-benign possibility introduced by going async.
- `reconnect_ssh_terminal` (manual Enter-to-retry) gets the same reason-carrying failure
  handling: sets the banner to `Connecting` right before redialing, and to `Failed(reason)`
  (via `mark_connect_failed`) instead of just logging on failure.

### Tab-close cleanup

- `TerminalPanel` (`panels/terminal.rs`) — the thin dock adapter — gains one generic,
  backend-agnostic hook: a new zero-data event `TerminalPanelEvent::Closed`, emitted from
  `Panel::on_removed` (a real trait hook, called by `TabPanel::remove_panel` — confirmed in
  `gpui-component`'s `dock/tab_panel.rs:350`; currently unimplemented here, default no-op).
  `TerminalPanel` doesn't know or care why it's being removed — matches its existing
  "adapter only" mandate (file header, `panels/terminal.rs:1-4`).
- `Workspace::open_ssh` subscribes to each SSH tab's panel for `Closed`, mirroring the
  existing `ReconnectRequested` subscription. Handler (`handle_ssh_tab_closed`):
  1. Remove this terminal's entry from `ssh_reconnect_configs` (the `EntityId → SshConfig`
     map, `workspace.rs:301`).
  2. If any other live entry in that map still maps to the same host key, stop — other tabs
     still need the session.
  3. Otherwise (last tab for this host): remove the cached `SshSession` from `ssh_sessions`
     (same eviction pattern `reconnect_ssh_terminal` already uses), and remove that host's
     entries from `sftp_panels` and `monitor_panels`, falling back to the placeholder
     (`show_sftp_placeholder`/`show_monitor_placeholder`) if either was the one currently
     displayed.
- Local/Telnet/Serial tabs also emit `Closed` (it lives on the shared `TerminalPanel`), but
  only SSH tabs (via `open_ssh`) subscribe to it — no session-sharing exists for those
  backends, so the event is simply unobserved for them.

## Component structure

- `src/terminal/view.rs` — `ConnBanner` enum replaces `disconnected: bool`; `host_label`
  field; `new_ssh_connecting` constructor; `mark_connect_failed`; `new_ssh_shell` gains
  `host_label` param; `on_key_down` and `render`'s banner block read the new enum.
- `src/panels/terminal.rs` — `TerminalPanelEvent::Closed` + `EventEmitter` impl;
  `Panel::on_removed` override.
- `src/workspace.rs` — `open_ssh` rewritten for the cached/not-cached branch described
  above; on-focus closure gains the cached-session guard; new `handle_ssh_tab_closed`;
  `reconnect_ssh_terminal`'s failure arm calls `mark_connect_failed` and its start sets the
  `Connecting` banner.

## Testing

- `ConnBanner` text formatting (`Connecting`/`Failed` → expected string) is a pure function
  of `host_label` + variant — worth a couple of unit tests in `view.rs`'s existing
  `#[cfg(test)]` module if one exists there already; otherwise inline assertions are fine,
  matching this file's existing test density.
- `on_key_down`'s gating (Enter swallowed during `Connecting`, forwarded as
  `ReconnectRequested` during `Failed`) isn't easily unit-testable without a full `Window` —
  consistent with the rest of this file's input handling, which relies on manual/smoke
  testing rather than unit tests for `on_key_down` itself.
- `handle_ssh_tab_closed`'s "any other tab still uses this host" check is a pure function of
  `ssh_reconnect_configs`'s values once the closing entry is removed — could be extracted
  and unit-tested, but matches this codebase's existing style of leaving small `HashMap`
  scans inline (e.g. `drag_reorder_target`'s handling in `saved_connections.rs`) rather than
  pulling out a named helper purely for testability.
- Manual smoke test must cover: double-click a reachable host (tab appears instantly, shows
  "正在连接…", then the real shell prompt); double-click an unreachable/wrong-port host (tab
  appears instantly, banner flips to "连接失败: ...", Enter retries); open two tabs to the
  same host, close one (session/SFTP/monitor stay alive), close the second (session evicted,
  SFTP/monitor panels close or fall back to placeholder if currently shown); a mid-session
  drop (kill the remote sshd or unplug network) still shows "连接已断开，按 Enter 重连" and
  Enter still reconnects.

## Non-goals

- No shared in-flight-dial dedup for rapid double-clicks on the same not-yet-connected host
  — see "Accepted edge case" above.
- No propagation of a specific reason string for the mid-session-disconnect banner (the
  feeder/disconnect-watch signal today is a bare "closed," no error value) — stays the fixed
  "连接已断开" text. Plumbing a real reason through that path is a separate, larger change.
- No change to the still-unwired `SavedConnectionsEvent::OpenSftp` path or `show_sftp`'s own
  on-demand synchronous connect fallback (used only by that dead path today) — out of scope,
  since the user's ask is specifically about the double-click-to-open-terminal flow.
- No dedicated "close this connection" UI action — closing is purely a side effect of
  closing the last terminal tab, per the confirmed design; there's no separate button/menu
  item to force-close a still-tabbed connection.
