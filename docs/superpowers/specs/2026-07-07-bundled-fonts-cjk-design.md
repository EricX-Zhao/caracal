# Bundled terminal fonts: JetBrains Mono default + Sarasa Mono SC CJK fallback

Date: 2026-07-07
Files under change: `src/main.rs`, `src/terminal/view.rs`, `assets/fonts/`.

## Background

Chinese text renders garbled (mojibake/tofu) in the terminal on Windows. Two
root causes in `src/terminal/view.rs`:

1. `system_monospace_family()` only resolves a real font name on Linux (via
   `fc-match`). On Windows/macOS it falls through to the literal string
   `"monospace"`, which is not a real font family name — resolution fails or
   degrades to whatever gpui/font-kit substitutes.
2. `FontConfig::default()`'s fallback chain only contains the bundled Nerd
   Font symbol font (`SYMBOLS_NERD_FONT_MONO`, registered in `main.rs`). There
   is no CJK font anywhere in the chain, so Chinese glyphs the primary font
   lacks have nowhere to fall back to.

This follows the existing pattern already used for the Nerd Font symbol font:
bundle a font's bytes into the binary via `include_bytes!`, register it with
`cx.text_system().add_fonts()` in `main.rs` so it lives in the same fontdb
cosmic-text shapes with, and reference it by family name from `FontConfig`.

## Decisions (confirmed with user)

- **Bundle two more fonts**, both SIL OFL 1.1 licensed (compatible with
  bundling as a standalone asset in an MIT/Apache-2.0 project — the license
  applies to the font file, not the code):
  - **JetBrains Mono Regular** (`JetBrainsMono-Regular.ttf`, ~274 KB) becomes
    the new hardcoded default primary terminal font, replacing the
    `system_monospace_family()` auto-detect call in `FontConfig::default()`.
    This reverses an earlier project decision to never hardcode the terminal
    font (see `zed-gpui-git-migration` memory) — the user explicitly chose
    this over patching just the Windows branch, since it also gives a
    consistent look across all three platforms.
  - **Sarasa Mono SC Regular** (`SarasaMonoSC-Regular.ttf`, ~14 MB) is added
    to the default fallback chain as the CJK glyph source, after the Nerd
    Font symbol fallback.
- **Only the Regular weight of each is bundled** (no Bold). Bold terminal text
  keeps using `Font::bold()` (`terminal/render.rs`'s `bold_font`), which
  synthesizes/faux-bolds glyphs missing a true bold face. Accepted trade-off:
  bold CJK looks slightly worse than a true bold face would, but terminal
  text is rarely bold, and this avoids doubling the CJK font's ~14 MB again.
- **`system_monospace_family()` is kept, not deleted**, and gets a real
  Windows branch (hardcoded `"Consolas"`, present since Windows Vista) instead
  of the literal `"monospace"` fallback. It's still reachable via
  `TerminalView::set_font_family("")`, the documented "reset to system font"
  path for a future settings UI — that path should resolve a real family name
  on Windows too, not just Linux.
- **macOS is not touched.** Its `system_monospace_family()` fallback stays the
  literal `"monospace"` string for the reset-to-system path; no report of it
  being broken there, and changing default primary font to JetBrains Mono
  means macOS no longer depends on that path for its default rendering either.
- **License files ship alongside each font**: `assets/fonts/OFL-JetBrainsMono.txt`
  and `assets/fonts/OFL-SarasaGothic.txt` (OFL requires the license text
  accompany redistributed copies of the font).

## Implementation

### `assets/fonts/`
- Add `JetBrainsMono-Regular.ttf`, `SarasaMonoSC-Regular.ttf`.
- Add `OFL-JetBrainsMono.txt`, `OFL-SarasaGothic.txt`.

### `src/main.rs`
- Add two `include_bytes!` constants (`JETBRAINS_MONO_REGULAR`,
  `SARASA_MONO_SC_REGULAR`) alongside the existing
  `SYMBOLS_NERD_FONT_MONO`.
- Extend the `add_fonts` call's `Vec` to register all three.

### `src/terminal/view.rs`
- Add `const CJK_FALLBACK: &str = "Sarasa Mono SC";` next to
  `SYMBOL_FALLBACK`.
- Add `const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";`.
- `FontConfig::default()`: `family: DEFAULT_FONT_FAMILY.into()`,
  `fallbacks: vec![SYMBOL_FALLBACK.into(), CJK_FALLBACK.into()]`.
- `system_monospace_family()`: add a `#[cfg(target_os = "windows")]` arm
  returning `"Consolas".into()` before the final literal-`"monospace"`
  fallback (which remains for macOS and as the last resort).

## Non-goals

- No settings UI is added — `set_font_family`/`set_font_size`/`set_font_config`
  already exist for a future one.
- gpui-component UI chrome (panels, buttons) isn't explicitly wired to the CJK
  fallback. Registering the font globally via `add_fonts` makes it available
  in the shared fontdb, so cosmic-text's own coverage-based fallback search
  may pick it up for UI text incidentally, but this isn't specifically tested
  or guaranteed.
- No changes to `terminal/render.rs`'s bold-handling logic.
