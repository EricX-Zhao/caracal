//! Caracal — native GPUI terminal. Phase 1: a bare window hosting a single
//! `TerminalView` running the local shell. No `gpui_component` yet (that arrives
//! in Phase 5 as the dock shell).
//!
//! Cross-platform (Windows / Linux / macOS) since the `gpui_platform` migration:
//! the `application()` factory in `gpui_platform` `#[cfg]`-gates the right
//! platform implementation (`gpui_linux` / `gpui_macos` / `gpui_windows`).

// On Windows, a plain console-subsystem exe pops up a black cmd window
// alongside the GUI window (that's where `env_logger`'s output goes). Only
// suppress it in release builds — `cargo run`/`cargo build` (debug) keep the
// console so `RUST_LOG=debug` is visible while developing. No effect on
// Linux/macOS (the attribute is a no-op there).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod config;
mod crypto;
mod keyring_store;
mod panels;
mod paths;
mod quick_commands;
mod settings;
mod terminal;
mod vault;
mod workspace;

use std::borrow::Cow;

use gpui::{
    App, AppContext, Bounds, KeyBinding, SharedString, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions, point, px, size,
};
use gpui_component::{Root, Theme, ThemeMode, ThemeRegistry};
use gpui_platform::application;

use crate::assets::CaracalAssets;

use gpui_component::dock::ClosePanel;
use terminal::view::{ClearScreen, Interrupt, SendBackTab, SendTab, TERMINAL_KEY_CONTEXT};
use workspace::{
    GotoTab1, GotoTab2, GotoTab3, GotoTab4, GotoTab5, GotoTab6, GotoTab7, GotoTab8, GotoTab9,
    NewConnectionAction, NewTab, NextTab, OpenSettingsAction, PrevTab, ToggleLeftSidebar,
    ToggleQuickCommands, ToggleRightSidebar, Workspace, ZoomIn, ZoomOut,
};

rust_i18n::i18n!("locales", fallback = "zh-CN");

/// gpui-component's own bundled theme collection (Ayu, Catppuccin, Gruvbox,
/// Tokyo Night, ...), embedded so the Settings → Appearance theme dropdown
/// has more than just the built-in Default Light/Dark pair. Apache-2.0
/// (same as the `gpui-component` dependency itself), copied verbatim from
/// its `themes/` directory — see `assets/themes/` for the individual files.
const BUNDLED_THEMES: &[&str] = &[
    include_str!("../assets/themes/adventure.json"),
    include_str!("../assets/themes/alduin.json"),
    include_str!("../assets/themes/asciinema.json"),
    include_str!("../assets/themes/aurora.json"),
    include_str!("../assets/themes/ayu.json"),
    include_str!("../assets/themes/catppuccin.json"),
    include_str!("../assets/themes/everforest.json"),
    include_str!("../assets/themes/fahrenheit.json"),
    include_str!("../assets/themes/flexoki.json"),
    include_str!("../assets/themes/gruvbox.json"),
    include_str!("../assets/themes/harper.json"),
    include_str!("../assets/themes/hybrid.json"),
    include_str!("../assets/themes/jellybeans.json"),
    include_str!("../assets/themes/kibble.json"),
    include_str!("../assets/themes/macos-classic.json"),
    include_str!("../assets/themes/matrix.json"),
    include_str!("../assets/themes/mellifluous.json"),
    include_str!("../assets/themes/molokai.json"),
    include_str!("../assets/themes/solarized.json"),
    include_str!("../assets/themes/spaceduck.json"),
    include_str!("../assets/themes/tokyonight.json"),
    include_str!("../assets/themes/twilight.json"),
];

fn load_bundled_themes(cx: &mut App) {
    for content in BUNDLED_THEMES {
        if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(content) {
            log::warn!("failed to load a bundled theme: {e}");
        }
    }
}

/// Apply the persisted theme by exact name (e.g. "Ayu Dark", not just a
/// light/dark mode) — falls back to the built-in Default Dark theme if the
/// saved name isn't registered (e.g. `settings.toml` predates this feature,
/// or names a theme this build no longer bundles).
fn apply_startup_theme(theme_name: &str, cx: &mut App) {
    let theme_name = SharedString::from(theme_name.to_string());
    match ThemeRegistry::global(cx).themes().get(&theme_name).cloned() {
        Some(theme) => Theme::global_mut(cx).apply_config(&theme),
        None => Theme::change(ThemeMode::Dark, None, cx),
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
        load_bundled_themes(cx);

        // Apply the persisted theme by name (defaults to Default Dark if no
        // settings.toml exists yet, or it names a theme that isn't
        // registered) — see Settings → Appearance for the full theme list.
        let startup_settings = settings::load();
        apply_startup_theme(&startup_settings.appearance.theme_name, cx);
        // The OS locale (`LANG`/etc.), not `i18n!()`'s `fallback` param, is
        // what's active before this call — verified directly: with
        // LANG=en_US.UTF-8 set, `t!(...)` returned English before any
        // `set_locale()` call. So the persisted preference must be applied
        // explicitly, same as the theme above.
        rust_i18n::set_locale(&startup_settings.general.language);

        // gpui-component's `Theme::default()` sets `font_family` to
        // `.SystemUIFont` — a macOS-only sentinel (`root.rs` applies it to the
        // whole app via the top-level `Root`'s `.font_family(...)`). On
        // Windows/Linux that name doesn't resolve to a real font, so the
        // window falls back to whatever the OS substitutes — on a zh-CN
        // Windows box that's Microsoft YaHei, not any font this app bundles.
        // Point it at the bundled Sarasa Mono SC (registered below), which
        // covers both Latin and CJK glyphs, so the whole app chrome — not
        // just the terminal content — renders from a font we actually ship.
        Theme::global_mut(cx).font_family = "Sarasa Mono SC".into();

        // Reclaim keys that gpui-component's Root context binds (tab → focus nav,
        // ctrl-c → Copy): bind them in the deeper "Terminal" context so the
        // terminal receives them as raw input (tab completion, SIGINT).
        cx.bind_keys([
            KeyBinding::new("ctrl-c", Interrupt, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("tab", SendTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("shift-tab", SendBackTab, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("secondary-shift-l", ClearScreen, Some(TERMINAL_KEY_CONTEXT)),
            KeyBinding::new("secondary-shift-t", NewTab, None),
            KeyBinding::new("secondary-shift-w", ClosePanel, None),
            KeyBinding::new("secondary-shift-n", NewConnectionAction, None),
            KeyBinding::new("secondary-tab", NextTab, None),
            KeyBinding::new("secondary-shift-tab", PrevTab, None),
            KeyBinding::new("secondary-1", GotoTab1, None),
            KeyBinding::new("secondary-2", GotoTab2, None),
            KeyBinding::new("secondary-3", GotoTab3, None),
            KeyBinding::new("secondary-4", GotoTab4, None),
            KeyBinding::new("secondary-5", GotoTab5, None),
            KeyBinding::new("secondary-6", GotoTab6, None),
            KeyBinding::new("secondary-7", GotoTab7, None),
            KeyBinding::new("secondary-8", GotoTab8, None),
            KeyBinding::new("secondary-9", GotoTab9, None),
            KeyBinding::new("secondary-b", ToggleLeftSidebar, None),
            KeyBinding::new("secondary-shift-b", ToggleRightSidebar, None),
            KeyBinding::new("secondary-j", ToggleQuickCommands, None),
            KeyBinding::new("secondary-,", OpenSettingsAction, None),
            KeyBinding::new("secondary-=", ZoomIn, None),
            KeyBinding::new("secondary-shift-=", ZoomIn, None),
            KeyBinding::new("secondary--", ZoomOut, None),
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
                    // Hides the native titlebar (macOS/Windows) so header.rs's
                    // own 40px row is the only title bar the user sees.
                    // `traffic_light_position` only affects macOS (ignored
                    // elsewhere); it's positioned to land inside that row.
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    // Requests client-side decorations on Linux/Wayland (per
                    // that field's own doc comment: "Wayland only... may be
                    // ignored" elsewhere). This is the sole trigger for
                    // gpui_component::Root's window_border() to switch from a
                    // no-op passthrough to real shadow + 8-direction
                    // resize-edge hit-testing.
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| Workspace::new(window, cx));
                    // The window's top-level view must be a gpui-component `Root`
                    // (provides theme context, overlays, notifications). Don't
                    // add `.bg(cx.theme().background)` here: `Root::render`
                    // already paints `cx.theme().tokens.background` fresh every
                    // frame, but `refine_style` applies our override *after*
                    // that, permanently baking in whatever color was current at
                    // this exact construction moment — so any later theme
                    // toggle would silently stop repainting Root's own
                    // background (anything without its own explicit `.bg(...)`
                    // showed this as a stuck stale-theme color).
                    cx.new(|cx| Root::new(workspace, window, cx))
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
