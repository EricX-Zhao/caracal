//! Caracal — native GPUI terminal. Phase 1: a bare window hosting a single
//! `TerminalView` running the local shell. No `gpui_component` yet (that arrives
//! in Phase 5 as the dock shell).
//!
//! Cross-platform (Windows / Linux / macOS) since the `gpui_platform` migration:
//! the `application()` factory in `gpui_platform` `#[cfg]`-gates the right
//! platform implementation (`gpui_linux` / `gpui_macos` / `gpui_windows`).

mod assets;
mod config;
mod panels;
mod quick_commands;
mod settings;
mod terminal;
mod workspace;

use std::borrow::Cow;

use gpui::{App, AppContext, Bounds, KeyBinding, Styled, WindowBounds, WindowOptions, actions, px, size};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};
use gpui_platform::application;

use crate::assets::CaracalAssets;

use terminal::view::{Interrupt, SendBackTab, SendTab, TERMINAL_KEY_CONTEXT};
use workspace::Workspace;

actions!(caracal, [ToggleTheme]);

/// Flip the theme and persist the choice to `settings.toml`, so Ctrl+K (bound
/// below) and the View menu's "切换主题" item (`panels::header`) always agree
/// with each other and with what Settings → Appearance shows.
pub(crate) fn toggle_theme(cx: &mut App) {
    let next = if Theme::global(cx).mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(next, None, cx);

    let mut settings = settings::load();
    settings.appearance.theme_mode = if next.is_dark() { "dark" } else { "light" }.to_string();
    if let Err(e) = settings::save(&settings) {
        log::error!("failed to persist theme toggle: {e}");
    }
}

/// Symbol font bundled into the binary and registered with the text system, so
/// Nerd Font glyphs resolve from the *same* fontdb cosmic-text shapes with
/// (system-installed copies in `~/.local/share/fonts` are not reliably scanned).
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// Default terminal font (see `terminal::view::FontConfig`), bundled so
/// rendering is consistent across platforms instead of depending on each OS
/// having a resolvable "system monospace" font (Windows in particular had no
/// working auto-detection — see the design spec).
const JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// CJK fallback font (see `terminal::view::FontConfig`), bundled so Chinese
/// glyphs resolve even on a Windows machine without an East Asian language
/// pack installed.
const SARASA_MONO_SC_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/SarasaMonoSC-Regular.ttf");

fn main() {
    // Without a logger backend installed, every `log::error!`/`warn!`/`info!`
    // call in the app is silently dropped. Default to `info` so SSH/SFTP
    // failures are visible; override with `RUST_LOG=debug` for more.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Register asset source: upstream lucide icons + our custom SVGs
    // (upload, download, file-plus, folder-plus, refresh-cw, trash-2).
    // Without this, every `Icon::new(..)` renders blank.
    application().with_assets(CaracalAssets).run(|cx: &mut App| {
        // Must run before using any gpui-component feature (theme/overlay/etc.).
        gpui_component::init(cx);

        // Apply the persisted theme (defaults to dark if no settings.toml
        // exists yet, or its theme_mode isn't "light") — uses the built-in
        // dark/light themes from gpui-component (One Dark / One Light).
        let startup_settings = settings::load();
        let startup_theme = if startup_settings.appearance.theme_mode == "light" {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(startup_theme, None, cx);

        // Allow toggling between light/dark with Ctrl+K.
        cx.bind_keys([KeyBinding::new(
            "ctrl-k",
            ToggleTheme,
            Some("caracal"),
        )]);

        cx.on_action(|_action: &ToggleTheme, cx| toggle_theme(cx));

        // Reclaim keys that gpui-component's Root context binds (tab → focus nav,
        // ctrl-c → Copy): bind them in the deeper "Terminal" context so the
        // terminal receives them as raw input (tab completion, SIGINT).
        cx.bind_keys([
            KeyBinding::new("ctrl-c", Interrupt, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", SendBackTab, Some(TERMINAL_KEY_CONTEXT)),
        ]);

        if let Err(e) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(SYMBOLS_NERD_FONT_MONO),
            Cow::Borrowed(JETBRAINS_MONO_REGULAR),
            Cow::Borrowed(SARASA_MONO_SC_REGULAR),
        ]) {
            log::warn!("failed to register bundled fonts: {e}");
        }

        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        let main_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| Workspace::new(window, cx));
                    // The window's top-level view must be a gpui-component `Root`
                    // (provides theme context, overlays, notifications).
                    cx.new(|cx| Root::new(workspace, window, cx).bg(cx.theme().background))
                },
            )
            .expect("failed to open window");

        // `on_window_closed` fires for every window (including the settings
        // window opened by `Workspace::open_settings`), not just this one —
        // only quit the app when the *main* window is the one that closed.
        let main_window_id = main_window.window_id();
        cx.on_window_closed(move |cx, window_id| {
            if window_id == main_window_id {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
