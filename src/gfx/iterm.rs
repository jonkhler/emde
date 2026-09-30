//! iTerm2 inline images (OSC 1337), also understood by WezTerm, Tabby,
//! Warp, Rio, mintty and VS Code with images enabled.
//!
//! The terminal decodes the file itself, so the payload is the original
//! file when it is small enough ([`ORIGINAL_MAX_BYTES`]), else a downscaled
//! PNG. The image is sized in cells (`width=C;height=R`), keeping its aspect
//! ratio, and drawn at the cursor. There is no source cropping: a partly
//! visible image is sent as a slice of its visible rows.
//!
//! iTerm2 and tmux accept at most [`MAX_SEQUENCE`] bytes per escape
//! sequence. Larger images use the multipart form of iTerm2 ≥ 3.5
//! (`MultipartFile`, `FilePart`…, `FileEnd`).

use super::{Passthrough, b64, tmux};

/// The longest escape sequence iTerm2 (and tmux) accepts.
pub const MAX_SEQUENCE: usize = 1 << 20;

/// Original files up to this size are sent as they are (their base64 still
/// fits in one sequence); larger ones should be re-encoded as a downscaled
/// PNG first.
pub const ORIGINAL_MAX_BYTES: usize = 750_000;

/// Base64 bytes per `FilePart` of a multipart transfer (a multiple of 4).
pub const PART_BYTES: usize = 256 * 1024;

/// How an inline image is sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Options {
    /// The terminal understands the multipart form (iTerm2 ≥ 3.5).
    pub multipart: bool,
    /// Wrapping for tmux passthrough (each sequence separately).
    pub passthrough: Passthrough,
}

/// Show `data` (an image file iTerm2 can decode) at the cursor over
/// `cols × rows` cells, preserving its aspect ratio:
/// `ESC ] 1337 ; File=inline=1;size=N;width=C;height=R;preserveAspectRatio=1 : <base64> BEL`.
///
/// When that sequence would exceed [`MAX_SEQUENCE`], the multipart form is
/// used if `opts.multipart` allows it; otherwise the image cannot be sent
/// and the result is `None` (send a smaller PNG instead).
pub fn inline_image(data: &[u8], cols: u16, rows: u16, opts: Options) -> Option<Vec<u8>> {
    let args = format!(
        "inline=1;size={};width={cols};height={rows};preserveAspectRatio=1",
        data.len()
    );
    // The single sequence's length, known before encoding anything.
    let single = FILE.len() + args.len() + 1 + b64::encoded_len(data.len()) + 1;
    let len = match opts.passthrough {
        Passthrough::Direct => single,
        // The wrapper, plus the OSC's one ESC doubled (base64 has none).
        Passthrough::Tmux => single + tmux::wrapped_len(b"") + 1,
    };
    if len > MAX_SEQUENCE {
        return opts
            .multipart
            .then(|| multipart(&args, &b64::encode(data), opts.passthrough));
    }
    let mut seq = Vec::with_capacity(single);
    seq.extend_from_slice(FILE);
    seq.extend_from_slice(args.as_bytes());
    seq.push(b':');
    b64::encode_into(data, &mut seq);
    seq.push(0x07);
    Some(match opts.passthrough {
        Passthrough::Direct => seq,
        Passthrough::Tmux => tmux::wrap(&seq),
    })
}

/// Opening of the single-sequence form.
const FILE: &[u8] = b"\x1b]1337;File=";

/// The multipart form: a header, the base64 in [`PART_BYTES`] parts, and an
/// end marker, each its own sequence.
fn multipart(args: &str, payload: &[u8], passthrough: Passthrough) -> Vec<u8> {
    let parts = payload.len().div_ceil(PART_BYTES);
    let mut out = Vec::with_capacity(payload.len() + (parts + 2) * 32 + args.len());
    let header = format!("\x1b]1337;MultipartFile={args}\x07");
    passthrough.push(header.as_bytes(), &mut out);
    let mut part = Vec::with_capacity(PART_BYTES + 24);
    for chunk in payload.chunks(PART_BYTES) {
        part.clear();
        part.extend_from_slice(b"\x1b]1337;FilePart=");
        part.extend_from_slice(chunk);
        part.push(0x07);
        passthrough.push(&part, &mut out);
    }
    passthrough.push(b"\x1b]1337;FileEnd\x07", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECT: Options = Options {
        multipart: false,
        passthrough: Passthrough::Direct,
    };

    #[test]
    fn single_sequence_format() {
        let out = inline_image(b"GIF89a", 12, 5, DIRECT).unwrap();
        assert_eq!(
            out,
            b"\x1b]1337;File=inline=1;size=6;width=12;height=5;preserveAspectRatio=1:R0lGODlh\x07"
        );
    }

    #[test]
    fn tmux_wrapping() {
        let opts = Options {
            passthrough: Passthrough::Tmux,
            ..DIRECT
        };
        let out = inline_image(b"GIF89a", 1, 1, opts).unwrap();
        assert_eq!(
            out,
            b"\x1bPtmux;\x1b\x1b]1337;File=inline=1;size=6;width=1;height=1;\
              preserveAspectRatio=1:R0lGODlh\x07\x1b\\"
        );
    }

    #[test]
    fn predicted_length_is_exact() {
        // The size check runs before encoding; it must agree with the bytes.
        for n in [0usize, 1, 2, 3, 100, 4097] {
            for passthrough in [Passthrough::Direct, Passthrough::Tmux] {
                let opts = Options {
                    passthrough,
                    ..DIRECT
                };
                let out = inline_image(&vec![7u8; n], 3, 2, opts).unwrap();
                let args = format!("inline=1;size={n};width=3;height=2;preserveAspectRatio=1");
                let single = FILE.len() + args.len() + 2 + b64::encoded_len(n);
                let want = match passthrough {
                    Passthrough::Direct => single,
                    Passthrough::Tmux => single + tmux::wrapped_len(b"") + 1,
                };
                assert_eq!(out.len(), want, "{n} {passthrough:?}");
            }
        }
    }

    /// The largest payload whose single sequence still fits, for `opts`.
    fn largest_single(opts: Options) -> usize {
        let fits = |n: usize| inline_image(&vec![0u8; n], 80, 24, opts).is_some();
        let (mut lo, mut hi) = (0usize, MAX_SEQUENCE);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(mid) { lo = mid } else { hi = mid - 1 }
        }
        lo
    }

    #[test]
    fn size_limit_without_multipart() {
        let n = largest_single(DIRECT);
        let out = inline_image(&vec![0u8; n], 80, 24, DIRECT).unwrap();
        assert!(
            out.len() <= MAX_SEQUENCE && out.len() > MAX_SEQUENCE - 8,
            "{}",
            out.len()
        );
        assert_eq!(inline_image(&vec![0u8; n + 3], 80, 24, DIRECT), None);
        assert!(ORIGINAL_MAX_BYTES < n, "originals up to the threshold fit");
        // Wrapping for tmux leaves less room, and still respects the cap.
        let tmux = Options {
            passthrough: Passthrough::Tmux,
            ..DIRECT
        };
        let m = largest_single(tmux);
        assert!(m < n);
        let out = inline_image(&vec![0u8; m], 80, 24, tmux).unwrap();
        assert!(out.len() <= MAX_SEQUENCE);
    }

    #[test]
    fn multipart_over_the_limit() {
        let data = vec![0xAB; 900_000]; // 1,200,000 base64 bytes
        let opts = Options {
            multipart: true,
            ..DIRECT
        };
        let out = inline_image(&data, 80, 24, opts).unwrap();
        let text = String::from_utf8(out).unwrap();
        let seqs: Vec<&str> = text.split_terminator('\x07').collect();
        assert_eq!(
            seqs[0],
            "\x1b]1337;MultipartFile=inline=1;size=900000;width=80;height=24;preserveAspectRatio=1"
        );
        assert_eq!(*seqs.last().unwrap(), "\x1b]1337;FileEnd");
        let parts: Vec<&str> = seqs[1..seqs.len() - 1]
            .iter()
            .map(|s| s.strip_prefix("\x1b]1337;FilePart=").unwrap())
            .collect();
        assert_eq!(parts.len(), 1_200_000usize.div_ceil(PART_BYTES));
        assert!(
            parts[..parts.len() - 1]
                .iter()
                .all(|p| p.len() == PART_BYTES)
        );
        assert_eq!(parts.concat().into_bytes(), b64::encode(&data));
        assert!(seqs.iter().all(|s| s.len() < MAX_SEQUENCE));
    }

    #[test]
    fn multipart_in_tmux_wraps_each_sequence() {
        let data = vec![1u8; 800_000];
        let opts = Options {
            multipart: true,
            passthrough: Passthrough::Tmux,
        };
        let out = inline_image(&data, 10, 10, opts).unwrap();
        let text = String::from_utf8(out).unwrap();
        let wrapped = text.matches("\x1bPtmux;\x1b\x1b]1337;").count();
        assert_eq!(wrapped, 2 + 1_066_668usize.div_ceil(PART_BYTES));
        assert_eq!(text.matches("\x1b]1337;").count(), wrapped);
    }
}
