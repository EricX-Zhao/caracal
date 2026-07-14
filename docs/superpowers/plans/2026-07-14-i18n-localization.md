# i18n localization (Chinese + English) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every user-facing string in the app renders in the user's chosen
language (Chinese or English), switchable live from Settings → General,
persisted across restarts.

**Architecture:** `rust_i18n` (direct dependency, already transitively
present via gpui-component with no conflict — verified) with one
`locales/app.yml` (v2 nested format), one `i18n!()` call in `main.rs`, and
every hardcoded Chinese string literal replaced with a `t!(...)` call.
Language selection lives in a new `GeneralSettings.language` field,
persisted the same way every other setting already is, applied via
`rust_i18n::set_locale(...)` + `cx.refresh_windows()` — the exact mechanism
already proven for the theme-staleness fix earlier this session.

**Tech Stack:** `rust-i18n = "4"`, `t!()` macro (verified: converts to
`gpui::SharedString` via a plain `.into()`), YAML locale file.

## Global Constraints

- Full conversion in one pass — every hardcoded Chinese UI string across
  all ~16 affected files, not a partial first cut.
- Chinese and English only.
- Chinese-language code **comments** are out of scope — never touch them,
  only string literals that are actually rendered to the user.
- Every per-file task ends with a verification grep:
  `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' <file>` must return nothing
  (a stray hit there means a UI string was missed — comments don't match
  this pattern since they aren't inside `"..."`).

---

### Task 1: Dependency, `i18n!()` setup, `GeneralSettings`, startup wiring, proof-of-concept

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs`
- Modify: `src/settings.rs`
- Modify: `src/panels/header.rs` (the one-string proof of concept)
- Create: `locales/app.yml`

**Interfaces:**
- Produces: `rust_i18n::t!(...)` usable from any module in the crate;
  `AppSettings.general: GeneralSettings { language: String }`; every later
  task's `t!(...)` calls resolve against `locales/app.yml`.

- [ ] **Step 1: Add the direct dependency**

In `Cargo.toml`, after the `dirs = "6"` line:

```toml
# UI localization (Settings -> General). Already a transitive dependency
# via gpui-component (which uses its own, independent i18n!() call for its
# own built-in strings) — this is our own, separate instance.
rust-i18n = "4"
```

- [ ] **Step 2: Create the locale file with just the proof-of-concept key**

Create `locales/app.yml`:

```yaml
_version: 2
Header:
  brand_tooltip:
    zh-CN: "终端"
    en: "Terminal"
```

(This one key proves the pipeline end-to-end; every later task adds many
more keys to this same file under their own top-level namespace.)

- [ ] **Step 3: Call `i18n!()` once, at the crate root**

In `src/main.rs`, add right after the `use crate::assets::CaracalAssets;`
line:

```rust
use crate::assets::CaracalAssets;

rust_i18n::i18n!("locales", fallback = "zh-CN");
```

- [ ] **Step 4: Add `GeneralSettings` to `settings.rs`**

In `src/settings.rs`, add a new struct (place it before `AppearanceSettings`)
and wire it into `AppSettings`:

```rust
/// General application settings, editable from Settings → General.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneralSettings {
    /// A `rust_i18n` locale code: `"zh-CN"` or `"en"`.
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "zh-CN".to_string()
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
        }
    }
}
```

Change `AppSettings` from:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub terminal: TerminalSettings,
}
```

to:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub terminal: TerminalSettings,
}
```

- [ ] **Step 5: Add tests for `GeneralSettings`, matching this file's existing conventions**

Add to the `#[cfg(test)] mod tests` block in `settings.rs`:

```rust
#[test]
fn default_general_settings_use_zh_cn() {
    let settings = AppSettings::default();
    assert_eq!(settings.general.language, "zh-CN");
}

#[test]
fn round_trip_preserves_general_language() {
    let settings = AppSettings {
        general: GeneralSettings {
            language: "en".to_string(),
        },
        ..AppSettings::default()
    };
    let text = toml::to_string_pretty(&settings).expect("serialize");
    let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
    assert_eq!(parsed.general.language, "en");
}

#[test]
fn old_settings_file_without_general_table_still_deserializes() {
    // Simulates a settings.toml written before GeneralSettings existed.
    let toml_text = r#"
        [appearance]
        theme_name = "Default Dark"

        [terminal]
        font_family = "Consolas"
        font_size = 16.0
    "#;
    let settings: AppSettings =
        toml::from_str(toml_text).expect("old-format settings must still parse");
    assert_eq!(settings.general.language, "zh-CN");
}
```

- [ ] **Step 6: Apply the persisted language at startup**

In `src/main.rs`, right after the existing
`apply_startup_theme(&startup_settings.appearance.theme_name, cx);` line,
add:

```rust
rust_i18n::set_locale(&startup_settings.general.language);
```

- [ ] **Step 7: Convert the proof-of-concept string in `header.rs`**

In `src/panels/header.rs`, find the brand-icon tooltip/label (currently
just the plain terminal icon with no tooltip text attached — check the
brand `div()` child around the module's icon() call). Since the brand mark
today has no visible text (only an icon), use this step to add a tooltip
instead, proving `t!()` works in a real render path:

```rust
.child(
    div()
        .flex_shrink_0()
        .text_color(cx.theme().foreground)
        .child(icon(AppIcon::Terminal))
        .id("header-brand")
        .tooltip(|window, cx| {
            gpui_component::tooltip::Tooltip::new(rust_i18n::t!("Header.brand_tooltip"))
                .build(window, cx)
        }),
)
```

(Exact surrounding code depends on the current state of the brand `child`
block — apply this `t!()` substitution to whatever tooltip/label API is
already there if the file has changed since this plan was written; the
point of this step is proving one real `t!()` call renders correctly in
each language, not the specific brand-icon UI.)

- [ ] **Step 8: Build**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 9: Run tests**

Run: `cargo test settings::`
Expected: all settings tests pass, including the 3 new ones.

- [ ] **Step 10: Manual proof-of-concept check**

Run: `cargo run`, hover the brand icon, confirm the tooltip reads "终端".
Not yet switchable from the UI (Task 2 adds that) — this step only proves
the locale file + `i18n!()` + `t!()` pipeline itself works.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock locales/app.yml src/main.rs src/settings.rs src/panels/header.rs
git commit -m "$(cat <<'EOF'
feat: add rust_i18n infrastructure + GeneralSettings.language

Direct rust-i18n dependency (already transitive via gpui-component,
which uses its own independent i18n!() call — no conflict), one
locales/app.yml, one i18n!() call in main.rs. GeneralSettings.language
persists the chosen locale (defaults to zh-CN); startup calls
set_locale() explicitly since the OS locale — not i18n!()'s fallback
param — is what's active otherwise (verified by a throwaway trial).

One proof-of-concept conversion (header.rs's brand tooltip) proves the
whole pipeline end-to-end before the bulk string-conversion work in
later commits.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Settings General tab — language dropdown

**Files:**
- Modify: `src/panels/settings_window.rs`
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `GeneralSettings.language` (Task 1).
- Produces: a working, live-switchable language control — lets every
  later task's manual verification actually flip languages from the UI
  instead of editing `settings.toml` by hand.

- [ ] **Step 1: Add the General-tab locale keys**

Add to `locales/app.yml` (top level, alongside `Header`):

```yaml
Settings:
  General:
    language_label:
      zh-CN: "语言"
      en: "Language"
    language_zh_cn:
      zh-CN: "简体中文"
      en: "简体中文"
    language_en:
      zh-CN: "English"
      en: "English"
```

(Language *names* in a picker conventionally show each language's own
native name regardless of the current UI language — "简体中文" and
"English" don't get re-translated per-locale, hence identical values on
both sides here.)

- [ ] **Step 2: Replace the General tab's placeholder with a real tab**

In `settings_window.rs`, find:

```rust
SettingsTab::General => self.render_placeholder_tab("General", cx).into_any_element(),
```

Change to:

```rust
SettingsTab::General => self.render_general_tab(cx).into_any_element(),
```

- [ ] **Step 3: Add `render_general_tab` and `set_language`**

Add near `render_appearance_tab`:

```rust
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
```

- [ ] **Step 4: Apply the language on Apply/Confirm**

In `apply()`, right after the existing theme-apply block (the
`ThemeRegistry::global(cx).themes().get(&theme_name)...`/`cx.refresh_windows();`
lines), add:

```rust
rust_i18n::set_locale(&self.draft.general.language);
```

placed *before* the existing `cx.refresh_windows()` call, so one refresh
covers both the theme and language change together (matches this
function's existing "apply everything, then one refresh" shape — don't
add a second `cx.refresh_windows()` call).

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 6: Manual check**

Run: `cargo run`. Open Settings → General, confirm the language dropdown
shows "简体中文" selected, switch to English, Apply, confirm the dropdown
button itself now reads its state correctly and (once later tasks convert
more strings) the rest of the app follows. Switch back to 简体中文.

- [ ] **Step 7: Commit**

```bash
git add src/panels/settings_window.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: language dropdown in Settings -> General

Same DropdownButton idiom as the existing theme dropdown. Applying
calls rust_i18n::set_locale() alongside the existing theme apply, one
shared cx.refresh_windows() covers both — matches the refresh pattern
already proven for theme staleness earlier this session.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Convert the small remaining files (`activity_bar.rs`, `workspace.rs`, `stub.rs`, `config.rs`)

**Files:**
- Modify: `src/panels/activity_bar.rs`, `src/workspace.rs`,
  `src/panels/stub.rs`, `src/config.rs`
- Modify: `locales/app.yml`

**Interfaces:**
- Produces: `ActivityBar.*`, `Workspace.*`, `Stub.*`, `Config.*` locale
  namespaces.

- [ ] **Step 1: Extract every Chinese string literal from each file**

Run, per file, to get the exact current list (this plan doesn't
pre-transcribe all of them — extract fresh, since exact wording is the
source of truth, not this document):

```bash
grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/activity_bar.rs
grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/workspace.rs
grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/stub.rs
grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/config.rs
```

`activity_bar.rs`'s hits are `PanelId::label()`'s match arms (e.g.
`"文件浏览器"`, `"网络"`, `"安全 / 认证"`, `"会话"`, `"命令历史"`,
`"资源监控"`) and `quick_commands_button`/`settings_button`'s tooltip
strings (`"快捷命令"`, `"设置"`). `workspace.rs`'s hits are scattered
status-bar/menu-adjacent strings — read each hit in context before
translating (a couple may be log messages already meant to stay
technical/English-only; only convert strings actually reaching the
rendered UI). `stub.rs`'s one hit is the placeholder body text
(`"此设置尚未实现"`-style). `config.rs`'s one hit is likely a doc-comment
false-positive from the earlier grep pass (comments don't need
conversion) — confirm before touching it.

- [ ] **Step 2: Add corresponding keys to `locales/app.yml`**

One top-level namespace per file: `ActivityBar`, `Workspace`, `Stub`. Skip
`Config` entirely if step 1 confirms its one hit is a comment, not a
string literal. Follow the exact same `zh-CN:`/`en:` nested-key shape as
Task 1/2's examples — key names should describe the string's purpose
(e.g. `ActivityBar.file_explorer`, `ActivityBar.quick_commands_tooltip`),
not the Chinese text itself.

- [ ] **Step 3: Replace each string literal with a `t!(...)` call**

Follow the two shapes from the design spec: plain `t!("Namespace.key")`
for static text (passed directly wherever `impl Into<SharedString>` is
accepted, e.g. `.child(...)`/`.label(...)`), or
`t!("Namespace.key", var = value)` replacing any `format!(...)` call that
embeds a Chinese format string.

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 5: Verify no Chinese string literals remain in these 4 files**

Run:
```bash
grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/activity_bar.rs src/workspace.rs src/panels/stub.rs src/config.rs
```
Expected: no output.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: all passing (these files have no string-related unit tests, but
this confirms nothing else broke).

- [ ] **Step 7: Commit**

```bash
git add src/panels/activity_bar.rs src/workspace.rs src/panels/stub.rs src/config.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert activity_bar/workspace/stub/config strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Convert `panels/quick_commands_panel.rs`

**Files:** Modify `src/panels/quick_commands_panel.rs`, `locales/app.yml`

**Interfaces:** Produces the `QuickCommands.*` namespace.

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/quick_commands_panel.rs` to get the exact current list (~13 strings: panel title, empty-state text, form field labels, Execute/Append mode labels, save/cancel button labels, delete-confirmation text, etc. — read each in context).
- [ ] **Step 2:** Add a `QuickCommands` namespace to `locales/app.yml` with one key per string, same shape as prior tasks.
- [ ] **Step 3:** Replace each with `t!(...)`, using named-variable interpolation for any that currently use `format!(...)`.
- [ ] **Step 4:** `cargo build` — expect success.
- [ ] **Step 5:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/quick_commands_panel.rs` — expect no output.
- [ ] **Step 6:** `cargo test` — expect all passing.
- [ ] **Step 7:** Commit:
```bash
git add src/panels/quick_commands_panel.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert quick_commands_panel.rs strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Convert `panels/monitor.rs`

**Files:** Modify `src/panels/monitor.rs`, `locales/app.yml`

**Interfaces:** Produces the `Monitor.*` namespace.

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/monitor.rs` (~21 strings: CPU/内存/网络/磁盘 labels, units, placeholder/disabled-state text, column headers, etc.).
- [ ] **Step 2:** Add a `Monitor` namespace to `locales/app.yml`.
- [ ] **Step 3:** Replace each with `t!(...)` (interpolate any `format!()`-built stat strings, e.g. percentage/byte-rate displays, using named variables).
- [ ] **Step 4:** `cargo build` — expect success.
- [ ] **Step 5:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/monitor.rs` — expect no output.
- [ ] **Step 6:** `cargo test panels::monitor` — expect all passing (this file has real unit tests for `compute_stats`; confirm the string changes didn't touch that logic).
- [ ] **Step 7:** Commit:
```bash
git add src/panels/monitor.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert monitor.rs strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Convert `terminal/view.rs`

**Files:** Modify `src/terminal/view.rs`, `locales/app.yml`

**Interfaces:** Produces the `Terminal.*` namespace (connection-banner
text — "connecting...", "failed: {reason}", etc.).

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/terminal/view.rs` (~7 strings — check `conn_banner_text`-style helpers; this file already has unit tests asserting exact banner text, e.g. `conn_banner_text_failed_includes_reason` from earlier this session — those tests will need their expected strings updated to match whatever the *current* locale renders, or rewritten to assert against the `zh-CN` locale key's value explicitly).
- [ ] **Step 2:** Add a `Terminal` namespace to `locales/app.yml`.
- [ ] **Step 3:** Replace each with `t!(...)`, interpolating the failure-reason variant.
- [ ] **Step 4:** Update the existing banner-text unit tests (`terminal::view::tests::conn_banner_text_*`) to assert against `rust_i18n::t!("Terminal....")` output (with the locale explicitly set to `"zh-CN"` at the top of the test, via `rust_i18n::set_locale("zh-CN")`, since test execution order shouldn't depend on whatever locale a previous test left active — tests run in the same process and share `rust_i18n`'s global current-locale state).
- [ ] **Step 5:** `cargo build` — expect success.
- [ ] **Step 6:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/terminal/view.rs` — expect no output (comments aside).
- [ ] **Step 7:** `cargo test terminal::view` — expect all passing.
- [ ] **Step 8:** Commit:
```bash
git add src/terminal/view.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert terminal/view.rs connection-banner strings

Existing banner-text unit tests now pin rust_i18n::set_locale("zh-CN")
explicitly before asserting, since locale is process-global state.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Convert the rest of `panels/settings_window.rs`

**Files:** Modify `src/panels/settings_window.rs`, `locales/app.yml`

**Interfaces:** Produces the `Settings.Appearance.*`/`Settings.Terminal.*`/
`Settings.*` (shared: Cancel/Apply/Confirm footer, tab labels, validation
error messages) namespaces. `Settings.General.*` already exists from Task 2.

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/settings_window.rs` — Task 2 already converted ~3 of these; the remaining ~20 cover: tab labels (though `SettingsTab::label()` currently returns English strings like `"General"`/`"Appearance"`/`"Terminal"` already — confirm whether those need a Chinese translation too, i.e. whether the tab bar itself should be localized or was already deliberately English; if the latter, leave `SettingsTab::label()` untouched and only convert genuinely-Chinese strings), font-picker labels (首选字体/备选字体/字号/etc.), validation error messages (from `parse_font_size`/`parse_monitor_interval`/`parse_scrollback_lines`'s callers), footer button labels (取消/应用/确定), monitor-settings labels, empty/error states.
- [ ] **Step 2:** Add the remaining keys under `Settings.Appearance`, `Settings.Terminal`, and a catch-all `Settings.*` for shared footer/validation strings.
- [ ] **Step 3:** Replace each with `t!(...)`.
- [ ] **Step 4:** `cargo build` — expect success.
- [ ] **Step 5:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/settings_window.rs` — expect no output.
- [ ] **Step 6:** `cargo test` — expect all passing.
- [ ] **Step 7:** Commit:
```bash
git add src/panels/settings_window.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert the rest of settings_window.rs strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Convert `panels/sessions.rs`

**Files:** Modify `src/panels/sessions.rs`, `locales/app.yml`

**Interfaces:** Produces the `Sessions.*` namespace (largest single
UI surface after SFTP and the new-connection window: panel header/toolbar,
empty state, context-menu items, group/connection row labels, drag-and-drop
affordances, delete-confirmation text).

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/sessions.rs` (~40 strings).
- [ ] **Step 2:** Add a `Sessions` namespace to `locales/app.yml`, grouped with sub-comments in the YAML by UI area (header/toolbar, tree rows, context menus, dialogs) for readability given the size.
- [ ] **Step 3:** Replace each with `t!(...)`, interpolating any per-connection dynamic text (e.g. numbered tab-name suffixes, item counts).
- [ ] **Step 4:** `cargo build` — expect success.
- [ ] **Step 5:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/sessions.rs` — expect no output.
- [ ] **Step 6:** `cargo test` — expect all passing.
- [ ] **Step 7:** Commit:
```bash
git add src/panels/sessions.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert sessions.rs strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Convert `panels/new_connection_window.rs`

**Files:** Modify `src/panels/new_connection_window.rs`, `locales/app.yml`

**Interfaces:** Produces the `NewConnectionWindow.*` namespace (form field
labels/placeholders for SSH/Local/Telnet/Serial connection types, per-type
validation errors, icon-picker labels, save/cancel buttons).

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/new_connection_window.rs` (~50 strings — the largest per-file count after `sftp.rs`).
- [ ] **Step 2:** Add a `NewConnectionWindow` namespace, grouped by connection-type sub-section (SSH/Local/Telnet/Serial/Common) in the YAML for readability.
- [ ] **Step 3:** Replace each with `t!(...)`.
- [ ] **Step 4:** `cargo build` — expect success.
- [ ] **Step 5:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/new_connection_window.rs` — expect no output.
- [ ] **Step 6:** `cargo test` — expect all passing.
- [ ] **Step 7:** Commit:
```bash
git add src/panels/new_connection_window.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert new_connection_window.rs strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Convert `panels/sftp.rs`

**Files:** Modify `src/panels/sftp.rs`, `locales/app.yml`

**Interfaces:** Produces the `Sftp.*` namespace (largest single file:
toolbar buttons/tooltips, path bar, file-list column headers, status row
— including the `format!("共 {count} 项 | {}", ...)` interpolation from
the design spec's worked example — transfer list, context menu, rename/new
folder/new file inline forms, download-dir bar, hidden-files toggle,
delete-confirmation text).

- [ ] **Step 1:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/sftp.rs` (~61 strings — the largest single file).
- [ ] **Step 2:** Add an `Sftp` namespace, grouped by UI area (toolbar, path bar, file list, status row, transfers, context menu, dialogs) for readability given the size.
- [ ] **Step 3:** Replace each with `t!(...)`. The status-row `format!` call becomes:
  ```rust
  let summary = t!("Sftp.status_row", count = count, size = human_size(total_bytes));
  ```
  matching the design spec's worked example exactly.
- [ ] **Step 4:** `cargo build` — expect success.
- [ ] **Step 5:** `grep -noP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/panels/sftp.rs` — expect no output.
- [ ] **Step 6:** `cargo test` — expect all passing.
- [ ] **Step 7:** Commit:
```bash
git add src/panels/sftp.rs locales/app.yml
git commit -m "$(cat <<'EOF'
feat: i18n — convert sftp.rs strings

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: Full-project verification pass

**Files:** none (verification only — fixes for anything found go here
before the final commit).

- [ ] **Step 1: Whole-tree sweep for any missed string literal**

Run:
```bash
grep -rnoP '"[^"]*[\x{4e00}-\x{9fff}][^"]*"' src/
```
Expected: no output. If anything remains, it was missed by an earlier
task — add the missing locale key and `t!(...)` call, following that
file's existing namespace.

- [ ] **Step 2: Full build + test**

Run: `cargo build && cargo test`
Expected: clean build, all tests passing.

- [ ] **Step 3: Manual bilingual smoke test (Linux, this session)**

Run: `cargo run`. Starting in 简体中文 (default), click through: header
brand tooltip, both activity bars (all icons' tooltips), Sessions panel
(header/toolbar/empty state/context menu), SFTP panel (toolbar/status
row/transfers), quick-commands drawer, resource monitor, Settings (all 3
tabs incl. General), the new-connection window. Then Settings → General →
English → Apply, confirm every one of those same surfaces re-renders in
English without a restart (this is exactly what `cx.refresh_windows()`
buys us — verify it actually does, the same way the theme-refresh fix was
verified earlier). Switch back to 简体中文, restart the app, confirm it
reopens in 简体中文 (persisted, not reset).

- [ ] **Step 4: Report results to the user**

Summarize what was verified, and flag anything deferred (e.g. if step 1's
sweep found strings not cleanly attributable to any of tasks 3-10's files,
list where they ended up and why).
