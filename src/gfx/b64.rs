//! Standard base64 (RFC 4648 §4: `A–Z a–z 0–9 + /`, `=` padding), the
//! payload encoding of the kitty and iTerm2 image protocols, and a lenient
//! decoder for `data:` image URIs.

/// The 64-character standard alphabet.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The encoded length of `n` input bytes (always a multiple of 4).
pub const fn encoded_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// Append the base64 encoding of `input` to `out`.
pub fn encode_into(input: &[u8], out: &mut Vec<u8>) {
    out.reserve(encoded_len(input.len()));
    let sextet = |bits: u32, shift: u32| ALPHABET[((bits >> shift) & 0x3f) as usize];
    let (groups, rest) = input.as_chunks::<3>();
    for &[a, b, c] in groups {
        let bits = u32::from(a) << 16 | u32::from(b) << 8 | u32::from(c);
        out.extend_from_slice(&[
            sextet(bits, 18),
            sextet(bits, 12),
            sextet(bits, 6),
            sextet(bits, 0),
        ]);
    }
    match *rest {
        [a] => {
            let bits = u32::from(a) << 16;
            out.extend_from_slice(&[sextet(bits, 18), sextet(bits, 12), b'=', b'=']);
        }
        [a, b] => {
            let bits = u32::from(a) << 16 | u32::from(b) << 8;
            out.extend_from_slice(&[sextet(bits, 18), sextet(bits, 12), sextet(bits, 6), b'=']);
        }
        _ => {}
    }
}

/// The base64 encoding of `input` as ASCII bytes.
pub fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(input, &mut out);
    out
}

/// The base64 encoding of `input` as a string.
pub fn encode_string(input: &[u8]) -> String {
    encode(input).into_iter().map(char::from).collect()
}

/// The value of one base64 symbol, standard (`+ /`) or URL-safe (`- _`).
fn sextet_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decode base64 as `data:` URIs carry it: the standard or the URL-safe
/// alphabet, padding optional, ASCII whitespace ignored. `None` for any
/// other byte, symbols after padding, or a length no encoding produces.
pub fn decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 2);
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut padding = 0usize;
    for &b in input {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            padding += 1;
            continue;
        }
        let value = sextet_value(b)?;
        if padding > 0 {
            return None;
        }
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // One symbol left over holds only 6 bits: no encoder writes that.
    (bits < 6 && padding <= 2).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(encode_string(input), expected, "{input:?}");
            assert_eq!(encode(input), expected.as_bytes());
            assert_eq!(encoded_len(input.len()), expected.len());
        }
    }

    #[test]
    fn every_alphabet_symbol_and_high_bytes() {
        // 0x00 0x10 0x83 0x10 0x51 0x87 … walks all 64 sextets in order.
        let bytes: Vec<u8> = (0u32..16)
            .flat_map(|i| {
                let bits = (4 * i) << 18 | (4 * i + 1) << 12 | (4 * i + 2) << 6 | (4 * i + 3);
                [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]
            })
            .collect();
        assert_eq!(encode(&bytes), ALPHABET);
        assert_eq!(encode_string(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(encode_string(&[0xfb, 0xef]), "++8=");
    }

    #[test]
    fn appends_to_existing_output() {
        let mut out = b"x:".to_vec();
        encode_into(b"hi", &mut out);
        assert_eq!(out, b"x:aGk=");
    }

    #[test]
    fn decoding_round_trips() {
        for n in 0..64usize {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let text = encode(&bytes);
            assert_eq!(decode(&text).as_deref(), Some(&bytes[..]), "{n}");
            // Without padding, too.
            let bare: Vec<u8> = text.iter().copied().filter(|&b| b != b'=').collect();
            assert_eq!(decode(&bare).as_deref(), Some(&bytes[..]), "{n}");
        }
    }

    #[test]
    fn decoding_is_lenient_but_total() {
        assert_eq!(decode(b"aGVs\nbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode(b" aGVsbG8 ").as_deref(), Some(&b"hello"[..]));
        // The URL-safe alphabet.
        assert_eq!(decode(b"-_8=").as_deref(), Some(&[0xfb, 0xff][..]));
        assert_eq!(decode(b"++8=").as_deref(), Some(&[0xfb, 0xef][..]));
        assert_eq!(decode(b"").as_deref(), Some(&[][..]));
        for bad in [
            &b"a"[..],
            b"aGVsb",
            b"aG=Vs",
            b"aGk*",
            b"aGk===",
            b"\xff\xfe",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }
}
