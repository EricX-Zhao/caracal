
# Caracal

A native, GPU-accelerated terminal / SSH / Telnet / serial client built on
[GPUI](https://github.com/zed-industries/zed) (the UI framework behind
[Zed](https://zed.dev)) and [gpui-component](https://github.com/longbridge/gpui-component).

## Features

- **Local terminal** — spawns your default shell (or a custom shell/working
  directory) in a native PTY.
- **SSH** — password-authenticated shell sessions, with an integrated
  **SFTP** file browser sharing the same connection (no second dial).
- **Telnet** — a raw-TCP client with RFC 854 IAC negotiation (terminal type,
  suppress-go-ahead, echo).
- **Serial** — connect to physical/virtual serial devices with full
  configuration (baud rate, data bits, parity, stop bits, flow control).
- **Saved connections** — a tree of saved connections with folders/groups,
  search, sort, drag-and-drop organization, and per-connection edit/
  duplicate/delete.
- GPU-accelerated rendering (via `wgpu`) with Nerd Font / powerline glyph
  support.

## Building from source

Requires a recent [Rust toolchain](https://rustup.rs) (stable).

```bash
cargo build --release
```

### Linux build dependencies

GPUI's X11/Wayland/font-kit support and the serial backend's device
enumeration need a few system packages (Debian/Ubuntu package names shown;
see [`.github/workflows/build.yml`](.github/workflows/build.yml) for the
exact list used in CI):

```bash
sudo apt-get install -y \
    build-essential clang cmake pkg-config \
    libfontconfig-dev libx11-dev libx11-xcb-dev \
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
    libvulkan1 libssl-dev libudev-dev
```

### Windows

No extra system packages are needed beyond the Rust (MSVC) toolchain.

## Prebuilt binaries

Every push to `main` builds Windows and Linux binaries via
[GitHub Actions](.github/workflows/build.yml); tagged releases (`vX.Y.Z`)
publish them as GitHub Releases, alongside a Windows NSIS installer (`.exe`)
and a Linux Debian package (`.deb`) built with
[cargo-packager](https://github.com/crabnebula-dev/cargo-packager). The raw
binaries are still included for anyone who'd rather place them on `PATH`
manually.

## Acknowledgments

Caracal owes a debt to the terminal clients that came before it:

- **[WindTerm](https://github.com/kingToolbox/WindTerm)** — for proving how
  far a single native terminal/SSH client can go: sessions, SFTP, serial, and
  more, all in one fast, keyboard-driven tool. A lot of Caracal's feature
  shape follows that lead.
- **[NyaTerm](https://github.com/nyakang/nyaterm)** — an earlier project that
  planted the seed for this one.

Thank you both.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.
