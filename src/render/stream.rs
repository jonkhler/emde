//! The stream sink: laid-out documents written to standard output (or any
//! writer) through one large buffer.
//!
//! Output goes through a 256 KiB [`BufWriter`], so a document costs a
//! handful of `write` calls. A reader that goes away (`emde x.md | head`)
//! shows up as [`io::ErrorKind::BrokenPipe`], which callers treat as a
//! normal end ([`is_broken_pipe`]); Rust ignores `SIGPIPE`, so nothing else
//! happens.

use std::io::{self, BufWriter, Write};

use crate::ir::Document;
use crate::layout::Layout;

use super::emit::{Emitter, RenderConfig};

/// Size of the output buffer.
pub const BUFFER: usize = 256 * 1024;

/// Bytes collected before they are handed to the buffer.
const CHUNK: usize = 64 * 1024;

/// Writes documents one after another; see the module docs.
pub struct StreamSink<W: Write> {
    out: BufWriter<W>,
    cfg: RenderConfig,
    buf: Vec<u8>,
    documents: usize,
}

impl<W: Write> StreamSink<W> {
    /// A sink writing to `writer` with the given encoding.
    pub fn new(writer: W, cfg: RenderConfig) -> StreamSink<W> {
        StreamSink {
            out: BufWriter::with_capacity(BUFFER, writer),
            cfg,
            buf: Vec::with_capacity(CHUNK + 4096),
            documents: 0,
        }
    }

    /// Write one laid-out document; documents are one blank line apart.
    pub fn write_document(&mut self, doc: &Document, layout: &Layout) -> io::Result<()> {
        if self.documents > 0 {
            self.out.write_all(b"\n")?;
        }
        let mut emitter = Emitter::new(doc, layout, &self.cfg);
        for i in 0..layout.lines.len() {
            emitter.write_line(i, &mut self.buf);
            if self.buf.len() >= CHUNK {
                self.out.write_all(&self.buf)?;
                self.buf.clear();
            }
        }
        self.out.write_all(&self.buf)?;
        self.buf.clear();
        let links = u32::try_from(doc.links.len()).unwrap_or(u32::MAX);
        self.cfg.link_base = self.cfg.link_base.saturating_add(links);
        self.documents += 1;
        Ok(())
    }

    /// Write text as it is (debug dumps).
    pub fn write_text(&mut self, text: &str) -> io::Result<()> {
        self.out.write_all(text.as_bytes())
    }

    /// Flush everything.
    pub fn finish(mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Whether an error means the reader went away.
pub fn is_broken_pipe(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that fails like a closed pipe after `n` bytes.
    struct Pipe {
        left: usize,
        got: Vec<u8>,
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.left == 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            let n = buf.len().min(self.left);
            self.left -= n;
            self.got.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipes_are_recognised() {
        let mut sink = StreamSink::new(
            Pipe {
                left: 3,
                got: Vec::new(),
            },
            RenderConfig::plain(),
        );
        let err = sink.write_text(&"x".repeat(BUFFER * 2)).unwrap_err();
        assert!(is_broken_pipe(&err));
        assert_eq!(sink.out.get_ref().got, b"xxx");
        assert!(!is_broken_pipe(&io::Error::other("other")));
    }
}
