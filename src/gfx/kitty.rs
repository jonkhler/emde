//! The kitty graphics protocol: Unicode placeholders, classic placements
//! and deletion.
//!
//! Commands are APC strings, `ESC _G <keys> ; <payload> ESC \`, with PNG
//! payloads (`f=100`) sent inline (`t=d`, the only medium that works over
//! SSH) as base64 in chunks of at most [`CHUNK`] bytes. Every command carries
//! `q=2`, so the terminal never replies: emde sends no queries once its key
//! reader runs, and a late reply would arrive as key presses.
//!
//! With [`Passthrough::Tmux`] each command, and each chunk of an upload,
//! is wrapped separately for tmux passthrough; no bare `ESC _G` is ever
//! written, because tmux treats an unwrapped APC as a pane title.
//!
//! # Unicode placeholders
//!
//! [`transmit_placeholder`] uploads the image with a virtual placement of
//! `cols × rows` cells. The image then shows wherever the text of
//! [`placeholder_row`] is printed: U+10EEEE cells whose foreground colour
//! carries the low 24 bits of the image id and whose diacritics carry the
//! row, the column and the id's high byte. Placeholders are ordinary text,
//! so they scroll, clip and survive tmux redraws for free.
//!
//! # Classic placements
//!
//! For terminals without placeholders (never inside tmux): [`transmit`]
//! once, then [`place`] at the cursor after every frame, cropped to the
//! visible rows. Re-placing under the same placement id replaces the old
//! placement without flicker.

use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::r#gen::diacritics::DIACRITICS;
use super::{Passthrough, b64};

/// The placeholder character (a private-use code point).
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Maximum base64 bytes per upload chunk (a multiple of 4).
pub const CHUNK: usize = 4096;

/// How many rows or columns a placeholder image can span: rows and columns
/// are numbered `0..MAX_CELLS`, one diacritic per number.
pub const MAX_CELLS: u16 = DIACRITICS.len() as u16;

/// The support query of the startup probe: a 1×1 RGB image queried with
/// `a=q`, answered `ESC _Gi=31;OK ESC \` by terminals with kitty graphics.
/// It must not carry `q=` (the reply is the point) and, like every kitty
/// command, must only reach tmux wrapped with [`Passthrough::Tmux`].
pub const SUPPORT_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";

/// A kitty image id: never 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(NonZeroU32);

impl ImageId {
    /// The id with this number, or `None` for 0.
    pub const fn new(raw: u32) -> Option<ImageId> {
        match NonZeroU32::new(raw) {
            Some(id) => Some(ImageId(id)),
            None => None,
        }
    }

    /// The id as a number.
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// The low 24 bits as the `(r, g, b)` foreground colour of placeholders.
    pub const fn rgb(self) -> (u8, u8, u8) {
        let id = self.get();
        ((id >> 16) as u8, (id >> 8) as u8, id as u8)
    }

    /// The high byte, carried by a placeholder's third diacritic.
    pub const fn high_byte(self) -> u8 {
        (self.get() >> 24) as u8
    }
}

/// Hands out image ids that are never 0 and never repeat until all 2³² − 1
/// have been used.
///
/// kitty ids are global to the terminal, shared by every program (and every
/// tmux pane) that draws there, so each allocator starts at a random
/// 32-bit base. An id is never reused because iTerm2 3.6 keeps showing the
/// old picture when an id is uploaded again.
#[derive(Debug)]
pub struct IdAllocator {
    next: AtomicU32,
}

impl IdAllocator {
    /// An allocator starting at a base derived from the time and process id.
    pub fn new() -> IdAllocator {
        IdAllocator::starting_at(random_base())
    }

    /// An allocator whose first id is `first` (or 1 if `first` is 0).
    pub const fn starting_at(first: u32) -> IdAllocator {
        IdAllocator {
            next: AtomicU32::new(first),
        }
    }

    /// The next id. Thread-safe.
    pub fn next_id(&self) -> ImageId {
        loop {
            let raw = self.next.fetch_add(1, Ordering::Relaxed);
            if let Some(id) = ImageId::new(raw) {
                return id;
            }
        }
    }
}

impl Default for IdAllocator {
    fn default() -> Self {
        IdAllocator::new()
    }
}

/// The next id from the process-wide allocator.
pub fn next_image_id() -> ImageId {
    static IDS: OnceLock<IdAllocator> = OnceLock::new();
    IDS.get_or_init(IdAllocator::new).next_id()
}

/// A per-process pseudo-random 32-bit value from the clock and the pid
/// (SplitMix64 finaliser).
fn random_base() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = nanos ^ u64::from(std::process::id()).rotate_left(32);
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((z ^ (z >> 31)) >> 32) as u32
}

/// The row/column diacritic that encodes `n`, if `n < MAX_CELLS`.
pub fn diacritic(n: u16) -> Option<char> {
    DIACRITICS.get(usize::from(n)).copied()
}

/// Clamp a cell count to what placeholders can address (`1..=MAX_CELLS`).
pub fn clamp_cells(n: u16) -> u16 {
    n.clamp(1, MAX_CELLS)
}

/// Upload a PNG and create a virtual placement of `cols × rows` cells for
/// Unicode placeholders (`a=T,U=1`). Both counts are clamped with
/// [`clamp_cells`]; print rows with the same counts.
pub fn transmit_placeholder(
    id: ImageId,
    png: &[u8],
    cols: u16,
    rows: u16,
    passthrough: Passthrough,
) -> Vec<u8> {
    let keys = format!(
        "a=T,U=1,i={},f=100,t=d,c={},r={},q=2",
        id.get(),
        clamp_cells(cols),
        clamp_cells(rows)
    );
    chunked(&keys, &b64::encode(png), passthrough)
}

/// Upload a PNG without displaying it (`a=t`), for classic placements.
pub fn transmit(id: ImageId, png: &[u8], passthrough: Passthrough) -> Vec<u8> {
    let keys = format!("a=t,i={},f=100,t=d,q=2", id.get());
    chunked(&keys, &b64::encode(png), passthrough)
}

/// Split base64 `data` into upload chunks: every chunk but the last holds
/// [`CHUNK`] bytes (a multiple of 4) and `m=1`, the last one `m=0`. Only the
/// first carries `keys`; the others carry just `m` and `q=2`.
fn chunked(keys: &str, data: &[u8], passthrough: Passthrough) -> Vec<u8> {
    let count = data.len().div_ceil(CHUNK).max(1);
    let mut out = Vec::with_capacity(data.len() + count * 32 + keys.len());
    let mut cmd = Vec::with_capacity(CHUNK + keys.len() + 16);
    let mut chunks = data.chunks(CHUNK);
    for n in 0..count {
        let more = u8::from(n + 1 < count);
        let head = if n == 0 {
            format!("\x1b_G{keys},m={more};")
        } else {
            format!("\x1b_Gm={more},q=2;")
        };
        cmd.clear();
        cmd.extend_from_slice(head.as_bytes());
        cmd.extend_from_slice(chunks.next().unwrap_or_default());
        cmd.extend_from_slice(b"\x1b\\");
        passthrough.push(&cmd, &mut out);
    }
    out
}

/// One complete command without payload.
fn command(keys: &str, passthrough: Passthrough) -> Vec<u8> {
    let cmd = format!("\x1b_G{keys}\x1b\\");
    let mut out = Vec::with_capacity(cmd.len() + 16);
    passthrough.push(cmd.as_bytes(), &mut out);
    out
}

/// The text of placeholder row `row` for the columns in `cols`:
/// `ESC[38;2;R;G;Bm`, then per cell U+10EEEE with the row, column and
/// high-byte diacritics, then `ESC[39m`. All three diacritics are always
/// written, so any column range stands on its own (and iTerm2 3.6.9, which
/// draws nothing without the third one, works). `None` if the row or a
/// column is not below [`MAX_CELLS`].
///
/// The row is `cols.len()` columns wide. Count it that way rather than with
/// a Unicode width function: U+10EEEE is East Asian Ambiguous, so a
/// double-width measure of ambiguous characters would count every cell
/// twice.
pub fn placeholder_row(id: ImageId, row: u16, cols: Range<u16>) -> Option<String> {
    let row_mark = diacritic(row)?;
    let high = diacritic(u16::from(id.high_byte()))?;
    if cols.end > MAX_CELLS {
        return None;
    }
    let (r, g, b) = id.rgb();
    let mut s = String::with_capacity(24 + cols.len() * 12);
    let _ = write!(s, "\x1b[38;2;{r};{g};{b}m");
    for col in cols {
        s.push(PLACEHOLDER);
        s.push(row_mark);
        s.push(diacritic(col)?);
        s.push(high);
    }
    s.push_str("\x1b[39m");
    Some(s)
}

/// Display an uploaded image at the cursor over `cols × rows` cells
/// (`a=p`), without moving the cursor (`C=1`). `crop` selects source pixel
/// rows (`y`, `h`) for a partly visible image; see
/// [`super::size::visible_pixel_rows`]. Re-sending the same `placement` id
/// replaces the previous placement without flicker (placement id 0 would
/// add a new placement every time instead, hence [`NonZeroU32`]).
///
/// The result is empty when there is nothing to show: no cells, or an
/// empty crop. kitty reads `c=0`, `r=0` and `h=0` as "the image's own
/// size", so such a placement would cover far more than intended; delete
/// the placement instead ([`delete`]).
pub fn place(
    id: ImageId,
    placement: NonZeroU32,
    cols: u16,
    rows: u16,
    crop: Option<Range<u32>>,
    passthrough: Passthrough,
) -> Vec<u8> {
    if cols == 0 || rows == 0 || crop.as_ref().is_some_and(Range::is_empty) {
        return Vec::new();
    }
    let mut keys = format!("a=p,i={},p={placement},c={cols},r={rows}", id.get());
    if let Some(crop) = crop {
        let height = crop.end - crop.start;
        let _ = write!(keys, ",y={},h={height}", crop.start);
    }
    keys.push_str(",C=1,q=2");
    command(&keys, passthrough)
}

/// What a delete command removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Deletion {
    /// Every placement of the image; the data stays for later placements
    /// (`d=i`).
    Placements,
    /// One placement of the image (`d=i` with `p=`).
    Placement(NonZeroU32),
    /// The placements and the image data (`d=I`), e.g. on exit.
    Image,
}

/// Delete placements or the image with this id (`a=d`).
pub fn delete(id: ImageId, what: Deletion, passthrough: Passthrough) -> Vec<u8> {
    let keys = match what {
        Deletion::Placements => format!("a=d,d=i,i={},q=2", id.get()),
        Deletion::Placement(p) => format!("a=d,d=i,i={},p={p},q=2", id.get()),
        Deletion::Image => format!("a=d,d=I,i={},q=2", id.get()),
    };
    command(&keys, passthrough)
}

/// Free every image in `ids` (`d=I` each), for the exit sequence.
pub fn delete_all(ids: impl IntoIterator<Item = ImageId>, passthrough: Passthrough) -> Vec<u8> {
    ids.into_iter()
        .flat_map(|id| delete(id, Deletion::Image, passthrough))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(raw: u32) -> ImageId {
        ImageId::new(raw).unwrap()
    }

    fn p(raw: u32) -> NonZeroU32 {
        NonZeroU32::new(raw).unwrap()
    }

    /// Split a byte stream of APC commands into their `(keys, payload)`.
    fn commands(bytes: &[u8]) -> Vec<(String, String)> {
        let text = std::str::from_utf8(bytes).unwrap();
        text.split("\x1b\\")
            .filter(|s| !s.is_empty())
            .map(|cmd| {
                let body = cmd.strip_prefix("\x1b_G").expect(cmd);
                let (keys, payload) = body.split_once(';').unwrap_or((body, ""));
                (keys.to_string(), payload.to_string())
            })
            .collect()
    }

    #[test]
    fn ids_are_never_zero() {
        assert_eq!(ImageId::new(0), None);
        let ids = IdAllocator::starting_at(u32::MAX - 1);
        let got: Vec<u32> = (0..4).map(|_| ids.next_id().get()).collect();
        assert_eq!(got, [u32::MAX - 1, u32::MAX, 1, 2]);
        let from_zero = IdAllocator::starting_at(0);
        assert_eq!(from_zero.next_id().get(), 1);
    }

    #[test]
    fn ids_are_unique() {
        let ids = IdAllocator::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(ids.next_id()));
        }
        let a = next_image_id();
        let b = next_image_id();
        assert_ne!(a, b);
    }

    #[test]
    fn id_colour_and_high_byte() {
        let i = id(0x12AB_CDEF);
        assert_eq!(i.rgb(), (0xAB, 0xCD, 0xEF));
        assert_eq!(i.high_byte(), 0x12);
    }

    #[test]
    fn chunking_10000_base64_bytes() {
        let data = vec![b'A'; 10_000];
        let out = chunked(
            "a=T,U=1,i=7,f=100,t=d,c=4,r=2,q=2",
            &data,
            Passthrough::Direct,
        );
        let cmds = commands(&out);
        let sizes: Vec<usize> = cmds.iter().map(|(_, p)| p.len()).collect();
        assert_eq!(sizes, [4096, 4096, 1808]);
        assert_eq!(cmds[0].0, "a=T,U=1,i=7,f=100,t=d,c=4,r=2,q=2,m=1");
        assert_eq!(cmds[1].0, "m=1,q=2");
        assert_eq!(cmds[2].0, "m=0,q=2");
        assert!(sizes[..2].iter().all(|s| s % 4 == 0));
    }

    #[test]
    fn single_and_empty_uploads() {
        let out = chunked("a=t,i=1,f=100,t=d,q=2", b"AAAA", Passthrough::Direct);
        assert_eq!(out, b"\x1b_Ga=t,i=1,f=100,t=d,q=2,m=0;AAAA\x1b\\");
        let exact = chunked("k", &[b'A'; CHUNK], Passthrough::Direct);
        assert_eq!(commands(&exact).len(), 1);
        let empty = chunked("k", b"", Passthrough::Direct);
        assert_eq!(empty, b"\x1b_Gk,m=0;\x1b\\");
    }

    #[test]
    fn transmit_encodes_png_bytes() {
        let out = transmit_placeholder(id(42), b"\x89PNG", 300, 0, Passthrough::Direct);
        assert_eq!(
            out,
            b"\x1b_Ga=T,U=1,i=42,f=100,t=d,c=297,r=1,q=2,m=0;iVBORw==\x1b\\"
        );
        let out = transmit(id(42), b"\x89PNG", Passthrough::Direct);
        assert_eq!(out, b"\x1b_Ga=t,i=42,f=100,t=d,q=2,m=0;iVBORw==\x1b\\");
    }

    #[test]
    fn tmux_wraps_every_chunk() {
        let png = vec![7u8; 7000]; // 9336 base64 bytes: three chunks
        let out = transmit_placeholder(id(9), &png, 10, 5, Passthrough::Tmux);
        let text = String::from_utf8(out.clone()).unwrap();
        // Every APC start is escaped inside a passthrough DCS.
        assert_eq!(text.matches("\x1bPtmux;\x1b\x1b_G").count(), 3);
        assert_eq!(text.matches("\x1b_G").count(), 3);
        for (i, w) in out.windows(3).enumerate() {
            if w == b"\x1b_G" {
                assert_eq!(out[i - 1], 0x1b, "unwrapped APC at byte {i}");
            }
        }
        let unwrapped: Vec<u8> = text
            .split("\x1bPtmux;")
            .filter(|s| !s.is_empty())
            .flat_map(|s| {
                s.strip_suffix("\x1b\\")
                    .unwrap()
                    .replace("\x1b\x1b", "\x1b")
                    .into_bytes()
            })
            .collect();
        assert_eq!(
            unwrapped,
            transmit_placeholder(id(9), &png, 10, 5, Passthrough::Direct)
        );
        for cmd in [
            place(id(9), p(1), 2, 3, None, Passthrough::Tmux),
            delete(id(9), Deletion::Image, Passthrough::Tmux),
        ] {
            assert!(cmd.starts_with(b"\x1bPtmux;\x1b\x1b_G"), "{cmd:?}");
            assert!(cmd.ends_with(b"\x1b\x1b\\\x1b\\"));
        }
    }

    #[test]
    fn placeholder_row_bytes() {
        let row = placeholder_row(id(0x00AB_CDEF), 2, 0..3).unwrap();
        assert!(row.starts_with("\x1b[38;2;171;205;239m\u{10EEEE}\u{030E}\u{0305}\u{0305}"));
        assert_eq!(
            row,
            "\x1b[38;2;171;205;239m\
             \u{10EEEE}\u{030E}\u{0305}\u{0305}\
             \u{10EEEE}\u{030E}\u{030D}\u{0305}\
             \u{10EEEE}\u{030E}\u{030E}\u{0305}\
             \x1b[39m"
        );
    }

    #[test]
    fn placeholder_row_high_byte_and_clipping() {
        // The docs' example id 42 + (2 << 24): high-byte diacritic U+030E.
        let row = placeholder_row(id(42 | 2 << 24), 1, 1..2).unwrap();
        assert_eq!(
            row,
            "\x1b[38;2;0;0;42m\u{10EEEE}\u{030D}\u{030D}\u{030E}\x1b[39m"
        );
        assert_eq!(
            placeholder_row(id(1), 0, 0..0).unwrap(),
            "\x1b[38;2;0;0;1m\x1b[39m"
        );
        assert!(placeholder_row(id(1), MAX_CELLS - 1, 0..MAX_CELLS).is_some());
        assert_eq!(placeholder_row(id(1), MAX_CELLS, 0..1), None);
        assert_eq!(placeholder_row(id(1), 0, 0..MAX_CELLS + 1), None);
    }

    #[test]
    fn diacritic_table() {
        assert_eq!(MAX_CELLS, 297);
        assert_eq!(diacritic(0), Some('\u{0305}'));
        assert_eq!(diacritic(1), Some('\u{030D}'));
        assert_eq!(diacritic(2), Some('\u{030E}'));
        assert_eq!(diacritic(296), Some('\u{1D244}'));
        assert_eq!(diacritic(297), None);
        let unique: std::collections::HashSet<char> = DIACRITICS.iter().copied().collect();
        assert_eq!(unique.len(), DIACRITICS.len());
        // All are combining marks: zero width, never a base character.
        use unicode_width::UnicodeWidthChar as _;
        assert!(DIACRITICS.iter().all(|c| c.width() == Some(0)));
    }

    #[test]
    fn classic_placement_and_deletion() {
        assert_eq!(
            place(id(5), p(1), 40, 10, None, Passthrough::Direct),
            b"\x1b_Ga=p,i=5,p=1,c=40,r=10,C=1,q=2\x1b\\"
        );
        assert_eq!(
            place(id(5), p(1), 40, 4, Some(96..224), Passthrough::Direct),
            b"\x1b_Ga=p,i=5,p=1,c=40,r=4,y=96,h=128,C=1,q=2\x1b\\"
        );
        // Nothing to show writes nothing: `c=0`, `r=0` or `h=0` would mean
        // the image's full size to kitty.
        assert!(place(id(5), p(1), 0, 4, None, Passthrough::Direct).is_empty());
        assert!(place(id(5), p(1), 40, 0, None, Passthrough::Direct).is_empty());
        assert!(place(id(5), p(1), 40, 4, Some(96..96), Passthrough::Direct).is_empty());
        #[allow(clippy::reversed_empty_ranges)]
        let backwards = place(id(5), p(1), 40, 4, Some(96..90), Passthrough::Tmux);
        assert!(backwards.is_empty());
        assert_eq!(
            delete(id(5), Deletion::Placements, Passthrough::Direct),
            b"\x1b_Ga=d,d=i,i=5,q=2\x1b\\"
        );
        assert_eq!(
            delete(id(5), Deletion::Placement(p(3)), Passthrough::Direct),
            b"\x1b_Ga=d,d=i,i=5,p=3,q=2\x1b\\"
        );
        assert_eq!(
            delete(id(5), Deletion::Image, Passthrough::Direct),
            b"\x1b_Ga=d,d=I,i=5,q=2\x1b\\"
        );
        assert_eq!(
            delete_all([id(1), id(2)], Passthrough::Direct),
            b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b_Ga=d,d=I,i=2,q=2\x1b\\"
        );
    }

    #[test]
    fn support_query() {
        let mut wrapped = Vec::new();
        Passthrough::Tmux.push(SUPPORT_QUERY, &mut wrapped);
        assert_eq!(
            wrapped,
            b"\x1bPtmux;\x1b\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\x1b\\\x1b\\"
        );
        let (keys, payload) = &commands(SUPPORT_QUERY)[0];
        assert!(!keys.contains("q="));
        assert_eq!(payload, "AAAA", "one RGB pixel, base64");
    }

    #[test]
    fn every_command_is_quiet() {
        let cmds = [
            transmit_placeholder(id(3), &[1; 5000], 2, 2, Passthrough::Direct),
            transmit(id(3), &[1; 10], Passthrough::Direct),
            place(id(3), p(1), 2, 2, Some(0..4), Passthrough::Direct),
            delete(id(3), Deletion::Placements, Passthrough::Direct),
        ];
        for bytes in cmds {
            for (keys, _) in commands(&bytes) {
                assert!(keys.split(',').any(|k| k == "q=2"), "{keys}");
            }
        }
    }
}

#[cfg(test)]
mod props {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn chunking_invariants(len in 0usize..20_000, tmux in any::<bool>()) {
            let data = vec![b'Q'; len];
            let pass = if tmux { Passthrough::Tmux } else { Passthrough::Direct };
            let out = chunked("a=t,i=1,q=2", &data, pass);
            let direct = if tmux {
                // Undo the wrapping: every command is one complete DCS.
                let text = String::from_utf8(out).unwrap();
                let parts: Vec<&str> = text.split_inclusive("\x1b\\\x1b\\").collect();
                prop_assert!(parts.iter().all(|p| p.starts_with("\x1bPtmux;")));
                parts
                    .iter()
                    .map(|p| p["\x1bPtmux;".len()..p.len() - 2].replace("\x1b\x1b", "\x1b"))
                    .collect::<String>()
                    .into_bytes()
            } else {
                out
            };
            let text = String::from_utf8(direct).unwrap();
            let cmds: Vec<&str> = text.split_terminator("\x1b\\").collect();
            prop_assert_eq!(cmds.len(), len.div_ceil(CHUNK).max(1));
            let mut payload = 0;
            for (i, cmd) in cmds.iter().enumerate() {
                let body = cmd.strip_prefix("\x1b_G").unwrap();
                let (keys, data) = body.split_once(';').unwrap();
                let last = i + 1 == cmds.len();
                let m = if last { "m=0" } else { "m=1" };
                if i == 0 {
                    prop_assert_eq!(keys, format!("a=t,i=1,q=2,{m}"));
                } else {
                    prop_assert_eq!(keys, format!("{m},q=2"));
                }
                if !last {
                    prop_assert_eq!(data.len(), CHUNK);
                }
                prop_assert!(data.len() <= CHUNK);
                payload += data.len();
            }
            prop_assert_eq!(payload, len);
        }

        #[test]
        fn tmux_wrap_round_trips(seq in proptest::collection::vec(any::<u8>(), 0..200)) {
            let wrapped = super::super::tmux::wrap(&seq);
            let inner = &wrapped[b"\x1bPtmux;".len()..wrapped.len() - 2];
            let mut unwrapped = Vec::new();
            let mut escape = false;
            for &b in inner {
                if b == 0x1b && !escape {
                    escape = true;
                    continue;
                }
                prop_assert!(!escape || b == 0x1b, "a lone ESC inside the wrapper");
                escape = false;
                unwrapped.push(b);
            }
            prop_assert!(!escape);
            prop_assert_eq!(unwrapped, seq);
        }
    }
}
