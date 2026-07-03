# Telnet and Serial terminal backends + new-connection UI

Date: 2026-07-03
Files under change: `src/config.rs`, `src/terminal/backend.rs` (doc comment only),
new `src/terminal/telnet.rs`, new `src/terminal/serial.rs`, `src/terminal/view.rs`,
`src/workspace.rs`, `src/panels/saved_connections.rs`, `src/panels/icons.rs`,
`Cargo.toml`.

## Background

Caracal (a nyaterm-style client, [nyaterm reference](https://github.com/nyakang/nyaterm/tree/main))
currently supports two connection types end to end: SSH (`src/terminal/ssh.rs`,
shared session cached per host, SFTP over the same connection) and local shell
(`src/terminal/backend.rs`'s `LocalPty`). `ConnectionType` in `src/config.rs`
only has `Ssh`/`Local` variants, and the new-connection form in
`src/panels/saved_connections.rs` only offers those two.

`src/terminal/backend.rs`'s own doc comment already anticipates this: "every
transport (local PTY, SSH, serial) implements the same `PtyBackend`. ... Phase 1
only implements `LocalPty`." This spec adds the two remaining transports —
Telnet and serial-port — plus the connection-type UI to create them.

## Decisions (confirmed with user)

- **Telnet has no stored credentials.** The form only asks for host + port
  (default port 23). Login happens interactively inside the terminal, exactly
  like typing at a raw `telnet` prompt — the client never tries to detect a
  login prompt and auto-type a username/password, since that's unreliable
  across different telnetd implementations and devices.
- **Telnet protocol handling is "basic negotiation"**, not raw passthrough and
  not a full per-option implementation: the client parses IAC (`RFC 854`)
  sequences out of the stream (so control bytes never render as garbage),
  answers `SUPPRESS_GO_AHEAD` and `TERMINAL_TYPE` negotiation so the session
  actually opens cleanly against network gear (routers/switches) and standard
  `telnetd`, and cleanly refuses (`DONT`/`WONT`) every other option instead of
  hanging. No NAWS (live window-resize) in this scope — `resize()` is a
  documented no-op for the telnet backend.
- **Serial gets full configuration**, not just port + baud: data bits, parity,
  stop bits, and flow control are all form fields (not hardcoded 8N1), because
  the target use case is real embedded/hardware debugging where devices vary.
- **The new-connection type selector stays a pill row**, extended from 2 to 4
  buttons (SSH / 本地终端 / Telnet / 串口) rather than becoming a dropdown —
  keeps the existing look, wraps to two rows.
- **已保存的连接 (Saved Connections) panel icon** changes from the upstream
  `IconName::HardDrive` to the project's own `assets/icons/server.svg` (already
  done as a drive-by fix, ahead of this spec — see `src/panels/icons.rs`).

## 1. Data model (`src/config.rs`)

```rust
pub enum ConnectionType {
    Ssh,
    Local,
    Telnet,
    Serial,
}
```

`SavedConnection` reuses `host`/`port` for `Telnet` (same fields SSH already
uses). `default_port()` (the serde default for missing `port` fields in old
config files) stays `22` — it's a parse-time fallback, not conn-type-aware.
The *form's* prefilled/placeholder port value is conn-type-aware instead: `22`
when the SSH pill is active, `23` when the Telnet pill is active (§4).
`SavedConnection` gains serial-only fields, all `Option` so existing
`connections.toml` files without them still deserialize via `#[serde(default)]`:

```rust
/// Device path, e.g. "/dev/ttyUSB0" or "COM3". Serial only.
#[serde(default)]
pub serial_port: Option<String>,
/// Serial only. Defaults to 115200 if unset.
#[serde(default)]
pub baud_rate: Option<u32>,
/// Serial only: 5/6/7/8. Defaults to 8 if unset.
#[serde(default)]
pub data_bits: Option<u8>,
/// Serial only: "none" | "odd" | "even". Defaults to "none" if unset.
#[serde(default)]
pub parity: Option<String>,
/// Serial only: 1 | 2. Defaults to 1 if unset.
#[serde(default)]
pub stop_bits: Option<u8>,
/// Serial only: "none" | "software" | "hardware". Defaults to "none" if unset.
#[serde(default)]
pub flow_control: Option<String>,
```

New conversion methods next to `to_ssh_config()`:

```rust
pub fn to_telnet_config(&self) -> TelnetConfig {
    TelnetConfig { host: self.host.clone(), port: self.port }
}

pub fn to_serial_config(&self) -> SerialConfig {
    SerialConfig {
        port: self.serial_port.clone().unwrap_or_default(),
        baud_rate: self.baud_rate.unwrap_or(115_200),
        data_bits: self.data_bits.unwrap_or(8),
        parity: self.parity.clone().unwrap_or_else(|| "none".into()),
        stop_bits: self.stop_bits.unwrap_or(1),
        flow_control: self.flow_control.clone().unwrap_or_else(|| "none".into()),
    }
}
```

`display_name()` / `subtitle()` / `resolve_icon()` / `tooltip_lines()` get
match arms:

- Telnet display/subtitle: same shape as SSH's `user@host:port`, minus the
  user — just `host:port`.
- Serial display: the port path's last path component (e.g. `ttyUSB0`), falling
  back to `"serial"`. Subtitle: `"{port} @ {baud}bps"`.
- Icons: `Telnet => AppIcon::Telnet`, `Serial => AppIcon::SerialPort` (both new
  `AppIcon` variants, see §5).

## 2. Backends

### `src/terminal/telnet.rs` (new)

Same shape as `LocalPty` in `backend.rs`: a plain `std::net::TcpStream`, no
tokio (unlike SSH, telnet has no multiplexed sub-protocol like SFTP to justify
an async session thread — one tab, one socket, same as a local shell).

```rust
pub struct TelnetConfig {
    pub host: String,
    pub port: u16,
}

pub struct TelnetBackend {
    write_tx: flume::Sender<Vec<u8>>,
    stream: Mutex<TcpStream>, // for shutdown on drop
}

impl TelnetBackend {
    pub fn connect(config: TelnetConfig, bytes_tx: flume::Sender<Vec<u8>>) -> Result<Self>;
}

impl PtyBackend for TelnetBackend {
    fn write(&self, bytes: &[u8]) { /* IAC-escape 0xFF -> 0xFF 0xFF, send via write_tx */ }
    fn resize(&self, _cols: u16, _rows: u16) { /* no-op: no NAWS in this scope */ }
}
```

Reader thread: reads raw bytes off the socket into a small state machine that
recognizes `IAC` (`0xFF`) and either:

- `IAC IAC` → emit one literal `0xFF` byte to the display stream.
- `IAC WILL <opt>` → if `opt == SUPPRESS_GO_AHEAD (3)` or `opt == ECHO (1)`,
  reply `IAC DO <opt>`; else reply `IAC DONT <opt>`.
- `IAC DO <opt>` → if `opt == TERMINAL_TYPE (24)` or `opt == SUPPRESS_GO_AHEAD (3)`,
  reply `IAC WILL <opt>`; else reply `IAC WONT <opt>`.
- `IAC SB TERMINAL_TYPE SEND(1) IAC SE` → reply
  `IAC SB TERMINAL_TYPE IS(0) "xterm-256color" IAC SE`.
- Any other `IAC SB ... IAC SE` → consumed and discarded (not forwarded to the
  display stream), no reply sent.
- Everything outside an `IAC` sequence → forwarded to `bytes_tx` unchanged.

Writer thread: same `write_tx`-channel-into-blocking-write pattern as
`LocalPty`'s writer thread; escapes `0xFF` → `0xFF 0xFF` before sending
(required by RFC 854 so a literal 0xFF byte the user types/pastes isn't
misparsed as the start of a command sequence).

### `src/terminal/serial.rs` (new)

Uses the `serialport` crate's blocking `Box<dyn SerialPort>` (which is
`Read + Write`), split via `try_clone()` into a reader-thread half and a
writer-thread half — same structural pattern as `LocalPty::spawn_with`
(reader thread → `bytes_tx`, writer thread ← `flume::Receiver` fed by
`PtyBackend::write`).

```rust
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
    pub data_bits: u8,     // 5/6/7/8
    pub parity: String,    // "none" | "odd" | "even"
    pub stop_bits: u8,     // 1/2
    pub flow_control: String, // "none" | "software" | "hardware"
}

pub struct SerialBackend {
    write_tx: flume::Sender<Vec<u8>>,
}

impl SerialBackend {
    pub fn open(config: SerialConfig, bytes_tx: flume::Sender<Vec<u8>>) -> Result<Self>;
}

impl PtyBackend for SerialBackend {
    fn write(&self, bytes: &[u8]) { let _ = self.write_tx.send(bytes.to_vec()); }
    fn resize(&self, _cols: u16, _rows: u16) { /* no-op: physical ports have no size */ }
}

/// Detected ports for the new-connection form's picker. Wraps
/// `serialport::available_ports()`; returns an empty Vec (not an error) if
/// detection fails, so the form always falls back gracefully to manual entry.
pub fn list_ports() -> Vec<String>;
```

`data_bits`/`parity`/`stop_bits`/`flow_control` map directly onto
`serialport::{DataBits, Parity, StopBits, FlowControl}` via small `match`
helpers local to this file.

### `Cargo.toml`

Add `serialport = "4.9"` under the "Terminal model + transport" section,
alongside `portable-pty` and `russh`.

## 3. `TerminalView` + `Workspace` wiring

`src/terminal/view.rs`: two new constructors next to `new_ssh_shell`, both
just new `with_backend` closures (no change to `with_backend` itself —
`TerminalView` stays fully backend-agnostic):

```rust
pub fn new_telnet(window: &mut Window, cx: &mut Context<Self>, config: TelnetConfig) -> Self {
    Self::with_backend(window, cx, move |_cols, _rows, bytes_tx| {
        Arc::new(TelnetBackend::connect(config, bytes_tx).expect("telnet connect failed"))
    })
}

pub fn new_serial(window: &mut Window, cx: &mut Context<Self>, config: SerialConfig) -> Self {
    Self::with_backend(window, cx, move |_cols, _rows, bytes_tx| {
        Arc::new(SerialBackend::open(config, bytes_tx).expect("serial open failed"))
    })
}
```

`.expect()` matches the existing `LocalPty::spawn(...).expect(...)` in
`TerminalView::new` — connect/open failures surface as a panic-in-closure today
for local shells too; this spec doesn't change that error-handling posture.

`src/workspace.rs`: `open_telnet`/`open_serial`, structurally identical to
`open_local_with` (§ same file) — no shared-connection cache like
`ssh_sessions`, since neither transport has an SFTP-style second channel to
justify one; each tab dials its own socket/port, same as local shells. Both
call `show_sftp_placeholder` on focus (SFTP stays SSH-only).

`SavedConnectionsEvent` (in `saved_connections.rs`) gains:

```rust
OpenTelnet(TelnetConfig),
OpenSerial(SerialConfig),
```

wired into `Workspace::new`'s existing `cx.subscribe_in(&saved, ...)` match
alongside `Open`/`OpenSftp`/`OpenLocal`.

## 4. New-connection UI (`src/panels/saved_connections.rs`)

**Type selector**: the existing two-pill row (`div().id("type-ssh")` /
`div().id("type-local")`, `render_form`) extends to four pills in a
`flex().flex_row().flex_wrap()` container: SSH / 本地终端 / Telnet / 串口. Same
active/inactive styling (`cx.theme().primary` vs `accent`).

**`ConnForm` fields**: Telnet reuses the SSH `host`/`port` `InputState`
entities — no new fields, just a new form-field-set match arm using the two
already-present inputs. Serial adds:

```rust
// Serial fields
serial_port: Entity<InputState>,   // text input, "/dev/ttyUSB0" placeholder
baud_rate: Entity<InputState>,     // text input, default "115200"
data_bits: u8,                     // 5/6/7/8, pill-toggle, default 8
parity: String,                    // "none"/"odd"/"even", pill-toggle, default "none"
stop_bits: u8,                     // 1/2, pill-toggle, default 1
flow_control: String,              // "none"/"software"/"hardware", pill-toggle, default "none"
detected_ports: Vec<String>,       // populated by the "刷新" button, empty until pressed
```

The four enum-like serial fields are plain struct fields (not `InputState`)
mutated directly by pill on_click handlers, matching how `form.conn_type`
itself is already a plain field flipped by pill clicks — consistent with the
rest of this form, no new widget dependency (`gpui-component`'s `Select`
component was considered and rejected: it's a searchable-list-backed widget
disproportionate to a 3-4-option toggle, and every other choice in this form
is already a hand-rolled pill).

**Port picker**: `serial_port` is a free-text `InputState` (so headless/manual
entry always works) with a "刷新" button next to it that calls
`serial::list_ports()` and stores the result in `detected_ports`; when
non-empty, a small clickable list renders beneath the input (same row style as
the connection-list rows) — clicking an entry sets the text input's value to
that path.

**`open_new_connection_form`/`start_edit`**: extended to construct/pre-fill the
new fields (straightforward, same pattern as the existing SSH/Local field
construction — default values from `SavedConnection::to_serial_config()`'s
defaults when adding, from the existing connection's stored values when
editing).

**`save_form`**: two new match arms building `SavedConnection { conn_type:
Telnet, host, port, .. }` and `SavedConnection { conn_type: Serial,
serial_port, baud_rate, data_bits, parity, stop_bits, flow_control, .. }`
(all other fields empty/`None`, matching how the `Local` arm today leaves
`host`/`user`/`password` empty).

**Row rendering / open-on-double-click cleanup**: today this logic is
duplicated in two places — `on_action_open_connection` (a `match
conn.conn_type` with 2 arms) and the row renderer's `clickable` binding (an
`if conn_type == Ssh {..} else {..}`, i.e. "SSH vs. everything else is
treated as Local"). That second form is already slightly wrong in spirit
(it doesn't actually branch on `Local`, just on "not SSH") and would be
outright wrong once there are 4 types. Both call sites get collapsed onto one
helper:

```rust
fn open_event(conn: &SavedConnection) -> SavedConnectionsEvent {
    match conn.conn_type {
        ConnectionType::Ssh => SavedConnectionsEvent::Open(conn.to_ssh_config()),
        ConnectionType::Local => SavedConnectionsEvent::OpenLocal(
            conn.shell_path.clone().unwrap_or_default(),
            conn.working_dir.clone().unwrap_or_default(),
        ),
        ConnectionType::Telnet => SavedConnectionsEvent::OpenTelnet(conn.to_telnet_config()),
        ConnectionType::Serial => SavedConnectionsEvent::OpenSerial(conn.to_serial_config()),
    }
}
```

`on_action_open_connection` becomes `cx.emit(open_event(conn))`; the row's
`on_click` closure captures `conn.clone()` (or the resolved event value, to
stay `'static`) and does the same on double-click.

## 5. Icons (`src/panels/icons.rs`)

No new SVG assets. `gpui-component-assets` already ships `network.svg` and
`cpu.svg`, unused by `AppIcon` today. New variants:

```rust
pub enum AppIcon {
    ...
    Telnet,
    SerialPort,
}
```

```rust
Telnet => IconName::Network,
SerialPort => IconName::Cpu,
```

`SavedConnection::resolve_icon()`'s auto-resolve-from-`conn_type` match gets
`ConnectionType::Telnet => AppIcon::Telnet` and
`ConnectionType::Serial => AppIcon::SerialPort`. The user-icon-name override
match in the same function also gets `"telnet"` / `"serial"|"cpu"` string
cases alongside the existing `"terminal"`/`"laptop"`/`"server"`/`"network"`.

## Out of scope

- Telnet NAWS (live terminal resize over the wire) and any option beyond
  `SUPPRESS_GO_AHEAD`/`TERMINAL_TYPE`/`ECHO` — basic negotiation only, per
  decision above.
- Telnet auto-login (credential storage/auto-type) — explicitly rejected.
- Serial DTR/RTS manual toggling, break signals, or any control beyond
  open/read/write/close.
- A settings UI for default baud-rate presets — the free-text baud input with
  a "115200" placeholder is enough for this pass.
