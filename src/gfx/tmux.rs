//! tmux passthrough: wrap an escape sequence so tmux forwards it to the
//! terminal outside instead of interpreting it.
//!
//! The wrapper is `ESC P tmux ;` + the sequence with every ESC doubled +
//! `ESC \`. tmux only forwards it when `allow-passthrough` is on (emde never
//! changes that option), and it drops any single string over 1 MiB, so
//! callers wrap each kitty chunk separately.

/// Opening of tmux's passthrough DCS.
const START: &[u8] = b"\x1bPtmux;";
/// String terminator closing the DCS.
const END: &[u8] = b"\x1b\\";

/// The number of bytes [`wrap`] produces for `seq`.
pub fn wrapped_len(seq: &[u8]) -> usize {
    let escapes = seq.iter().filter(|&&b| b == 0x1b).count();
    START.len() + seq.len() + escapes + END.len()
}

/// Append `seq`, wrapped for tmux passthrough, to `out`.
pub fn wrap_into(seq: &[u8], out: &mut Vec<u8>) {
    out.reserve(wrapped_len(seq));
    out.extend_from_slice(START);
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(END);
}

/// `seq` wrapped for tmux passthrough.
pub fn wrap(seq: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    wrap_into(seq, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_vector() {
        assert_eq!(
            wrap(b"\x1b_Gm=0;AAAA\x1b\\"),
            b"\x1bPtmux;\x1b\x1b_Gm=0;AAAA\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn doubles_every_escape_and_nothing_else() {
        assert_eq!(wrap(b""), b"\x1bPtmux;\x1b\\");
        assert_eq!(wrap(b"abc\x07"), b"\x1bPtmux;abc\x07\x1b\\");
        assert_eq!(wrap(b"\x1b\x1b"), b"\x1bPtmux;\x1b\x1b\x1b\x1b\x1b\\");
        for seq in [&b""[..], b"\x1b]1337;File=:AA==\x07", b"\x1b\x1bx"] {
            assert_eq!(wrap(seq).len(), wrapped_len(seq));
        }
    }
}
