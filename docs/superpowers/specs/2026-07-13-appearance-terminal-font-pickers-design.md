# Primary/fallback font pickers for Appearance and Terminal settings

Date: 2026-07-13

Files under change: `src/settings.rs`, `src/panels/settings_window.rs`,
`src/terminal/view.rs`, `src/workspace.rs`.

## Background

Today, font configuration is asymmetric and partly hardcoded:

- **Terminal** already has a user-configurable primary font
  (`TerminalSettings.font_family`, free-text `Input` on the Terminal settings
  tab, `settings_window.rs:322`) plus font size. Its fallback chain, however,
  is **hardcoded** in `terminal/view.rs`: `FontConfig::default()`
  (`view.rs:80-88`) always uses `SYMBOL_FALLBACK = "Symbols Nerd Font"` then
  `CJK_FALLBACK = "Sarasa Mono SC"` (`view.rs:53,58`), with
  `DEFAULT_FONT_FAMILY = "JetBrains Mono"` (`view.rs:64`) as the primary
  default. None of the three are settings-driven.
- **Appearance** has no font concept at all — `Theme.font_family` (a single
  `SharedString`, no fallback field: `gpui-component/crates/ui/src/theme/mod.rs:55`)
  is hardcoded to `"Sarasa Mono SC"` once at startup (`main.rs:100`) and never
  exposed in Settings.
- A latent inconsistency: `Workspace::resolved_font_family` (`workspace.rs:589-595`)
  maps an **empty** settings value to the bundled `"JetBrains Mono"` default,
  which is a *different* empty-string meaning than
  `TerminalView::set_font_family`'s own (`view.rs:630-638`, empty = "detect the
  real system monospace font via `system_monospace_family()`"). The doc
  comment on `resolved_font_family` (`workspace.rs:585-588`) explicitly calls
  the real system-detection path "still-unused" — this spec finally activates
  it (see Decisions, "Terminal primary — retiring `resolved_font_family`").

This spec adds a "首选字体"/"备选字体" (primary/fallback font) pair to the
Appearance tab, and turns Terminal's existing free-text primary field plus its
two previously-hardcoded fallbacks into three equivalent settings, all five
using the same new dropdown control.

## Decisions (confirmed with user)

### Scope: 5 font slots, one shared 4-choice dropdown

- **Terminal** (3 slots): 首选字体 (`font_family`, existing field, reused),
  备选字体 1 (new `font_fallback1`, default `"Symbols Nerd Font"`), 备选字体 2
  (new `font_fallback2`, default `"Sarasa Mono SC"`).
- **Appearance** (2 slots, both new): 首选字体 (`font_family`, default `""` =
  "系统默认" — resolved at runtime, see below), 备选字体 (`font_fallback`,
  default `"Sarasa Mono SC"`).
- **Every one of the 5 fields uses the same fixed 4-choice dropdown**
  (confirmed with user) — no free-text entry anymore, including for
  Terminal's primary font (today a free-text `Input`; this spec removes that
  capability in favor of the fixed list, per the user's explicit choice of
  "下拉选择" covering "终端首选/备选1/备选2，外观首选/备选" as one set). The four
  choices, shared identically by all 5 dropdowns:

  | Stored value (`String`) | Dropdown label |
  |---|---|
  | `""` | 系统默认 |
  | `"JetBrains Mono"` | JetBrains Mono |
  | `"Sarasa Mono SC"` | Sarasa Mono SC |
  | `"Symbols Nerd Font"` | Symbols Nerd Font |

  These three named values are the existing `DEFAULT_FONT_FAMILY`/
  `CJK_FALLBACK`/`SYMBOL_FALLBACK` constants in `terminal/view.rs`, changed
  from private to `pub(crate)` so `settings_window.rs` references the same
  literals rather than duplicating them.

### Terminal fallback chain: additive, not a replacement

- Confirmed with user: the existing two-tier hardcoded fallback (Nerd Font
  icons, then Sarasa Mono SC for CJK) becomes two **independently
  configurable** slots in that same order — `font_fallback1` (icons) is tried
  before `font_fallback2` (CJK), matching today's behavior when both are left
  at their defaults. This is not a single merged "fallback font" setting.
- `terminal::view::FontConfig.fallbacks: Vec<SharedString>` (`view.rs:77`)
  needs no shape change — it's already a `Vec`. Only its *source* changes:
  `FontConfig::default()`'s hardcoded
  `vec![SYMBOL_FALLBACK.into(), CJK_FALLBACK.into()]` is no longer what feeds
  a live `TerminalView`; settings-sourced values do, via a new
  `TerminalView::set_font_fallbacks(&mut self, fallbacks: Vec<SharedString>,
  cx)` setter (alongside the existing `set_font_family`/`set_font_size`,
  `view.rs:623-644`). `Workspace::seed_font_from_settings` and
  `apply_font_settings` (`workspace.rs:600-626`) both gain
  `font_fallback1`/`font_fallback2` parameters (sourced from
  `settings::load()`/the settings draft respectively) and call this new
  setter alongside their existing `set_font_family`/`set_font_size` calls.

### Terminal primary — retiring `resolved_font_family`

- `Workspace::resolved_font_family` (`workspace.rs:589-595`) and its two tests
  (`workspace.rs:1021-1029`) are **removed**. `seed_font_from_settings`
  (`workspace.rs:600-607`) and `apply_font_settings` (`workspace.rs:612-626`)
  call `TerminalView::set_font_family` directly with the raw settings string
  instead of pre-resolving `""` to `"JetBrains Mono"` first.
- **Behavior change, confirmed as intentional:** selecting "系统默认" for
  Terminal's primary font now stores `""` and, on the terminal side, actually
  triggers `TerminalView::set_font_family`'s own existing (previously
  dormant from the UI's perspective) `system_monospace_family()` detection
  (`fc-match monospace` on Linux, `"Consolas"` on Windows) — not the bundled
  `"JetBrains Mono"` default as `""` used to mean. Anyone with an existing
  `settings.toml` that has `font_family = ""` (the shipped default) will see
  their terminal font resolve differently after this change: previously
  bundled JetBrains Mono, now real system-monospace detection. This is the
  whole point of adding a distinct "系统默认" choice next to "JetBrains Mono"
  in the same list — they need to mean different things.

### Appearance primary "系统默认": a new `system_ui_font_family()` helper

- New private fn in `workspace.rs` (near the font-application logic it
  serves), mirroring `terminal/view.rs`'s existing `system_monospace_family()`
  (`view.rs:114-134`) but resolving a **UI** font, not monospace: `fc-match
  sans-serif` on Linux, a hardcoded `"Segoe UI"` on Windows, and — on macOS or
  any detection failure — falls back to gpui-component's own
  `.SystemUIFont` sentinel (the value `Theme::default()` already ships,
  `gpui-component/crates/ui/src/theme/mod.rs:213`), since that sentinel is
  known to resolve correctly there today (per `main.rs:91-99`'s comment, only
  Windows/Linux needed the override).
- Not unit-tested, same as `system_monospace_family()` today (host-dependent
  subprocess call) — covered by manual verification only.

### Appearance font application: `Workspace`'s own `.font()` override

- gpui-component's `Root` (which wraps `Workspace`, `main.rs:140`) renders its
  own top-level `.font_family(cx.theme().font_family.clone())`
  (`gpui-component/crates/ui/src/root.rs:562`) — a single family, no fallback
  field exists on `Theme` at all. This spec does **not** patch the vendored
  `gpui-component` crate.
- Instead, `Workspace::render()`'s own outer `div()` (`workspace.rs:985-988`,
  a *descendant* of `Root`'s element) gets an explicit `.font(Font { family,
  fallbacks })` built the same way `terminal::view::FontConfig::to_font()`
  already does (`view.rs:94-102`: primary family + `FontFallbacks::from_fonts`
  over the fallback list). Per GPUI's normal style cascade, a descendant's
  explicit font setting overrides an ancestor's — so this correctly overrides
  `Root`'s single-family text style for all of `Workspace`'s content (which is
  effectively the entire app UI) without needing any change to
  `gpui-component`.
- `Workspace` gains two new fields, `appearance_font_family: SharedString` and
  `appearance_font_fallback: SharedString`, initialized from
  `settings::load()` in `Workspace::new` (mirroring how `terminal_views`-style
  state is already seeded there) and updated by a new
  `Workspace::apply_appearance_font_settings(family, fallback, cx)` — called
  from `SettingsWindow::apply()` (`settings_window.rs:150-178`) alongside the
  existing `Theme::change(...)` (theme_mode) and `apply_font_settings(...)`
  (terminal) calls, calling `cx.notify()` so the new font takes effect
  immediately, no restart required.
- **Important: these two fields always hold an already-resolved, never-empty
  font name — never the raw `""` "系统默认" sentinel.** `system_ui_font_family()`
  (a subprocess call on Linux) is invoked exactly twice per occurrence: once
  in `Workspace::new` (startup) and once in `apply_appearance_font_settings`
  (Settings → Apply/Confirm) — **never** from `Workspace::render()`. This
  mirrors how `TerminalView::set_font_family` already resolves
  `system_monospace_family()` once, at call time, caching the result in
  `self.font_config.family`, with `render()` only ever reading the
  already-resolved cached value. Re-resolving on every render would spawn a
  subprocess every frame — a real perf bug this spec explicitly avoids by
  resolving eagerly, in the same two places that already write these fields.

### Settings UI: one reusable `font_picker` dropdown, `DropdownButton`-based

- New control built from `gpui_component::button::{Button, DropdownButton}`
  and `gpui_component::menu::{PopupMenu, PopupMenuItem}` — the same
  components already used for `sftp.rs`'s history dropdown
  (`sftp.rs:1305-1332`) and `saved_connections.rs`'s menus. No new widget
  type is introduced, and the generic `Select`/`SearchableList` component
  (`gpui-component/crates/ui/src/select.rs`) is deliberately not used — it's
  built for searchable, dynamically-sized lists and is unnecessary machinery
  for a fixed 4-item choice.
- A private `FontSlot` enum (`TerminalPrimary`, `TerminalFallback1`,
  `TerminalFallback2`, `AppearancePrimary`, `AppearanceFallback`,
  `#[derive(Clone, Copy)]`) identifies which `draft` field a given picker
  reads/writes, so **one** `SettingsWindow::font_picker(&self, label:
  &'static str, slot: FontSlot, cx)` method renders all 5 dropdowns — not 5
  near-duplicate methods. `FontSlot` also supplies a stable ASCII id suffix
  (e.g. `"terminal-primary"`) for the `DropdownButton`'s `ElementId`,
  independent of the (Chinese, and non-unique across tabs — "首选字体" appears
  for both Terminal and Appearance) display label, avoiding an id collision
  that reusing the label text directly would cause.
- The dropdown button's own label shows the current selection's display
  string (from the table above; `""` shows "系统默认"), so no "currently
  selected" checkmark is needed inside the popup menu itself (and the
  underlying `PopupMenuItem::selected` flag is `pub(crate)`-only in
  gpui-component, unavailable from caracal regardless).
- Clicking a menu item calls a new `SettingsWindow::set_font_slot(&mut self,
  slot: FontSlot, value: &str, cx)`, which writes straight into the
  corresponding `self.draft.*` field and `cx.notify()`s — the same
  immediate-draft-mutation pattern `set_theme_mode`/`toggle_monitor_enabled`
  already use (`settings_window.rs:197-205`). Font fields need no validation
  function (unlike `parse_font_size`/`parse_monitor_interval`/
  `parse_scrollback_lines`) since a dropdown can only ever produce one of the
  4 known-valid values.
- **Removed**: `SettingsWindow.font_family_input: Entity<InputState>` — the
  struct field (`settings_window.rs:77`), its construction
  (`settings_window.rs:87-91`), the corresponding entry in the `Self { ... }`
  literal in `new()` (`settings_window.rs:108`), and the line in
  `sync_inputs_to_draft` that reads it (`settings_window.rs:120`) — all
  superseded by the `FontSlot::TerminalPrimary` dropdown.
- Layout: Terminal tab keeps "字号 (px)" as free-text (unaffected — font
  *size* is a separate concern from this spec) and gains "备选字体 1"/"备选字体
  2" groups after the renamed-to-dropdown primary field, in the same
  labeled-group visual idiom already used for every other Terminal field
  (`settings_window.rs:311-323` shows the shape). Appearance tab keeps its
  existing "主题" dark/light pills and gains "首选字体"/"备选字体" groups below
  them.

## Testing

- `settings.rs`: extend `TerminalSettings`'s default/round-trip/
  forward-compat tests (same shape as the existing `scrollback_lines` ones)
  to cover `font_fallback1`/`font_fallback2`; add equivalent new tests for
  `AppearanceSettings.font_family`/`font_fallback` (its `round_trip_preserves_fields`
  struct literal, `settings.rs:138-148`, needs the two new fields added or it
  won't compile).
- `terminal/view.rs`: unit test that `FontConfig::to_font()` builds the
  `Font.fallbacks` chain in `[fallback1, fallback2]` order from
  non-default settings-sourced values (not just the old hardcoded constants).
- `workspace.rs`: remove `resolved_font_family_empty_uses_bundled_default`/
  `resolved_font_family_passes_through_explicit_value` (the function they test
  is deleted); no direct replacement test is added for the delegation to
  `TerminalView::set_font_family` itself, since (a) that path is already
  exercised by existing `TerminalView` tests indirectly and (b) the actual OS
  detection (`system_monospace_family`/`system_ui_font_family`) is
  host-dependent and, consistent with today's untested
  `system_monospace_family`, left to manual verification.
- `settings_window.rs`: no new `parse_*` unit tests needed (dropdowns have no
  free-text parsing); manual verification instead confirms each of the 5
  dropdowns shows the right default, offers all 4 choices, and persists
  correctly across Apply/Cancel.
- Manual verification (the GUI-driving limits from earlier sessions still
  apply — this needs the user's own click-through): change each of the 5
  font pickers away from its default, Apply, confirm the terminal's primary/
  fallback rendering and the app chrome's font both visibly change without a
  restart; confirm "系统默认" resolves to a real detected font on this machine
  for both Terminal and Appearance; confirm Cancel discards unsaved picks;
  confirm a fresh `settings.toml` (or one missing these keys) still loads with
  the documented defaults.
