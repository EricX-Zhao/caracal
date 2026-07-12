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

### Cross-compiling a Windows build from Linux

For a quick local Windows binary without waiting on CI (see below), from a
Linux/WSL machine:

```bash
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
cargo xwin build --target x86_64-pc-windows-msvc
```

`cargo-xwin` downloads and caches the Windows SDK/CRT files it needs on first
use (no Visual Studio install required) and links against them. The binary
lands at `target/x86_64-pc-windows-msvc/debug/caracal.exe`.

**Debug builds only** — `--release` currently fails: `gpui_windows`'s build
script precompiles its HLSL shaders via `fxc.exe` for release builds only
(`#[cfg(not(debug_assertions))]` in its `build.rs`), and `fxc.exe` is itself a
native Windows PE binary that can't run on Linux without something like Wine.
Debug builds skip that step entirely and link fine. Real release builds still
come from CI (`windows-latest`, native).

This also needs a few things beyond the two `rustup`/`cargo install` steps
above, since Ubuntu's `clang`/`llvm-18` packages don't expose the MSVC-style
tools cargo-xwin needs under their plain (unversioned) names:

```bash
sudo apt-get install -y lld-18 clang-tools-18 nasm
for t in llvm-lib llvm-rc llvm-dlltool llvm-ar llvm-mt llvm-cvtres lld-link clang-cl; do
    sudo ln -s /usr/bin/${t}-18 /usr/bin/${t}
done
```

There's also a known upstream path mismatch: `embed-resource` (which `gpui`
uses to embed its Windows manifest) runs `llvm-rc` with its working directory
set to the `.rc` file's own folder, but `gpui`'s `gpui.rc` references its
manifest with a path relative to the crate root instead — the combination
means the manifest resource can't be found when cross-compiling. Worked
around locally by symlinking the path `llvm-rc` expects inside the fetched
git checkout:

```bash
GPUI_DIR=$(find ~/.cargo/git/checkouts -maxdepth 2 -iname "zed-*" | head -1)/1d217ee/crates/gpui
mkdir -p "$GPUI_DIR/resources/windows/resources/windows"
ln -sf ../../gpui.manifest.xml "$GPUI_DIR/resources/windows/resources/windows/gpui.manifest.xml"
```

This lives in Cargo's shared git-checkout cache, not this repo, so it can be
lost on a fresh clone or `cargo update` and may need reapplying.

## Prebuilt binaries

Every push to `main` builds Windows and Linux binaries via
[GitHub Actions](.github/workflows/build.yml); tagged releases (`vX.Y.Z`)
publish them as GitHub Releases.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.
