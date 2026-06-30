//! Caracal — native GPUI terminal. Phase 1: a bare window hosting a single
//! `TerminalView` running the local shell. No `gpui_component` yet (that arrives
//! in Phase 5 as the dock shell).
//!
//! Cross-platform (Windows / Linux / macOS) since the `gpui_platform` migration:
//! the `application()` factory in `gpui_platform` `#[cfg]`-gates the right
//! platform implementation (`gpui_linux` / `gpui_macos` / `gpui_windows`).

mod terminal;

use std::borrow::Cow;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use terminal::ssh::SshConfig;
use terminal::view::TerminalView;

/// Symbol font bundled into the binary and registered with the text system, so
/// Nerd Font glyphs resolve from the *same* fontdb cosmic-text shapes with
/// (system-installed copies in `~/.local/share/fonts` are not reliably scanned).
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// Phase 4 entry point for SSH (the session-list UI is Phase 6): if
/// `CARACAL_SSH=user@host[:port]` is set, open an SSH terminal using
/// `CARACAL_SSH_PASSWORD` for auth. Otherwise `None` → local shell.
fn ssh_config_from_env() -> Option<SshConfig> {
    let spec = std::env::var("CARACAL_SSH").ok()?;
    let (user, hostport) = spec.split_once('@')?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(22)),
        None => (hostport, 22),
    };
    Some(SshConfig {
        host: host.to_string(),
        port,
        user: user.to_string(),
        password: std::env::var("CARACAL_SSH_PASSWORD").unwrap_or_default(),
    })
}

fn main() {
    #[cfg(target_os = "linux")]

    application().run(|cx: &mut App| {
        if let Err(e) = cx
            .text_system()
            .add_fonts(vec![Cow::Borrowed(SYMBOLS_NERD_FONT_MONO)])
        {
            log::warn!("failed to register bundled symbol font: {e}");
        }

        let ssh = ssh_config_from_env();
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| match ssh {
                    Some(config) => TerminalView::new_ssh(window, cx, config),
                    None => TerminalView::new(window, cx),
                })
            },
        )
        .expect("failed to open window");

        cx.on_window_closed(|cx, _window_id| cx.quit()).detach();
        cx.activate(true);
    });
}
