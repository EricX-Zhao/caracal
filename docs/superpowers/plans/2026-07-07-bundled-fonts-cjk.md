# Bundled Fonts (JetBrains Mono + Sarasa Mono SC CJK) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix garbled Chinese text on Windows by bundling a cross-platform
default terminal font (JetBrains Mono) and a CJK fallback font (Sarasa Mono
SC) into the binary, replacing the broken per-OS "system monospace"
auto-detection as the default.

**Architecture:** Same pattern already used for the bundled Nerd Font symbol
font: font bytes go in `assets/fonts/`, get `include_bytes!`'d into `main.rs`,
and get registered into gpui's text system via `cx.text_system().add_fonts()`
so cosmic-text's fontdb can find and shape them. `terminal/view.rs`'s
`FontConfig` then references the fonts by family name as the default primary
family and as an added fallback.

**Tech Stack:** Rust, gpui (git rev pinned in `Cargo.toml`), cosmic-text (via
gpui_platform's wgpu renderer).

## Global Constraints

- Only the Regular weight of each new font is bundled — no Bold. (Design spec
  decision — bold terminal text keeps using synthetic bold via
  `Font::bold()`.)
- Both fonts are SIL OFL 1.1 licensed; the license text must ship alongside
  each font file in `assets/fonts/`.
- `system_monospace_family()` is kept (not deleted) — it backs
  `TerminalView::set_font_family("")`, the "reset to system font" path for a
  future settings UI — and gets a real Windows branch instead of the current
  literal `"monospace"` fallback.
- macOS's `system_monospace_family()` fallback is not touched.
- Full spec: `docs/superpowers/specs/2026-07-07-bundled-fonts-cjk-design.md`.

---

### Task 1: Add bundled font assets

**Files:**
- Create: `assets/fonts/JetBrainsMono-Regular.ttf`
- Create: `assets/fonts/SarasaMonoSC-Regular.ttf`
- Create: `assets/fonts/OFL-JetBrainsMono.txt`
- Create: `assets/fonts/OFL-SarasaGothic.txt`

**Interfaces:**
- Produces: two font files on disk with confirmed family names
  `"JetBrains Mono"` and `"Sarasa Mono SC"` (verified via each file's `name`
  table), which Task 2 registers and Task 3 references by string.

- [ ] **Step 1: Download JetBrains Mono v2.304 and extract the Regular TTF + license**

```bash
mkdir -p /tmp/font-fetch/jbmono && cd /tmp/font-fetch/jbmono
curl -sL --max-time 60 -o jb.zip \
  "https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip"
unzip -o -j jb.zip "fonts/ttf/JetBrainsMono-Regular.ttf" "OFL.txt" -d .
```

Expected: `JetBrainsMono-Regular.ttf` (~274 KB) and `OFL.txt` (~4.3 KB) appear
in `/tmp/font-fetch/jbmono/`.

- [ ] **Step 2: Download Sarasa Gothic v1.0.40 and extract the Mono SC Regular TTF**

```bash
mkdir -p /tmp/font-fetch/sarasa && cd /tmp/font-fetch/sarasa
curl -sL --max-time 120 -o sarasa.7z \
  "https://github.com/be5invis/Sarasa-Gothic/releases/download/v1.0.40/SarasaMonoSC-TTF-Unhinted-1.0.40.7z"
7z e sarasa.7z SarasaMonoSC-Regular.ttf -y
curl -s --max-time 15 -o OFL.txt \
  "https://raw.githubusercontent.com/be5invis/Sarasa-Gothic/master/LICENSE"
```

Expected: `SarasaMonoSC-Regular.ttf` (~14 MB) and `OFL.txt` in
`/tmp/font-fetch/sarasa/`.

- [ ] **Step 3: Verify family names via the font's own `name` table**

```bash
python3 -c "
from fontTools.ttLib import TTFont
for path in ('/tmp/font-fetch/jbmono/JetBrainsMono-Regular.ttf', '/tmp/font-fetch/sarasa/SarasaMonoSC-Regular.ttf'):
    f = TTFont(path)
    for rec in f['name'].names:
        if rec.nameID == 1 and rec.platformID == 3:
            print(path, '->', rec.toUnicode())
            break
"
```

Expected output (two lines):
```
/tmp/font-fetch/jbmono/JetBrainsMono-Regular.ttf -> JetBrains Mono
/tmp/font-fetch/sarasa/SarasaMonoSC-Regular.ttf -> Sarasa Mono SC
```

If either name differs, use the printed name (not the filename) everywhere in
Tasks 2-3.

- [ ] **Step 4: Copy fonts and licenses into the repo**

```bash
cd /home/eric/code/caracal
cp /tmp/font-fetch/jbmono/JetBrainsMono-Regular.ttf assets/fonts/
cp /tmp/font-fetch/jbmono/OFL.txt assets/fonts/OFL-JetBrainsMono.txt
cp /tmp/font-fetch/sarasa/SarasaMonoSC-Regular.ttf assets/fonts/
cp /tmp/font-fetch/sarasa/OFL.txt assets/fonts/OFL-SarasaGothic.txt
ls -la assets/fonts/
```

Expected: `assets/fonts/` now has 6 files — the existing
`SymbolsNerdFontMono-Regular.ttf`, plus the 4 new ones above.

- [ ] **Step 5: Commit**

```bash
git add assets/fonts/JetBrainsMono-Regular.ttf assets/fonts/SarasaMonoSC-Regular.ttf \
  assets/fonts/OFL-JetBrainsMono.txt assets/fonts/OFL-SarasaGothic.txt
git commit -m "assets: bundle JetBrains Mono + Sarasa Mono SC fonts"
```

---

### Task 2: Register the new fonts with gpui's text system

**Files:**
- Modify: `src/main.rs:28-32` (bundled-font constants), `src/main.rs:76-81`
  (the `add_fonts` call)

**Interfaces:**
- Consumes: `assets/fonts/JetBrainsMono-Regular.ttf`,
  `assets/fonts/SarasaMonoSC-Regular.ttf` (Task 1).
- Produces: both fonts registered in gpui's text system at startup, under
  family names `"JetBrains Mono"` and `"Sarasa Mono SC"`, which Task 3's
  `FontConfig` references by those exact strings.

- [ ] **Step 1: Add the two new `include_bytes!` constants**

In `src/main.rs`, current state (lines 28-32):

```rust
/// Symbol font bundled into the binary and registered with the text system, so
/// Nerd Font glyphs resolve from the *same* fontdb cosmic-text shapes with
/// (system-installed copies in `~/.local/share/fonts` are not reliably scanned).
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");
```

Replace with:

```rust
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
```

- [ ] **Step 2: Register both fonts in the `add_fonts` call**

Current state (`src/main.rs:76-81`):

```rust
        if let Err(e) = cx
            .text_system()
            .add_fonts(vec![Cow::Borrowed(SYMBOLS_NERD_FONT_MONO)])
        {
            log::warn!("failed to register bundled symbol font: {e}");
        }
```

Replace with:

```rust
        if let Err(e) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(SYMBOLS_NERD_FONT_MONO),
            Cow::Borrowed(JETBRAINS_MONO_REGULAR),
            Cow::Borrowed(SARASA_MONO_SC_REGULAR),
        ]) {
            log::warn!("failed to register bundled fonts: {e}");
        }
```

- [ ] **Step 3: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -30`
Expected: builds successfully (warnings about unrelated pre-existing code are
fine; no errors about `JETBRAINS_MONO_REGULAR` / `SARASA_MONO_SC_REGULAR` /
missing files).

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: register bundled JetBrains Mono + Sarasa Mono SC with text system"
```

---

### Task 3: Make JetBrains Mono the default primary font, add Sarasa Mono SC to the fallback chain, fix Windows system-font resolution

**Files:**
- Modify: `src/terminal/view.rs:50-109` (constants, `FontConfig::default()`,
  `system_monospace_family()`)
- Test: `src/terminal/view.rs` (new `#[cfg(test)] mod tests` at end of file)

**Interfaces:**
- Consumes: family names `"JetBrains Mono"`, `"Sarasa Mono SC"` (Task 2 —
  registered in the text system at startup).
- Produces: `FontConfig::default()` — `family: "JetBrains Mono"`,
  `fallbacks: ["Symbols Nerd Font", "Sarasa Mono SC"]` — consumed by
  `TerminalView::new` (existing call site, unchanged) and by
  `FontConfig::to_font()` (existing method, unchanged) which
  `terminal/render.rs` and `TerminalView::render` already call.

- [ ] **Step 1: Write the failing test for the new defaults**

Current end of `src/terminal/view.rs` (line 621) is the closing brace of
`impl Render for TerminalView`. Append a new test module after it:

```rust

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_font_config_uses_bundled_fonts() {
        let config = FontConfig::default();
        assert_eq!(config.family.as_ref(), "JetBrains Mono");
        assert_eq!(
            config.fallbacks,
            vec![
                SharedString::from(SYMBOL_FALLBACK),
                SharedString::from(CJK_FALLBACK),
            ]
        );
    }

    #[test]
    fn to_font_carries_fallback_chain() {
        let config = FontConfig::default();
        let font = config.to_font();
        assert_eq!(font.family.as_ref(), "JetBrains Mono");
        assert!(font.fallbacks.is_some());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib terminal::view::tests -- --nocapture`
Expected: FAIL — `CJK_FALLBACK` not found in this scope (doesn't exist yet),
and/or `config.family` is not `"JetBrains Mono"` (still calls
`system_monospace_family()`).

- [ ] **Step 3: Add the `CJK_FALLBACK` and `DEFAULT_FONT_FAMILY` constants**

Current state (`src/terminal/view.rs:50-52`):

```rust
/// The bundled symbol font (registered in `main`) used as the default fallback so
/// Nerd Font / powerline glyphs resolve even when the primary font lacks them.
const SYMBOL_FALLBACK: &str = "Symbols Nerd Font";
```

Replace with:

```rust
/// The bundled symbol font (registered in `main`) used as the default fallback so
/// Nerd Font / powerline glyphs resolve even when the primary font lacks them.
const SYMBOL_FALLBACK: &str = "Symbols Nerd Font";

/// The bundled CJK font (registered in `main`) used as a fallback so Chinese
/// glyphs resolve even on a system with no East Asian fonts installed (the
/// original cause of Windows mojibake — see the design spec).
const CJK_FALLBACK: &str = "Sarasa Mono SC";

/// The bundled default primary terminal font (registered in `main`). Hardcoded
/// rather than relying on per-OS "system monospace" detection, which had no
/// working implementation on Windows/macOS (see `system_monospace_family`,
/// kept below for the explicit "reset to system font" path).
const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";
```

- [ ] **Step 4: Update `FontConfig::default()` to use the new constants**

Current state (`src/terminal/view.rs:66-74`):

```rust
impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: system_monospace_family(),
            size: px(14.0),
            fallbacks: vec![SYMBOL_FALLBACK.into()],
        }
    }
}
```

Replace with:

```rust
impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: DEFAULT_FONT_FAMILY.into(),
            size: px(14.0),
            fallbacks: vec![SYMBOL_FALLBACK.into(), CJK_FALLBACK.into()],
        }
    }
}
```

- [ ] **Step 5: Add the Windows branch to `system_monospace_family()`**

Current state (`src/terminal/view.rs:91-109`):

```rust
/// The system's default monospace family. We resolve it ourselves (via
/// fontconfig on Linux) because gpui doesn't map the generic `"monospace"`
/// alias. Falls back to `"monospace"` if detection fails.
fn system_monospace_family() -> SharedString {
    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = std::process::Command::new("fc-match")
            .args(["-f", "%{family[0]}", "monospace"])
            .output()
            && out.status.success()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name.into();
            }
        }
    }
    "monospace".into()
}
```

Replace with:

```rust
/// The system's default monospace family, used by
/// `TerminalView::set_font_family("")` to reset away from the bundled default
/// (see `DEFAULT_FONT_FAMILY`). We resolve it ourselves (via fontconfig on
/// Linux, hardcoded on Windows) because gpui doesn't map the generic
/// `"monospace"` alias to a real family name on either platform — the literal
/// string `"monospace"` is not a font and fails to resolve. Falls back to the
/// literal string on macOS/detection failure (unreported there so far).
fn system_monospace_family() -> SharedString {
    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = std::process::Command::new("fc-match")
            .args(["-f", "%{family[0]}", "monospace"])
            .output()
            && out.status.success()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name.into();
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        return "Consolas".into();
    }
    #[allow(unreachable_code)]
    "monospace".into()
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib terminal::view::tests -- --nocapture`
Expected: PASS — both `default_font_config_uses_bundled_fonts` and
`to_font_carries_fallback_chain` succeed.

- [ ] **Step 7: Run the full test suite to check for regressions**

Run: `cargo test 2>&1 | tail -40`
Expected: all existing tests (`grid_snapshot`, `keymap`, `serial`, `telnet`,
etc.) still pass; no new failures.

- [ ] **Step 8: Commit**

```bash
git add src/terminal/view.rs
git commit -m "feat: default to bundled JetBrains Mono + Sarasa Mono SC CJK fallback"
```

---

### Task 4: Build verification

**Files:** none (verification only)

**Interfaces:**
- Consumes: the fully wired binary from Tasks 1-3.

- [ ] **Step 1: Full release build**

Run: `cargo build --release 2>&1 | tail -30`
Expected: builds successfully. Note the binary size increase (~14 MB from
Sarasa Mono SC, ~0.3 MB from JetBrains Mono) is expected per the design spec.

- [ ] **Step 2: Run the app and visually confirm on this (Linux) machine**

Run: `cargo run --release`
In the opened terminal window, run a command that prints Chinese, e.g.:
```bash
echo "你好，世界 — 中文字体测试"
```
Expected: Chinese characters render as proper glyphs (Sarasa Mono SC), not
tofu boxes or mojibake. Latin text renders in JetBrains Mono. Nerd Font /
powerline icons (if visible in the shell prompt) still render correctly.

- [ ] **Step 3: Note the Windows-specific verification that can't run here**

This plan's Linux run confirms the fallback chain and font registration work
end-to-end. It does **not** exercise the `#[cfg(target_os = "windows")]`
branch in `system_monospace_family()` (only reachable via
`set_font_family("")`, which nothing currently calls) or prove the original
bug is fixed on an actual Windows machine — flag this to the user as
follow-up verification they'll need to do themselves on Windows, since no
Windows machine/CI is available in this environment.
