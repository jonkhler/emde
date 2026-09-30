//! Byte output: SGR and OSC 8 encoding, the stream sink, and plain and
//! tagged text renderings.
//!
//! * [`sgr`] — minimal SGR transitions and colour downsampling;
//! * [`osc8`] — hyperlink sequences and URL safety;
//! * [`emit`] — layout lines to bytes, with the highlight overlay and
//!   gradient backgrounds applied;
//! * [`stream`] — the buffered sink for standard output;
//! * [`debug`] — style tags instead of escapes, for snapshots.

pub mod debug;
pub mod emit;
pub mod osc8;
pub mod sgr;
pub mod stream;

pub use debug::debug_text;
pub use emit::{Emitter, Mark, Piece, RenderConfig, SegStyle, marked_segments, segments};
pub use stream::{StreamSink, is_broken_pipe};

use crate::ir::Document;
use crate::layout::Layout;

/// Every line of a layout as bytes.
pub fn to_bytes(doc: &Document, layout: &Layout, cfg: &RenderConfig) -> Vec<u8> {
    let mut out = Vec::with_capacity(layout.text.len() + layout.lines.len() * 8);
    Emitter::new(doc, layout, cfg).write_all(&mut out);
    out
}

/// A layout as plain text: no escape sequences, one line per layout line.
pub fn plain_text(doc: &Document, layout: &Layout) -> String {
    let bytes = to_bytes(doc, layout, &RenderConfig::plain());
    String::from_utf8(bytes).unwrap_or_default()
}
