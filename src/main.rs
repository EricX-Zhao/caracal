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

use terminal::view::TerminalView;

/// Symbol font bundled into the binary and registered with the text system, so
/// Nerd Font glyphs resolve from the *same* fontdb cosmic-text shapes with
/// (system-installed copies in `~/.local/share/fonts` are not reliably scanned).
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

fn main() {
    #[cfg(target_os = "linux")]

    application().run(|cx: &mut App| {
        if let Err(e) = cx
            .text_system()
            .add_fonts(vec![Cow::Borrowed(SYMBOLS_NERD_FONT_MONO)])
        {
            log::warn!("failed to register bundled symbol font: {e}");
        }

        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TerminalView::new(window, cx)),
        )
        .expect("failed to open window");

        cx.on_window_closed(|cx, _window_id| cx.quit()).detach();
        cx.activate(true);
    });
}
