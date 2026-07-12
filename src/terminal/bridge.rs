//! The bridge welds three execution contexts together: the PTY reader thread,
//! the alacritty parser/feeder thread, and the GPUI main thread. It is the
//! *only* place cross-context channels live (CLAUDE.md §2).
//!
//! Two responsibilities:
//!   1. `run_feeder` — a blocking thread that drains raw PTY bytes into the
//!      `Term` through the ANSI parser, batching queued chunks under one lock up
//!      to a bounded budget so the renderer can paint intermediate frames.
//!   2. `run_drain` — a GPUI-side async task that consumes terminal `Event`s and
//!      coalesces redraws: at most one `cx.notify()` per frame (~16ms), so a
//!      `cat` of a huge file can't melt the UI thread.

use std::time::Duration;

use alacritty_terminal::event::Event;
use alacritty_terminal::vte::ansi::Processor;
use gpui::{AsyncApp, WeakEntity};

use crate::terminal::model::SharedTerm;
use crate::terminal::view::TerminalView;

/// Minimum interval between redraws. Bursts of terminal output within one
/// interval collapse into a single `cx.notify()`.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Upper bound on how many bytes the feeder parses while holding the `Term`
/// lock in one go. Without this, a fast producer (e.g. `cat` of a large file)
/// floods `bytes_rx` and the inner drain loop never exits — the whole file is
/// parsed under one lock, the renderer is starved until it finishes, and the
/// grid jumps straight to the final screen ("all at once"). Capping the batch
/// lets the renderer interleave and show the scroll progressively, while still
/// batching enough to stay efficient (the drain task throttles redraws anyway).
const MAX_BATCH_BYTES: usize = 64 * 1024;

/// Run the parser/feeder loop. Blocking; meant to own a dedicated OS thread.
///
/// Reads raw byte chunks from `bytes_rx`, feeds them through the ANSI parser into
/// the shared `Term`, then signals the drain task via an `Event::Wakeup`.
///
/// `closed` fires exactly once, after the loop ends because `bytes_rx` was
/// drained and closed (every backend-side sender dropped — the read side of
/// the transport gave up, whether because the process/session exited or the
/// connection died). `TerminalView` watches this to detect a dead backend —
/// see `view.rs`'s `mark_disconnected`.
pub fn run_feeder(
    term: SharedTerm,
    bytes_rx: flume::Receiver<Vec<u8>>,
    events: flume::Sender<Event>,
    closed: flume::Sender<()>,
) {
    let mut parser: Processor = Processor::new();
    while let Ok(chunk) = bytes_rx.recv() {
        {
            let mut term = term.lock();
            let mut processed = chunk.len();
            parser.advance(&mut *term, &chunk);
            // Coalesce already-queued chunks under the same lock, but only up to
            // a bounded budget so we periodically release the lock and let the
            // renderer paint an intermediate frame (see MAX_BATCH_BYTES).
            while processed < MAX_BATCH_BYTES {
                match bytes_rx.try_recv() {
                    Ok(more) => {
                        processed += more.len();
                        parser.advance(&mut *term, &more);
                    }
                    Err(_) => break,
                }
            }
        }
        if events.send(Event::Wakeup).is_err() {
            break;
        }
    }
    let _ = closed.send(());
}

/// GPUI-side drain task. Consumes terminal events, throttles redraws, and routes
/// PTY write-backs (query responses, clipboard load) to the backend.
///
/// Long-lived across a `TerminalView` reconnect (see `view.rs`'s
/// `reconnect_with`) — it doesn't take a `backend` param, and instead reads
/// `view.backend` fresh each time it needs to write, so a write-back after a
/// reconnect goes to the *current* backend generation, not a stale `Arc`
/// captured before the swap.
pub async fn run_drain(
    view: WeakEntity<TerminalView>,
    events: flume::Receiver<Event>,
    cx: &mut AsyncApp,
) {
    loop {
        // Block until at least one event is available.
        let first = match events.recv_async().await {
            Ok(ev) => ev,
            Err(_) => break, // feeder gone -> tear down
        };

        let mut dirty = false;
        let mut title: Option<String> = None;
        let mut exited = false;
        let mut write_back: Vec<u8> = Vec::new();

        let mut handle = |ev: Event| match ev {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => dirty = true,
            Event::Title(t) => {
                title = Some(t);
                dirty = true;
            }
            Event::ResetTitle => {
                title = Some(String::new());
            }
            Event::PtyWrite(text) => write_back.extend_from_slice(text.as_bytes()),
            Event::ClipboardLoad(_, formatter) => {
                // Paste-on-request: respond with empty clipboard for now (Phase 3
                // wires the real system clipboard).
                write_back.extend_from_slice(formatter("").as_bytes());
            }
            Event::TextAreaSizeRequest(_) | Event::ColorRequest(..) | Event::ClipboardStore(..) => {}
            Event::Bell => {}
            Event::Exit | Event::ChildExit(_) => exited = true,
        };

        handle(first);
        // Drain everything else queued right now into this same frame.
        while let Ok(ev) = events.try_recv() {
            handle(ev);
        }

        if dirty || title.is_some() || !write_back.is_empty() {
            let update = view.update(cx, |view, cx| {
                if !write_back.is_empty() {
                    view.write_to_backend(&write_back);
                }
                if let Some(t) = title.take() {
                    view.set_title(t);
                }
                cx.notify();
            });
            if update.is_err() {
                break; // view dropped
            }
        }

        if exited {
            let _ = view.update(cx, |view, cx| {
                view.mark_exited();
                cx.notify();
            });
            break;
        }

        // Throttle: hold off until the next frame so a burst coalesces.
        cx.background_executor().timer(FRAME_INTERVAL).await;
    }
}
