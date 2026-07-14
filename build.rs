//! Embeds `assets/icon.ico` as the Windows executable's icon (taskbar,
//! Explorer, Alt-Tab). No-op on other targets — Linux/macOS have no
//! equivalent "compile an icon into the binary" step; see the README's
//! build notes for how those platforms pick up `assets/icon.svg` instead.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    if let Err(e) = res.compile() {
        println!("cargo:warning=failed to embed Windows icon resource: {e}");
    }
}
