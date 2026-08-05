//! `SettingsWindow`: a standalone second window (File → "设置...") for
//! application-level settings. Follows nyaterm's draft + Apply/Confirm/Cancel
//! model: [`SettingsWindow`] clones the committed [`crate::settings::AppSettings`]
//! into a local draft on open; nothing is written to `settings.toml` or applied
//! live until Apply or Confirm.

/// Parse the Terminal tab's font-size text field. Rejects non-finite,
/// non-positive, and unreasonably large values (a typo like "1400" should not
/// silently make every tab's text 100x too big).
fn parse_font_size(text: &str) -> Option<f32> {
    let value: f32 = text.trim().parse().ok()?;
    if value.is_finite() && value >= 6.0 && value <= 96.0 {
        Some(value)
    } else {
        None
    }
}

/// Parse the Terminal tab's monitor-poll-interval field. Rejects
/// non-positive and unreasonably small/large values (a 0-second interval
/// would spin the poll loop; a typo like "99999" would poll once every 27
/// hours, effectively never).
fn parse_monitor_interval(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1..=3600).contains(&value) {
        Some(value)
    } else {
        None
    }
}

/// Parse the Terminal tab's scrollback-lines field. Rejects non-integer and
/// out-of-range values — 1,000 keeps a minimally useful history, 50,000 caps
/// memory use for a single tab's grid.
fn parse_scrollback_lines(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1_000..=50_000).contains(&value) {
        Some(value)
    } else {
        None
    }
}

/// Parse the Backup tab's keep-versions field. Rejects non-positive and
/// unreasonably large values — 1 keeps just the latest backup, 100 is a
/// generous upper bound (more than that is almost certainly a typo).
fn parse_keep_versions(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1..=100).contains(&value) {
        Some(value)
    } else {
        None
    }
}

/// Check the 12 configurable shortcuts' *resolved* keys (override or
/// default, via `keybindings::effective_key`) for a duplicate. Returns
/// `(key, first_action_id, second_action_id)` for the first collision
/// found (table order), or `None` if every resolved key is unique. O(n^2)
/// over exactly 12 entries — a HashMap-based dedup isn't worth the extra
/// code at this fixed, tiny size.
fn find_keybinding_conflict(
    overrides: &std::collections::HashMap<String, String>,
) -> Option<(String, &'static str, &'static str)> {
    let resolved: Vec<(&'static str, String)> = keybindings::DEFAULT_KEYBINDINGS
        .iter()
        .map(|(id, _)| (*id, keybindings::effective_key(id, overrides).unwrap_or_default()))
        .collect();
    for i in 0..resolved.len() {
        for j in (i + 1)..resolved.len() {
            if resolved[i].1 == resolved[j].1 {
                return Some((resolved[i].1.clone(), resolved[i].0, resolved[j].0));
            }
        }
    }
    None
}

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, prelude::FluentBuilder, px, red, transparent_black,
};
use gpui_component::button::{Button, ButtonVariants, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::notification::NotificationType;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Sizable, Theme, ThemeMode, ThemeRegistry, WindowExt};

use crate::keyring_store::SecretStore;
use crate::panels::keybindings;
use crate::settings::{self, AppSettings};
use crate::terminal::view::{CJK_FALLBACK, DEFAULT_FONT_FAMILY, SYMBOL_FALLBACK};
use crate::workspace::Workspace;

/// The 4 choices every font-family dropdown offers. `""` means "detect a
/// system font at apply/seed time" — see `TerminalView::set_font_family`/
/// `set_font_fallbacks` and `Workspace::apply_appearance_font_settings`
/// (neither dropdown resolves it here; the picker only ever stores one of
/// these 4 raw values). Just the raw values, not (value, display-label)
/// pairs — every non-empty value's display label is identical to the value
/// itself (font family names aren't translated), so `font_choice_label`
/// derives it instead of duplicating it here.
const FONT_CHOICES: &[&str] = &["", DEFAULT_FONT_FAMILY, CJK_FALLBACK, SYMBOL_FALLBACK];

/// Display label for one `FONT_CHOICES` value: the localized "system
/// default" sentinel for `""`, or the font family name itself otherwise.
fn font_choice_label(value: &str) -> SharedString {
    if value.is_empty() {
        SharedString::from(rust_i18n::t!("Settings.system_default"))
    } else {
        SharedString::from(value.to_string())
    }
}

/// Localized display label for one of the 12 configurable shortcut
/// action-ids (see `panels::keybindings::DEFAULT_KEYBINDINGS`). Falls back
/// to the raw id for anything unrecognized (shouldn't happen — every
/// caller sources ids from that same table).
fn shortcut_action_label(action_id: &str) -> SharedString {
    match action_id {
        "new_tab" => rust_i18n::t!("Settings.Shortcuts.new_tab").into(),
        "close_tab" => rust_i18n::t!("Settings.Shortcuts.close_tab").into(),
        "next_tab" => rust_i18n::t!("Settings.Shortcuts.next_tab").into(),
        "prev_tab" => rust_i18n::t!("Settings.Shortcuts.prev_tab").into(),
        "new_connection" => rust_i18n::t!("Settings.Shortcuts.new_connection").into(),
        "toggle_left_sidebar" => rust_i18n::t!("Settings.Shortcuts.toggle_left_sidebar").into(),
        "toggle_right_sidebar" => rust_i18n::t!("Settings.Shortcuts.toggle_right_sidebar").into(),
        "toggle_quick_commands" => rust_i18n::t!("Settings.Shortcuts.toggle_quick_commands").into(),
        "open_settings" => rust_i18n::t!("Settings.Shortcuts.open_settings").into(),
        "zoom_in" => rust_i18n::t!("Settings.Shortcuts.zoom_in").into(),
        "zoom_out" => rust_i18n::t!("Settings.Shortcuts.zoom_out").into(),
        "clear_screen" => rust_i18n::t!("Settings.Shortcuts.clear_screen").into(),
        _ => SharedString::from(action_id.to_string()),
    }
}

/// Human-readable form of a raw gpui binding string (e.g.
/// `"secondary-shift-t"` -> `"Ctrl+Shift+T"` on Windows/Linux, `"⌘+Shift+T"`
/// on macOS) — what the Shortcuts tab actually displays, instead of gpui's
/// internal syntax. Reuses gpui's own `Keystroke::parse` (rather than
/// hand-splitting on `-`) specifically because a couple of this feature's
/// own default keys use `-`/`=`/`,` as the literal final key (e.g.
/// `zoom_out`'s default `"secondary--"`), which a naive split on `-` can't
/// disambiguate from a modifier separator — `Keystroke::parse` already
/// handles that correctly.
fn format_binding_for_display(binding: &str) -> String {
    let Ok(ks) = gpui::Keystroke::parse(binding) else {
        return binding.to_string();
    };
    let mut parts = Vec::new();
    if ks.modifiers.secondary() {
        parts.push(if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" }.to_string());
    }
    if ks.modifiers.alt {
        parts.push(if cfg!(target_os = "macos") { "⌥" } else { "Alt" }.to_string());
    }
    if ks.modifiers.shift {
        parts.push("Shift".to_string());
    }
    let key_label = if ks.key.chars().count() == 1 {
        ks.key.to_uppercase()
    } else {
        let mut chars = ks.key.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => ks.key.clone(),
        }
    };
    parts.push(key_label);
    parts.join("+")
}

/// Identifies which font-setting field a `SettingsWindow::font_picker`
/// dropdown reads/writes.
#[derive(Clone, Copy)]
enum FontSlot {
    TerminalPrimary,
    TerminalFallback1,
    TerminalFallback2,
    AppearancePrimary,
    AppearanceFallback,
}

impl FontSlot {
    /// Stable ASCII id suffix for the dropdown's `ElementId` — independent
    /// of the (Chinese, and non-unique across tabs — "首选字体" appears for
    /// both Terminal and Appearance) display label.
    fn id_suffix(self) -> &'static str {
        match self {
            FontSlot::TerminalPrimary => "terminal-primary",
            FontSlot::TerminalFallback1 => "terminal-fallback1",
            FontSlot::TerminalFallback2 => "terminal-fallback2",
            FontSlot::AppearancePrimary => "appearance-primary",
            FontSlot::AppearanceFallback => "appearance-fallback",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Appearance,
    Terminal,
    Security,
    Shortcuts,
    Backup,
}

impl SettingsTab {
    /// Stable ASCII id for this tab's element id — independent of
    /// `label()`'s (possibly Chinese) display text, so switching the
    /// language live doesn't change the sidebar buttons' element ids.
    fn id_key(self) -> &'static str {
        match self {
            SettingsTab::General => "general",
            SettingsTab::Appearance => "appearance",
            SettingsTab::Terminal => "terminal",
            SettingsTab::Security => "security",
            SettingsTab::Shortcuts => "shortcuts",
            SettingsTab::Backup => "backup",
        }
    }

    fn label(self) -> SharedString {
        match self {
            SettingsTab::General => rust_i18n::t!("Settings.tab_general").into(),
            SettingsTab::Appearance => rust_i18n::t!("Settings.tab_appearance").into(),
            SettingsTab::Terminal => rust_i18n::t!("Settings.tab_terminal").into(),
            SettingsTab::Security => rust_i18n::t!("Settings.tab_security").into(),
            SettingsTab::Shortcuts => rust_i18n::t!("Settings.tab_shortcuts").into(),
            SettingsTab::Backup => rust_i18n::t!("Settings.tab_backup").into(),
        }
    }
}

pub struct SettingsWindow {
    workspace: WeakEntity<Workspace>,
    committed: AppSettings,
    draft: AppSettings,
    active_tab: SettingsTab,
    font_size_input: Entity<InputState>,
    monitor_interval_input: Entity<InputState>,
    scrollback_input: Entity<InputState>,
    webdav_url_input: Entity<InputState>,
    webdav_username_input: Entity<InputState>,
    webdav_password_input: Entity<InputState>,
    keep_versions_input: Entity<InputState>,
    /// True while any WebDAV action (test/backup/refresh/restore) is in
    /// flight — disables every backup-tab action button so a second click
    /// can't start an overlapping operation.
    backup_busy: bool,
    /// Populated by "刷新列表" — empty until the user has fetched at least
    /// once (not auto-fetched on tab open, since that would silently hit
    /// the network every time Settings opens).
    backup_versions: Vec<crate::webdav::BackupVersion>,
    error: Option<SharedString>,
    /// The action-id currently being recorded (`Record` button clicked,
    /// waiting for the next keystroke), or `None` when nothing is being
    /// recorded.
    recording: Option<String>,
    /// Focus target for capturing the next keystroke while recording —
    /// created once in `new`, (re)focused via `window.focus(...)` each
    /// time `Record` is clicked.
    record_focus: FocusHandle,
}

impl SettingsWindow {
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let committed = settings::load();
        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.terminal.font_size.to_string())
        });
        let monitor_interval_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.terminal.monitor_basic_interval_secs.to_string())
        });
        let scrollback_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.terminal.scrollback_lines.to_string())
        });
        let webdav_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.backup.webdav_url.clone())
                .placeholder("https://dav.example.com/remote.php/dav/files/me/caracal-backups/")
        });
        let webdav_username_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.backup.webdav_username.clone())
        });
        // Decrypt the saved password (if any) to re-populate the field on
        // open, mirroring how every other draft field seeds from
        // `committed`. `None` (vault locked, or nothing saved yet, or a
        // corrupt value) just leaves the field empty rather than erroring
        // — the user can always re-type it.
        let initial_webdav_password = cx
            .try_global::<crate::workspace::VaultKey>()
            .and_then(|key| key.0.decrypt_str(&committed.backup.encrypted_webdav_password).ok())
            .unwrap_or_default();
        let webdav_password_input = cx.new(|cx| {
            InputState::new(window, cx).masked(true).default_value(initial_webdav_password)
        });
        let keep_versions_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.backup.keep_versions.to_string())
        });
        Self {
            workspace,
            draft: committed.clone(),
            committed,
            active_tab: SettingsTab::Appearance,
            font_size_input,
            monitor_interval_input,
            scrollback_input,
            webdav_url_input,
            webdav_username_input,
            webdav_password_input,
            keep_versions_input,
            backup_busy: false,
            backup_versions: Vec::new(),
            error: None,
            recording: None,
            record_focus: cx.focus_handle(),
        }
    }

    /// Read both inputs into the draft, validating font size. Returns `false`
    /// (and sets `self.error`) without mutating the draft further if the
    /// font-size field doesn't parse.
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        let size_text = self.font_size_input.read(cx).value();
        let Some(size) = parse_font_size(&size_text) else {
            self.error = Some(rust_i18n::t!("Settings.font_size_invalid").into());
            return false;
        };
        self.draft.terminal.font_size = size;

        let interval_text = self.monitor_interval_input.read(cx).value();
        let Some(interval) = parse_monitor_interval(&interval_text) else {
            self.error = Some(rust_i18n::t!("Settings.monitor_interval_invalid").into());
            return false;
        };
        self.draft.terminal.monitor_basic_interval_secs = interval;

        let scrollback_text = self.scrollback_input.read(cx).value();
        let Some(scrollback_lines) = parse_scrollback_lines(&scrollback_text) else {
            self.error = Some(rust_i18n::t!("Settings.scrollback_lines_invalid").into());
            return false;
        };
        self.draft.terminal.scrollback_lines = scrollback_lines;

        self.draft.backup.webdav_url = self.webdav_url_input.read(cx).value().to_string();
        self.draft.backup.webdav_username = self.webdav_username_input.read(cx).value().to_string();

        // Leaving the password field empty keeps whatever was already
        // saved (a freshly-decrypted field is legitimately empty when
        // nothing's been configured yet, or when the vault is locked —
        // that must not silently wipe a previously-saved password).
        let webdav_password_text = self.webdav_password_input.read(cx).value().to_string();
        if !webdav_password_text.is_empty() {
            match cx.try_global::<crate::workspace::VaultKey>() {
                Some(key) => {
                    self.draft.backup.encrypted_webdav_password = key.0.encrypt_str(&webdav_password_text);
                }
                None => {
                    self.error = Some(rust_i18n::t!("Settings.Backup.vault_locked_error").into());
                    return false;
                }
            }
        }

        let keep_versions_text = self.keep_versions_input.read(cx).value();
        let Some(keep_versions) = parse_keep_versions(&keep_versions_text) else {
            self.error = Some(rust_i18n::t!("Settings.Backup.keep_versions_invalid").into());
            return false;
        };
        self.draft.backup.keep_versions = keep_versions;

        self.error = None;
        true
    }

    /// Persist the draft, apply it live (theme immediately; font broadcast to
    /// every open tab via `Workspace`), and update `committed`. Returns
    /// `false` (leaving the window open with `self.error` set) on validation
    /// or save failure.
    fn apply(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.sync_inputs_to_draft(cx) {
            cx.notify();
            return false;
        }

        if let Some((key, _first, second)) =
            find_keybinding_conflict(&self.draft.keybindings.overrides)
        {
            self.error = Some(
                rust_i18n::t!(
                    "Settings.Shortcuts.conflict_error",
                    key = key,
                    action = shortcut_action_label(second)
                )
                .into(),
            );
            cx.notify();
            return false;
        }

        if let Err(e) = settings::save(&self.draft) {
            log::error!("failed to save settings: {e}");
            self.error = Some(rust_i18n::t!("Settings.save_failed").into());
            cx.notify();
            return false;
        }

        // Look up the exact theme by name (not just a light/dark mode) — see
        // `theme_dropdown`. Falls back to the built-in Default Dark theme if
        // the name isn't registered (shouldn't normally happen: the dropdown
        // only ever offers names straight from `ThemeRegistry`).
        let theme_name = SharedString::from(self.draft.appearance.theme_name.clone());
        match ThemeRegistry::global(cx).themes().get(&theme_name).cloned() {
            Some(theme) => Theme::global_mut(cx).apply_config(&theme),
            None => Theme::change(ThemeMode::Dark, None, cx),
        }
        rust_i18n::set_locale(&self.draft.general.language);
        // `Theme::change`/`apply_config` only refresh the window they're
        // given (none, here), so force every open window to repaint or
        // panels that don't redraw for other reasons keep showing stale
        // colors/text — covers both the theme and language change above.
        cx.refresh_windows();

        let font_family = self.draft.terminal.font_family.clone();
        let font_size = px(self.draft.terminal.font_size);
        let font_fallback1 = self.draft.terminal.font_fallback1.clone();
        let font_fallback2 = self.draft.terminal.font_fallback2.clone();
        let appearance_font_family = self.draft.appearance.font_family.clone();
        let appearance_font_fallback = self.draft.appearance.font_fallback.clone();
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.apply_font_settings(font_family, font_size, font_fallback1, font_fallback2, cx);
            workspace.apply_appearance_font_settings(
                appearance_font_family,
                appearance_font_fallback,
                cx,
            );
        });

        // Keyboard shortcuts: re-register only the actions whose resolved
        // key actually changed since `committed` — appending a fresh
        // `KeyBinding` is immediate and safe (see `keybindings.rs`'s doc
        // comment: gpui's own precedence rule means a later-added binding
        // shadows an earlier one for the same keystroke+context, so this
        // never needs `clear_key_bindings()` and never touches
        // gpui-component's own internal bindings).
        let mut changed: Vec<(&'static str, String)> = Vec::new();
        let mut suppressed: Vec<(&'static str, String)> = Vec::new();
        for (action_id, _) in keybindings::DEFAULT_KEYBINDINGS {
            let old_key =
                keybindings::effective_key(action_id, &self.committed.keybindings.overrides);
            let new_key =
                keybindings::effective_key(action_id, &self.draft.keybindings.overrides);
            if old_key != new_key {
                if let Some(new_key) = new_key {
                    changed.push((action_id, new_key));
                }
                if let Some(old_key) = old_key {
                    if !keybindings::FIXED_KEYS.contains(&old_key.as_str()) {
                        suppressed.push((action_id, old_key));
                    }
                }
            }
        }
        if !changed.is_empty() || !suppressed.is_empty() {
            let mut bindings: Vec<gpui::KeyBinding> = suppressed
                .iter()
                .map(|(action_id, old_key)| keybindings::suppress_key(action_id, old_key))
                .collect();
            bindings.extend(keybindings::build_key_bindings_for(&changed));
            cx.bind_keys(bindings);
        }

        self.committed = self.draft.clone();
        cx.notify();
        true
    }

    fn on_click_cancel(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = self.committed.clone();
        self.error = None;
        window.remove_window();
        cx.notify();
    }

    fn on_click_apply(&mut self, _ev: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply(cx);
    }

    fn on_click_confirm(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.apply(cx) {
            window.remove_window();
        }
    }

    fn set_theme_name(&mut self, name: &str, cx: &mut Context<Self>) {
        self.draft.appearance.theme_name = name.to_string();
        cx.notify();
    }

    fn toggle_monitor_enabled(&mut self, cx: &mut Context<Self>) {
        self.draft.terminal.monitor_basic_enabled = !self.draft.terminal.monitor_basic_enabled;
        cx.notify();
    }

    fn font_slot_value(&self, slot: FontSlot) -> &str {
        match slot {
            FontSlot::TerminalPrimary => &self.draft.terminal.font_family,
            FontSlot::TerminalFallback1 => &self.draft.terminal.font_fallback1,
            FontSlot::TerminalFallback2 => &self.draft.terminal.font_fallback2,
            FontSlot::AppearancePrimary => &self.draft.appearance.font_family,
            FontSlot::AppearanceFallback => &self.draft.appearance.font_fallback,
        }
    }

    fn set_font_slot(&mut self, slot: FontSlot, value: &str, cx: &mut Context<Self>) {
        let field = match slot {
            FontSlot::TerminalPrimary => &mut self.draft.terminal.font_family,
            FontSlot::TerminalFallback1 => &mut self.draft.terminal.font_fallback1,
            FontSlot::TerminalFallback2 => &mut self.draft.terminal.font_fallback2,
            FontSlot::AppearancePrimary => &mut self.draft.appearance.font_family,
            FontSlot::AppearanceFallback => &mut self.draft.appearance.font_fallback,
        };
        *field = value.to_string();
        cx.notify();
    }

    /// One "首选/备选" font dropdown: a text label above, a `DropdownButton`
    /// below showing the current choice's display name and offering all of
    /// `FONT_CHOICES`. Shared by all 5 font fields across both tabs — see
    /// the design spec for why a fixed dropdown (not free text) is used
    /// here, including for what used to be a free-text field.
    fn font_picker(
        &self,
        label: impl Into<SharedString>,
        slot: FontSlot,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let current = self.font_slot_value(slot).to_string();
        let display = FONT_CHOICES
            .iter()
            .find(|&&value| value == current)
            .map(|&value| font_choice_label(value))
            .unwrap_or_else(|| font_choice_label(""));
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.into()),
            )
            .child(
                DropdownButton::new(SharedString::from(format!(
                    "font-picker-{}",
                    slot.id_suffix()
                )))
                .xsmall()
                .button(
                    Button::new(SharedString::from(format!(
                        "font-picker-btn-{}",
                        slot.id_suffix()
                    )))
                    .xsmall()
                    .label(display),
                )
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu;
                    for &value in FONT_CHOICES {
                        let choice_label = font_choice_label(value);
                        let value = value.to_string();
                        let weak = weak.clone();
                        menu = menu.item(PopupMenuItem::new(choice_label).on_click(
                            move |_ev, _window, cx| {
                                let value = value.clone();
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_font_slot(slot, &value, cx);
                                });
                            },
                        ));
                    }
                    menu
                }),
            )
    }

    /// One sidebar tab button.
    fn tab_button(&self, tab: SettingsTab, cx: &Context<Self>) -> impl IntoElement {
        let active = self.active_tab == tab;
        div()
            .id(SharedString::from(format!("settings-tab-{}", tab.id_key())))
            .px_3()
            .py_1()
            .rounded_sm()
            .bg(if active {
                cx.theme().list_active
            } else {
                transparent_black()
            })
            .text_color(if active {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .hover(|s| s.bg(cx.theme().accent))
            .child(tab.label())
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.active_tab = tab;
                this.recording = None;
                this.error = None;
                cx.notify();
            }))
    }

    /// Theme dropdown: lists every theme in `ThemeRegistry` (the built-in
    /// Default Light/Dark pair plus the bundled gpui-component theme
    /// collection — see `main.rs`'s `BUNDLED_THEMES`); selecting one applies
    /// it immediately by exact name, not just a light/dark mode.
    fn theme_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.draft.appearance.theme_name.clone();
        let weak = cx.entity().downgrade();
        let names: Vec<SharedString> = ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .map(|theme| theme.name.clone())
            .collect();

        DropdownButton::new("settings-theme-picker")
            .xsmall()
            .button(
                Button::new("settings-theme-picker-btn")
                    .xsmall()
                    .label(current),
            )
            .dropdown_menu(move |menu, _window, _cx| {
                // ~40 bundled themes is too many to show at once — cap the
                // popup at roughly 8 rows tall and let it scroll instead.
                let mut menu = menu.max_h(px(280.0)).scrollable(true);
                for name in &names {
                    let name = name.clone();
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                        move |_ev, _window, cx| {
                            let name = name.clone();
                            let _ = weak.update(cx, |this, cx| {
                                this.set_theme_name(&name, cx);
                            });
                        },
                    ));
                }
                menu
            })
    }

    fn render_general_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current_label = if self.draft.general.language == "en" {
            rust_i18n::t!("Settings.General.language_en")
        } else {
            rust_i18n::t!("Settings.General.language_zh_cn")
        };
        let weak = cx.entity().downgrade();

        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(rust_i18n::t!("Settings.General.language_label")),
            )
            .child(
                DropdownButton::new("settings-language-picker")
                    .xsmall()
                    .button(
                        Button::new("settings-language-picker-btn")
                            .xsmall()
                            .label(current_label),
                    )
                    .dropdown_menu(move |menu, _window, _cx| {
                        let weak_zh = weak.clone();
                        let weak_en = weak.clone();
                        menu.item(
                            PopupMenuItem::new(rust_i18n::t!("Settings.General.language_zh_cn"))
                                .on_click(move |_ev, _window, cx| {
                                    let _ = weak_zh.update(cx, |this, cx| {
                                        this.set_language("zh-CN", cx);
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new(rust_i18n::t!("Settings.General.language_en"))
                                .on_click(move |_ev, _window, cx| {
                                    let _ = weak_en.update(cx, |this, cx| {
                                        this.set_language("en", cx);
                                    });
                                }),
                        )
                    }),
            )
    }

    fn set_language(&mut self, language: &str, cx: &mut Context<Self>) {
        self.draft.general.language = language.to_string();
        cx.notify();
    }

    /// UI-level appearance only (theme). The terminal's own font lives on the
    /// Terminal tab (`render_terminal_tab`) — it affects terminal content,
    /// not the application chrome this tab controls.
    fn render_appearance_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.theme_label")),
                    )
                    .child(self.theme_dropdown(cx)),
            )
            .child(self.font_picker(
                rust_i18n::t!("Settings.font_primary"),
                FontSlot::AppearancePrimary,
                cx,
            ))
            .child(self.font_picker(
                rust_i18n::t!("Settings.font_fallback"),
                FontSlot::AppearanceFallback,
                cx,
            ))
    }

    /// Terminal-content settings: currently just font family/size, which only
    /// affect `TerminalView` rendering (see `Workspace::apply_font_settings`),
    /// not the application chrome.
    fn render_terminal_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.font_picker(rust_i18n::t!("Settings.font_primary"), FontSlot::TerminalPrimary, cx))
            .child(self.font_picker(
                rust_i18n::t!("Settings.font_fallback_1"),
                FontSlot::TerminalFallback1,
                cx,
            ))
            .child(self.font_picker(
                rust_i18n::t!("Settings.font_fallback_2"),
                FontSlot::TerminalFallback2,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.font_size_px")),
                    )
                    .child(Input::new(&self.font_size_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.monitor_basic")),
                    )
                    .child(self.monitor_enabled_switch(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.monitor_interval_secs")),
                    )
                    .child(Input::new(&self.monitor_interval_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.scrollback_lines")),
                    )
                    .child(Input::new(&self.scrollback_input)),
            )
    }

    /// The vault's two escape hatches (see
    /// docs/superpowers/specs/2026-07-15-encrypted-credential-storage-design.md):
    /// dropping the OS-keyring convenience-unlock cache, and a full reset
    /// for a forgotten master password. Both act immediately (not gated by
    /// this window's Apply/Confirm/Cancel draft model — there's no "draft"
    /// state for a destructive one-shot action).
    fn render_security_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let vault_unlocked = cx.try_global::<crate::workspace::VaultKey>().is_some();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Security.forget_unlock_label")),
                    )
                    .child(
                        Button::new("settings-forget-unlock")
                            .xsmall()
                            .label(rust_i18n::t!("Vault.forget_unlock_button"))
                            .disabled(!vault_unlocked)
                            .on_click(cx.listener(|_this, _ev: &ClickEvent, window, cx| {
                                if cx.try_global::<crate::workspace::VaultKey>().is_some() {
                                    if let Some(meta) = crate::config::load().vault {
                                        crate::keyring_store::OsSecretStore.clear(&meta.vault_id);
                                    }
                                }
                                window.push_notification(
                                    (NotificationType::Success, rust_i18n::t!("Vault.forget_unlock_done")),
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Security.reset_vault_label")),
                    )
                    .child(
                        Button::new("settings-reset-vault")
                            .xsmall()
                            .danger()
                            .label(rust_i18n::t!("Vault.reset_vault_button"))
                            .on_click(cx.listener(Self::reset_vault)),
                    ),
            )
    }

    /// Double-confirmation (see `SavedConnection` deletion's precedent in
    /// `src/panels/sftp.rs`'s `delete_selected`) since this permanently
    /// destroys every saved password/key.
    fn reset_vault(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            alert
                .title(rust_i18n::t!("Vault.reset_confirm_title"))
                .description(rust_i18n::t!("Vault.reset_confirm_body"))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    window.open_alert_dialog(cx, move |alert, _window, _cx| {
                        alert
                            .title(rust_i18n::t!("Vault.reset_confirm_title_2"))
                            .description(rust_i18n::t!("Vault.reset_confirm_body_2"))
                            .confirm()
                            .on_ok(move |_, window, cx| {
                                window.close_dialog(cx);
                                let mut cfg = crate::config::load();
                                if let Some(meta) = &cfg.vault {
                                    crate::keyring_store::OsSecretStore.clear(&meta.vault_id);
                                }
                                crate::vault::reset(&mut cfg);
                                if let Err(e) = crate::config::save(&cfg) {
                                    log::error!("failed to save reset vault: {e}");
                                }
                                cx.remove_global::<crate::workspace::VaultKey>();
                                window.push_notification(
                                    (NotificationType::Warning, rust_i18n::t!("Vault.reset_done")),
                                    cx,
                                );
                                true
                            })
                    });
                    true
                })
        });
    }

    /// Builds a `WebDavConfig` straight from whatever's currently typed in
    /// the 3 credential fields — every backup action button acts on the
    /// live draft, not on `committed`/`self.draft.backup`, so the user
    /// never has to hit Apply first just to test or use what they just
    /// typed.
    fn current_webdav_config(&self, cx: &Context<Self>) -> crate::webdav::WebDavConfig {
        crate::webdav::WebDavConfig {
            url: self.webdav_url_input.read(cx).value().to_string(),
            username: self.webdav_username_input.read(cx).value().to_string(),
            password: self.webdav_password_input.read(cx).value().to_string(),
        }
    }

    fn on_click_test_connection(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.current_webdav_config(cx);
        if config.url.trim().is_empty() {
            window.push_notification(
                (NotificationType::Error, rust_i18n::t!("Settings.Backup.url_required")),
                cx,
            );
            return;
        }
        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::test_connection(config);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(())) => window.push_notification(
                            (NotificationType::Success, rust_i18n::t!("Settings.Backup.test_success")),
                            cx,
                        ),
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.test_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.test_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn on_click_backup_now(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.current_webdav_config(cx);
        if config.url.trim().is_empty() {
            window.push_notification(
                (NotificationType::Error, rust_i18n::t!("Settings.Backup.url_required")),
                cx,
            );
            return;
        }
        let keep_versions =
            parse_keep_versions(&self.keep_versions_input.read(cx).value()).unwrap_or(5);

        let connections = match std::fs::read(crate::config::config_path()) {
            Ok(bytes) => bytes,
            Err(e) => {
                window.push_notification(
                    (
                        NotificationType::Error,
                        rust_i18n::t!("Settings.Backup.read_local_failed", error = e.to_string()),
                    ),
                    cx,
                );
                return;
            }
        };
        let settings_bytes = match std::fs::read(crate::settings::settings_path()) {
            Ok(bytes) => bytes,
            Err(e) => {
                window.push_notification(
                    (
                        NotificationType::Error,
                        rust_i18n::t!("Settings.Backup.read_local_failed", error = e.to_string()),
                    ),
                    cx,
                );
                return;
            }
        };
        // A missing quick_commands.toml is normal (it's never created
        // until the user adds their first quick command) — an empty
        // member unpacks fine later, since `QuickCommandsFile`'s
        // `commands` field is `#[serde(default)]`.
        let quick_commands_bytes = std::fs::read(crate::quick_commands::quick_commands_path()).unwrap_or_default();

        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::backup_now(config, keep_versions, connections, settings_bytes, quick_commands_bytes);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(filename)) => window.push_notification(
                            (
                                NotificationType::Success,
                                rust_i18n::t!("Settings.Backup.backup_success", filename = filename),
                            ),
                            cx,
                        ),
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.backup_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.backup_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn on_click_refresh_versions(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.current_webdav_config(cx);
        if config.url.trim().is_empty() {
            window.push_notification(
                (NotificationType::Error, rust_i18n::t!("Settings.Backup.url_required")),
                cx,
            );
            return;
        }
        self.backup_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let rx = crate::webdav::list_versions(config);
            let result = rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.backup_busy = false;
                    match result {
                        Ok(Ok(mut versions)) => {
                            versions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)); // newest first
                            this.backup_versions = versions;
                        }
                        Ok(Err(e)) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.list_failed", error = e.to_string()),
                            ),
                            cx,
                        ),
                        Err(_) => window.push_notification(
                            (
                                NotificationType::Error,
                                rust_i18n::t!("Settings.Backup.list_failed", error = "worker thread failed"),
                            ),
                            cx,
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// One row in the version list: the timestamp, human-formatted. The
    /// restore action is added to this row in a later round.
    fn version_row(&self, version: &crate::webdav::BackupVersion, cx: &Context<Self>) -> impl IntoElement {
        let label = version.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_sm().text_color(cx.theme().foreground).child(label))
    }

    /// Draft-state credential fields only — the network action buttons
    /// (test/backup/refresh/restore) are added on top of this in later
    /// rounds, following the Security tab's pattern of immediate-action
    /// buttons coexisting with a tab's draft fields.
    fn render_backup_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let vault_unlocked = cx.try_global::<crate::workspace::VaultKey>().is_some();

        div()
            .flex()
            .flex_col()
            .gap_3()
            .when(!vault_unlocked, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(rust_i18n::t!("Settings.Backup.vault_locked_notice")),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.webdav_url_label")),
                    )
                    .child(Input::new(&self.webdav_url_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.webdav_username_label")),
                    )
                    .child(Input::new(&self.webdav_username_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.webdav_password_label")),
                    )
                    .child(Input::new(&self.webdav_password_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.keep_versions_label")),
                    )
                    .child(Input::new(&self.keep_versions_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Button::new("settings-backup-test")
                            .xsmall()
                            .label(rust_i18n::t!("Settings.Backup.test_button"))
                            .disabled(self.backup_busy)
                            .on_click(cx.listener(Self::on_click_test_connection)),
                    )
                    .child(
                        Button::new("settings-backup-now")
                            .xsmall()
                            .label(rust_i18n::t!("Settings.Backup.backup_now_button"))
                            .disabled(self.backup_busy)
                            .on_click(cx.listener(Self::on_click_backup_now)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(rust_i18n::t!("Settings.Backup.versions_label")),
                            )
                            .child(
                                Button::new("settings-backup-refresh")
                                    .xsmall()
                                    .label(rust_i18n::t!("Settings.Backup.refresh_button"))
                                    .disabled(self.backup_busy)
                                    .on_click(cx.listener(Self::on_click_refresh_versions)),
                            ),
                    )
                    .child(if self.backup_versions.is_empty() {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(rust_i18n::t!("Settings.Backup.versions_empty"))
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(self.backup_versions.iter().map(|v| self.version_row(v, cx)))
                            .into_any_element()
                    }),
            )
    }

    /// One row: action label, current resolved key (override or default) —
    /// or the recording prompt while this row is being captured — plus
    /// Record and Reset buttons.
    fn shortcut_row(&self, action_id: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let current_key =
            keybindings::effective_key(action_id, &self.draft.keybindings.overrides)
                .unwrap_or_default();
        let is_recording = self.recording.as_deref() == Some(action_id);

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_sm().child(shortcut_action_label(action_id)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if is_recording {
                                rust_i18n::t!("Settings.Shortcuts.recording_prompt").to_string()
                            } else {
                                format_binding_for_display(&current_key)
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "settings-shortcut-record-{action_id}"
                        )))
                        .xsmall()
                        .label(rust_i18n::t!("Settings.Shortcuts.record_button"))
                        .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
                            this.start_recording(action_id, window, cx);
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "settings-shortcut-reset-{action_id}"
                        )))
                        .xsmall()
                        .label(rust_i18n::t!("Settings.Shortcuts.reset_button"))
                        .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                            this.reset_shortcut(action_id, cx);
                        })),
                    ),
            )
    }

    fn reset_shortcut(&mut self, action_id: &'static str, cx: &mut Context<Self>) {
        self.draft.keybindings.overrides.remove(action_id);
        cx.notify();
    }

    fn start_recording(
        &mut self,
        action_id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recording = Some(action_id.to_string());
        self.error = None;
        window.focus(&self.record_focus, cx);
        cx.notify();
    }

    /// Handles the next keystroke while `self.recording` is `Some`.
    /// Escape cancels (reverts to whatever key the row had before,
    /// without capturing Escape itself as a binding — standard capture-
    /// widget convention). A bare, unmodified key is rejected inline. A
    /// successful capture stages the canonicalized key string into
    /// `self.draft.keybindings.overrides` — nothing is bound live or
    /// saved to disk here; that only happens on Apply (Task 7).
    fn on_capture_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(action_id) = self.recording.clone() else {
            return;
        };
        let ks = &ev.keystroke;
        if ks.key == "escape" {
            self.recording = None;
            self.error = None;
            cx.notify();
            return;
        }
        if !keybindings::has_modifier(ks) {
            self.error = Some(rust_i18n::t!("Settings.Shortcuts.needs_modifier").into());
            cx.notify();
            return;
        }
        let key_string = keybindings::format_keystroke(ks);
        self.draft.keybindings.overrides.insert(action_id, key_string);
        self.recording = None;
        self.error = None;
        cx.notify();
    }

    fn on_click_reset_all_shortcuts(
        &mut self,
        _ev: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft.keybindings.overrides.clear();
        cx.notify();
    }

    fn render_shortcuts_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let mut rows = div().flex().flex_col().gap_2();
        for (action_id, _) in keybindings::DEFAULT_KEYBINDINGS {
            rows = rows.child(self.shortcut_row(action_id, cx));
        }

        div()
            .track_focus(&self.record_focus)
            .on_key_down(cx.listener(Self::on_capture_key_down))
            .flex()
            .flex_col()
            .gap_3()
            .child(rows)
            .child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}: {} .. {}",
                        rust_i18n::t!("Settings.Shortcuts.goto_tab_label"),
                        format_binding_for_display("secondary-1"),
                        format_binding_for_display("secondary-9")
                    )),
            )
            .child(
                Button::new("settings-shortcuts-reset-all")
                    .xsmall()
                    .label(rust_i18n::t!("Settings.Shortcuts.reset_all_button"))
                    .on_click(cx.listener(Self::on_click_reset_all_shortcuts)),
            )
    }

    /// Standard boolean-toggle style for this settings window: a pill switch
    /// (`gpui_component::switch::Switch`), not a text pill button — see the
    /// "UI conventions" note in `docs/superpowers/specs/2026-07-07-settings-page-design.md`.
    fn monitor_enabled_switch(&self, cx: &Context<Self>) -> impl IntoElement {
        Switch::new("settings-monitor-enabled")
            .checked(self.draft.terminal.monitor_basic_enabled)
            .on_click(cx.listener(|this, _checked: &bool, _window, cx| {
                this.toggle_monitor_enabled(cx);
            }))
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let content = match self.active_tab {
            SettingsTab::General => self.render_general_tab(cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_tab(cx).into_any_element(),
            SettingsTab::Terminal => self.render_terminal_tab(cx).into_any_element(),
            SettingsTab::Security => self.render_security_tab(cx).into_any_element(),
            SettingsTab::Shortcuts => self.render_shortcuts_tab(cx).into_any_element(),
            SettingsTab::Backup => self.render_backup_tab(cx).into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(140.0))
                            .p_2()
                            .border_r_1()
                            .border_color(border)
                            .child(self.tab_button(SettingsTab::General, cx))
                            .child(self.tab_button(SettingsTab::Appearance, cx))
                            .child(self.tab_button(SettingsTab::Terminal, cx))
                            .child(self.tab_button(SettingsTab::Security, cx))
                            .child(self.tab_button(SettingsTab::Shortcuts, cx))
                            .child(self.tab_button(SettingsTab::Backup, cx)),
                    )
                    .child(div().flex_1().p_4().child(content)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(border)
                    .when_some(self.error.clone(), |el, err| {
                        el.child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(red())
                                .child(err),
                        )
                    })
                    .child(
                        div()
                            .id("settings-cancel")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(rust_i18n::t!("Settings.cancel"))
                            .on_click(cx.listener(Self::on_click_cancel)),
                    )
                    .child(
                        div()
                            .id("settings-apply")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(rust_i18n::t!("Settings.apply"))
                            .on_click(cx.listener(Self::on_click_apply)),
                    )
                    .child(
                        div()
                            .id("settings-confirm")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .child(rust_i18n::t!("Settings.confirm"))
                            .on_click(cx.listener(Self::on_click_confirm)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_binding_for_display_renders_modifiers_and_key() {
        // On this dev/CI host (non-macOS), the "secondary" modifier reads
        // as "Ctrl" — see `keybindings.rs`'s tests for the same convention.
        assert_eq!(format_binding_for_display("secondary-shift-t"), "Ctrl+Shift+T");
        assert_eq!(format_binding_for_display("secondary-b"), "Ctrl+B");
        assert_eq!(format_binding_for_display("secondary-tab"), "Ctrl+Tab");
    }

    #[test]
    fn format_binding_for_display_handles_punctuation_keys() {
        // These are exactly the defaults (zoom_out, open_settings, zoom_in)
        // that a naive split-on-'-' can't parse correctly — the key IS a
        // hyphen/comma/equals, not a separator.
        assert_eq!(format_binding_for_display("secondary--"), "Ctrl+-");
        assert_eq!(format_binding_for_display("secondary-,"), "Ctrl+,");
        assert_eq!(format_binding_for_display("secondary-="), "Ctrl+=");
    }

    #[test]
    fn format_binding_for_display_falls_back_to_raw_string_on_parse_failure() {
        // Two unrecognized non-modifier tokens in a row is rejected by
        // `Keystroke::parse` (not a valid `key->key_char` form either).
        assert_eq!(format_binding_for_display("abc-def"), "abc-def");
    }

    #[test]
    fn parses_valid_size() {
        assert_eq!(parse_font_size("14"), Some(14.0));
        assert_eq!(parse_font_size(" 16.5 "), Some(16.5));
    }

    #[test]
    fn rejects_non_numeric() {
        assert_eq!(parse_font_size("abc"), None);
        assert_eq!(parse_font_size(""), None);
    }

    #[test]
    fn rejects_out_of_range() {
        assert_eq!(parse_font_size("0"), None);
        assert_eq!(parse_font_size("-5"), None);
        assert_eq!(parse_font_size("500"), None);
    }

    #[test]
    fn parses_valid_scrollback_lines() {
        assert_eq!(parse_scrollback_lines("10000"), Some(10_000));
        assert_eq!(parse_scrollback_lines(" 1000 "), Some(1_000));
        assert_eq!(parse_scrollback_lines("50000"), Some(50_000));
    }

    #[test]
    fn rejects_out_of_range_scrollback_lines() {
        assert_eq!(parse_scrollback_lines("999"), None);
        assert_eq!(parse_scrollback_lines("50001"), None);
    }

    #[test]
    fn rejects_non_numeric_scrollback_lines() {
        assert_eq!(parse_scrollback_lines("abc"), None);
        assert_eq!(parse_scrollback_lines(""), None);
    }

    #[test]
    fn find_keybinding_conflict_none_when_all_defaults() {
        let overrides = std::collections::HashMap::new();
        assert!(find_keybinding_conflict(&overrides).is_none());
    }

    #[test]
    fn find_keybinding_conflict_detects_a_collision() {
        let mut overrides = std::collections::HashMap::new();
        // toggle_left_sidebar's default is "secondary-b" — collide new_tab into it.
        overrides.insert("new_tab".to_string(), "secondary-b".to_string());
        let conflict = find_keybinding_conflict(&overrides).expect("expected a conflict");
        assert_eq!(conflict.0, "secondary-b");
        assert_eq!(conflict.1, "new_tab");
        assert_eq!(conflict.2, "toggle_left_sidebar");
    }

    #[test]
    fn find_keybinding_conflict_none_when_override_is_still_unique() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("new_tab".to_string(), "secondary-shift-r".to_string());
        assert!(find_keybinding_conflict(&overrides).is_none());
    }
}
