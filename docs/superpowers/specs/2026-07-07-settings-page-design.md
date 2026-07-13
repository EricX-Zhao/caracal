# Settings page (child window)

Date: 2026-07-07
Files under change: new `src/settings.rs`, new `src/panels/settings_window.rs`,
`src/panels/header.rs`, `src/workspace.rs`, `src/main.rs` (startup theme load).

Item 1 of [nyaterm-gap-roadmap.md](../../reference/nyaterm-gap-roadmap.md) — the settings
page is built first because later sub-projects (quick commands' poll intervals, resource
monitor's per-panel toggles) need a place to configure into, and because it's the first
place caracal opens a genuine second native window, a pattern later reused for the
standalone new-connection window.

## Background

caracal has font-configuration methods (`TerminalView::set_font_family`/`set_font_size`/
`set_font_config` in `src/terminal/view.rs`) with no UI calling them, and a Ctrl+K
dark/light theme toggle (`main.rs`) that isn't persisted — it resets to `ThemeMode::Dark`
on every restart. There is no settings UI, no settings config file, and no established
pattern in this codebase for opening a second OS window (`main.rs` opens exactly one, at
startup). `AppConfig` (`src/config.rs`) holds only `connections`/`groups`, persisted to
`connections.toml` — app-level settings don't belong in that file.

## Decisions (confirmed with user)

- **Standalone OS window**, not an in-main-window modal — matches nyaterm, doesn't cover
  the active terminal session. Confirmed technically straightforward: `App::open_window`
  (`gpui/src/app.rs:1136`) can be called at any time, not just at startup, and
  `gpui-component`'s own examples (`examples/color_mix_oklab.rs`) open a second window with
  the exact `cx.open_window(WindowOptions { .. }, |window, cx| { let view = cx.new(..);
  cx.new(|cx| Root::new(view, window, cx)) })` shape `main.rs` already uses for the main
  window. This same recipe is what the later new-connection and quick-command-editor
  windows will reuse.
- **Only one settings window at a time.** `Workspace` gains an
  `Option<WindowHandle<SettingsWindow>>` (or equivalent `AnyWindowHandle`); the menu action
  checks whether it's already open (and the handle still valid) and focuses/activates it
  instead of opening a duplicate.
- **Persistence: a new `settings.toml`**, not folded into `connections.toml`. New
  `src/settings.rs` module mirrors `config.rs`'s existing shape exactly: `AppSettings`
  struct, `settings_path()` (`$XDG_CONFIG_HOME/caracal/settings.toml`, same `$HOME/.config`
  fallback as `config_path()`), `load()` (missing file / parse error → `AppSettings::default()`,
  logged, never crashes), `save()` (create parent dir, `toml::to_string_pretty`, write).
- **Trigger: "设置..." added to the existing "文件" (File) menu** in `header.rs` — that menu
  bar already exists (File / View / Terminal / Help) and already hosts workspace-level
  actions; no new icon or menu needed.
- **Left sidebar: 3 flat tabs, no category grouping** (nyaterm's nested category headers are
  overkill for 3 items): **General**, **Appearance**, **Terminal**. General and Terminal are
  placeholder tabs for this pass — they render the same "此设置尚未实现" placeholder idiom
  already used by `panels/stub.rs`'s `StubPanel`, with no backing fields — because nothing in
  caracal today has settings for those categories yet (General and Terminal are pre-built
  because the roadmap's later items — quick commands, resource monitor toggles — will need
  exactly those two homes, per the roadmap's stated reasoning). No AI / Sync&Backup / Transfer
  tabs — those correspond to nyaterm features caracal doesn't have and isn't planning per the
  roadmap.
- **Appearance tab is the only real tab this pass**, covering the two things with existing
  backend hooks: font (family + size) and theme mode (dark/light).
- **Draft + Apply/Confirm/Cancel**, matching nyaterm: the window clones committed
  `AppSettings` into local draft state on open; Appearance's controls mutate the draft only.
  Apply writes the draft to `settings.toml` and pushes it live (see propagation below) without
  closing the window; Confirm does the same then closes; Cancel discards the draft and closes
  without persisting or applying anything. No live-preview-per-keystroke — changes take
  effect only on Apply/Confirm, consistent with nyaterm.
- **Font changes broadcast to every already-open terminal tab**, not just new ones.
  `Workspace` gains a `Vec<WeakEntity<TerminalView>>` recording every `TerminalView` it
  creates (`open_local`, `open_local_with`, the SSH/Telnet/Serial constructors — every call
  site that currently does `cx.new(|cx| TerminalView::new...(...))`). On Apply/Confirm, the
  settings window calls back into `Workspace` (via the same `WeakEntity<Workspace>` pattern
  `header.rs` already uses) to iterate that vector, dropping dead weak refs, and calls
  `set_font_config` on each live one. New tabs opened after Apply read the (now-updated)
  committed `AppSettings` as their initial `FontConfig`.
- **Theme mode becomes persisted and single-sourced.** `main.rs` currently hardcodes
  `Theme::change(ThemeMode::Dark, None, cx)` at startup; this becomes "load `settings.toml`,
  use its `theme_mode` if present, else default to Dark." The existing Ctrl+K toggle
  (`main.rs`'s `ToggleTheme` action) keeps working exactly as today (still calls
  `Theme::change` directly, no draft involved — it's a direct global toggle, not a settings
  window field) but now additionally writes the new mode through to `settings.toml` via
  `settings::save`, so both paths (Ctrl+K and Settings → Appearance) agree on one persisted
  value instead of Ctrl+K's choice being silently lost on restart.

## Data model

```rust
// src/settings.rs
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub appearance: AppearanceSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// Empty string = bundled default (`terminal::view::DEFAULT_FONT_FAMILY`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `set_font_size` takes `gpui::Pixels`, so callers convert
    /// via `px(settings.appearance.font_size)` — `Pixels` itself isn't
    /// (de)serializable, hence storing the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// "dark" | "light".
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
}
```

(`General`/`Terminal` get no struct fields yet — they're placeholder tabs with no data;
fields get added to `AppSettings` when those sub-projects need them, not speculatively now.)

## Component structure

- `src/settings.rs` — `AppSettings`/`AppearanceSettings`, `settings_path`/`load`/`save`. Pure
  data + I/O, no gpui imports (same layering rule `config.rs` already follows).
- `src/panels/settings_window.rs` — `SettingsWindow` (the gpui `Render`able root view for the
  second window): left tab-list sidebar, right content area swapping on selected tab, footer
  (Cancel / Apply / Confirm). Holds `committed: AppSettings` and `draft: AppSettings`; a
  `workspace: WeakEntity<Workspace>` callback handle for propagation. An `AppearanceTab`
  sub-view (or inline render function, matching whichever is less boilerplate given only one
  real tab exists) holds the font-family text input, font-size stepper, and theme-mode
  toggle, all writing into `draft.appearance`.
- `src/panels/header.rs` — File menu gets a new `PopupMenuItem::new("设置...")` calling back
  into `Workspace` to open-or-focus the settings window.
- `src/workspace.rs` — new `settings_window: Option<WindowHandle<SettingsWindow>>` field +
  `open_settings` method (open-or-focus); new `terminal_views: Vec<WeakEntity<TerminalView>>`
  field, pushed to at every `TerminalView` creation site; new `apply_font_settings` (or
  similar) method the settings window calls on Apply/Confirm, which broadcasts
  `set_font_config` to every live weak ref and prunes dead ones.
- `src/main.rs` — startup reads `settings::load()` and uses its `theme_mode` instead of the
  hardcoded `ThemeMode::Dark`; the existing `ToggleTheme` action handler additionally calls
  `settings::save` with the new mode.

## UI conventions

- **Boolean on/off settings use `gpui_component::switch::Switch`** (a pill-shaped toggle,
  filled with `cx.theme().primary` when checked), not a custom text pill/button (`"已启用"` /
  `"已禁用"`). The resource-monitor "启用/禁用" toggle originally used a hand-rolled pill
  button; it was switched to `Switch` on 2026-07-13 and is the reference implementation —
  see `SettingsWindow::monitor_enabled_switch` in `settings_window.rs`. Every future
  on/off setting added to this settings window (or any other settings surface in the app)
  should use `Switch` the same way, not reinvent a pill/checkbox look.

## Testing

- `src/settings.rs`: unit tests mirroring `config.rs`'s existing test shape — missing file →
  default, corrupt file → default (logged, doesn't panic), round-trip serialize/deserialize
  preserves all fields, and a `#[serde(default)]` backward-compatibility test (old/partial
  TOML still deserializes, matching `config.rs`'s
  `old_config_without_new_fields_still_deserializes` test).
- The window/UI wiring (`settings_window.rs`, `workspace.rs` broadcast logic) isn't unit-tested
  — consistent with the rest of the codebase, which doesn't test gpui `render()` methods, only
  pure logic. Verification for that part is a manual run (open Settings, change font/theme,
  Apply, confirm an already-open tab's font changes, confirm a newly-opened tab uses the new
  default, restart the app and confirm the theme persisted).

## Non-goals

- No General or Terminal tab *content* — placeholder only, per the confirmed decision above.
- No live-preview-while-typing for font/size — only on Apply/Confirm.
- No settings search, no keyboard-shortcuts editor, no import/export — none of that exists
  in caracal today and none of it is needed to unblock the later roadmap items.
