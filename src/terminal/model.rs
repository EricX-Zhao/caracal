//! alacritty_terminal `Term` wrapper + listener.
//!
//! This is the terminal *model* layer. It owns the alacritty `Term` behind the
//! `Arc<FairMutex<Term<_>>>` pattern (the listener wraps a channel sender). That
//! `Arc<Mutex>` is alacritty's own required pattern and is the single sanctioned
//! exception to the "few Arc<Mutex>" rule (see CLAUDE.md §2).

use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use gpui::{Hsla, Rgba};

/// Shared handle to the terminal model. Cloned across the GPUI thread, the PTY
/// feeder thread, and the term listener.
pub type SharedTerm = Arc<FairMutex<Term<EventProxy>>>;

/// The `Term` listener. A thin newtype around a `flume` sender; every event the
/// terminal produces (title changes, PTY write-backs for query responses, bell,
/// …) is forwarded to the bridge drain task on the GPUI side.
#[derive(Clone)]
pub struct EventProxy(pub flume::Sender<Event>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        // Best-effort: if the receiver is gone the view is being torn down.
        let _ = self.0.send(event);
    }
}

/// Terminal dimensions for `Term::new` / `Term::resize`. alacritty grows the
/// scrollback itself based on `Config::scrolling_history`, so `total_lines`
/// just mirrors the visible screen here.
#[derive(Clone, Copy, Debug)]
pub struct TermDimensions {
    pub columns: usize,
    pub screen_lines: usize,
}

impl TermDimensions {
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns: columns.max(2),
            screen_lines: screen_lines.max(1),
        }
    }
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Build a fresh shared terminal at the given size.
pub fn new_term(cols: usize, rows: usize, events: flume::Sender<Event>) -> SharedTerm {
    let config = Config {
        scrolling_history: 10_000,
        ..Config::default()
    };
    let dims = TermDimensions::new(cols, rows);
    let term = Term::new(config, &dims, EventProxy(events));
    Arc::new(FairMutex::new(term))
}

// ---------------------------------------------------------------------------
// ANSI color resolution
//
// Independent of the gpui-component theme (CLAUDE.md §2: the two palettes never
// mix). The terminal owns its own palette; OSC overrides stored on the `Term`'s
// `Colors` table take precedence, falling back to these defaults.
// ---------------------------------------------------------------------------

pub const DEFAULT_FG: Rgb = Rgb {
    r: 0xcc,
    g: 0xcc,
    b: 0xcc,
};
pub const DEFAULT_BG: Rgb = Rgb {
    r: 0x0c,
    g: 0x0c,
    b: 0x0c,
};

/// Standard xterm 16-color base palette.
const BASE16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // black
    (0xcc, 0x00, 0x00), // red
    (0x4e, 0x9a, 0x06), // green
    (0xc4, 0xa0, 0x00), // yellow
    (0x34, 0x65, 0xa4), // blue
    (0x75, 0x50, 0x7b), // magenta
    (0x06, 0x98, 0x9a), // cyan
    (0xd3, 0xd7, 0xcf), // white
    (0x55, 0x57, 0x53), // bright black
    (0xef, 0x29, 0x29), // bright red
    (0x8a, 0xe2, 0x34), // bright green
    (0xfc, 0xe9, 0x4f), // bright yellow
    (0x72, 0x9f, 0xcf), // bright blue
    (0xad, 0x7f, 0xa8), // bright magenta
    (0x34, 0xe2, 0xe2), // bright cyan
    (0xee, 0xee, 0xec), // bright white
];

fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// Default RGB for an indexed (0..256) color when the terminal hasn't overridden it.
fn indexed_default(i: u8) -> Rgb {
    match i {
        0..=15 => {
            let (r, g, b) = BASE16[i as usize];
            rgb(r, g, b)
        }
        16..=231 => {
            // 6x6x6 color cube.
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let conv = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
            rgb(conv(r), conv(g), conv(b))
        }
        232..=255 => {
            // Grayscale ramp.
            let level = 8 + (i - 232) * 10;
            rgb(level, level, level)
        }
    }
}

/// Default RGB for a named color.
fn named_default(name: NamedColor) -> Rgb {
    use NamedColor::*;
    match name {
        Black => indexed_default(0),
        Red => indexed_default(1),
        Green => indexed_default(2),
        Yellow => indexed_default(3),
        Blue => indexed_default(4),
        Magenta => indexed_default(5),
        Cyan => indexed_default(6),
        White => indexed_default(7),
        BrightBlack => indexed_default(8),
        BrightRed => indexed_default(9),
        BrightGreen => indexed_default(10),
        BrightYellow => indexed_default(11),
        BrightBlue => indexed_default(12),
        BrightMagenta => indexed_default(13),
        BrightCyan => indexed_default(14),
        BrightWhite => indexed_default(15),
        Foreground | BrightForeground => DEFAULT_FG,
        Background => DEFAULT_BG,
        Cursor => DEFAULT_FG,
        // Dim variants: fall back to their normal counterparts.
        DimBlack => indexed_default(0),
        DimRed => indexed_default(1),
        DimGreen => indexed_default(2),
        DimYellow => indexed_default(3),
        DimBlue => indexed_default(4),
        DimMagenta => indexed_default(5),
        DimCyan => indexed_default(6),
        DimWhite => indexed_default(7),
        DimForeground => DEFAULT_FG,
    }
}

fn rgb_to_hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Resolve an alacritty cell `Color` to a paintable gpui `Hsla`, consulting the
/// terminal's live color table (OSC overrides) before the static defaults.
pub fn resolve_color(color: Color, colors: &alacritty_terminal::term::color::Colors) -> Hsla {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => colors[i as usize].unwrap_or_else(|| indexed_default(i)),
        Color::Named(name) => colors[name].unwrap_or_else(|| named_default(name)),
    };
    rgb_to_hsla(rgb)
}

pub fn default_bg_hsla() -> Hsla {
    rgb_to_hsla(DEFAULT_BG)
}
