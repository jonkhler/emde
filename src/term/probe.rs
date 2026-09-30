//! One batched `/dev/tty` probe (kitty query, XTVERSION, cell size, OSC 11,
//! DA1 sentinel) and a byte-level reply parser.
//!
//! # The batch
//!
//! The probe writes one batch of queries to `/dev/tty` in raw mode, then
//! reads replies with `poll(2)` (`select(2)` on macOS) until the sentinel
//! arrives or the deadline passes:
//!
//! ```text
//! ESC_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA ESC\   kitty graphics query: never with q=, never unwrapped in tmux
//! ESC[>0q  ESC[16t  ESC[14t  ESC[18t         XTVERSION; cell px; text area px; text area cells
//! ESC]11;? ESC\                              background colour
//! ESC[c                                      DA1, the sentinel, always last
//! ```
//!
//! Every terminal answers DA1 and terminals answer in order, so once the DA1
//! reply is in, every other answer has arrived. Inside tmux, tmux answers the
//! batch itself (an unwrapped APC would set the pane title, so the kitty
//! query is never sent unwrapped there). When `allow-passthrough` is on and
//! the outer terminal could show Unicode placeholders (see [`Batch::new`]),
//! the kitty query and an `ESC[5n` status request also go to the outer
//! terminal, wrapped in `ESC Ptmux; … ESC \`; its `ESC[0n` answer marks the
//! end of the outer replies.
//!
//! # Late replies
//!
//! crossterm has no parser for terminal replies: one that arrives after the
//! deadline reaches the pager as Alt-keys and letters, which are pager keys.
//! After a timeout the input layer runs keys through a [`LateReplyFilter`]
//! for [`LATE_REPLY_GRACE`].
//!
//! # Cache
//!
//! Probing iTerm2 over SSH costs a round trip plus about 26 ms, so answers
//! that took longer than [`CACHE_MIN_ELAPSED`] are kept per session in
//! `$XDG_RUNTIME_DIR/emde/caps-<hash>` for [`CACHE_TTL`]. The hash covers the
//! variables that identify the session and its terminal, so a new SSH
//! connection or a different terminal misses the cache. Without
//! `$XDG_RUNTIME_DIR` nothing is cached; [`ProbeRequest::reprobe`] bypasses
//! the cache and refreshes it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use filedescriptor::{POLLIN, pollfd};

use super::caps::{multiplexer_term, tmux_placeholder_candidate};
use super::env::Env;
use super::tmux::TmuxInfo;
use crate::style::Rgb;

/// kitty graphics query: a 1×1 RGB image with `a=q`, so nothing is stored.
/// It never carries `q=`, which would suppress the `OK` answer.
pub const KITTY_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
/// XTVERSION; answered with `DCS > | name version ST`.
pub const XTVERSION_QUERY: &[u8] = b"\x1b[>0q";
/// Cell size in pixels; answered with `CSI 6 ; height ; width t`.
pub const CELL_SIZE_QUERY: &[u8] = b"\x1b[16t";
/// Text area in pixels; answered with `CSI 4 ; height ; width t`.
pub const TEXT_AREA_PX_QUERY: &[u8] = b"\x1b[14t";
/// Text area in cells; answered with `CSI 8 ; rows ; columns t`.
pub const TEXT_AREA_CELLS_QUERY: &[u8] = b"\x1b[18t";
/// Background colour; answered with `OSC 11 ; rgb:RRRR/GGGG/BBBB ST`.
pub const BACKGROUND_QUERY: &[u8] = b"\x1b]11;?\x1b\\";
/// Primary device attributes (the sentinel); answered with `CSI ? Ps ; … c`.
pub const DA1_QUERY: &[u8] = b"\x1b[c";
/// Device status report (the outer sentinel inside tmux); answered with `CSI 0 n`.
pub const STATUS_QUERY: &[u8] = b"\x1b[5n";

/// Reply deadline for a local terminal (or tmux, which answers locally).
pub const LOCAL_TIMEOUT: Duration = Duration::from_millis(150);
/// Reply deadline when the answers travel over SSH.
pub const SSH_TIMEOUT: Duration = Duration::from_secs(1);
/// Upper bound for a configured deadline.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(10);
/// How long late bytes are drained after the sentinel (or the deadline).
pub const DRAIN: Duration = Duration::from_millis(10);
/// How long the input layer swallows reply-shaped input after a timeout.
pub const LATE_REPLY_GRACE: Duration = Duration::from_secs(2);
/// How long a cached probe stays valid.
pub const CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Probes faster than this are not worth caching.
pub const CACHE_MIN_ELAPSED: Duration = Duration::from_millis(10);

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// Longest CSI parameter string kept (real answers are about 20 bytes).
const MAX_CSI: usize = 64;
/// Longest OSC, DCS or APC payload kept.
const MAX_STRING: usize = 512;
/// Longest terminal-reported text kept (XTVERSION).
const MAX_TEXT: usize = 128;

// ---------------------------------------------------------------------------
// The batch
// ---------------------------------------------------------------------------

/// What a probe should find out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Needs {
    /// The background colour (OSC 11), for `theme.background = "auto"`.
    pub background: bool,
    /// Graphics support: the kitty query, XTVERSION and the cell size.
    pub graphics: bool,
}

impl Needs {
    /// Everything (`--doctor`).
    pub const ALL: Needs = Needs {
        background: true,
        graphics: true,
    };

    /// Whether there is anything to ask at all.
    pub fn any(self) -> bool {
        self.background || self.graphics
    }
}

/// One batch of queries, plus what its replies mean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    bytes: Vec<u8>,
    needs: Needs,
    in_tmux: bool,
    wrapped: bool,
}

impl Batch {
    /// Build the batch for `needs`.
    ///
    /// Inside tmux the kitty query is never sent unwrapped. It goes to the
    /// outer terminal wrapped, followed by a wrapped `ESC[5n` as the outer
    /// sentinel, when `allow_passthrough` (`images.tmux_passthrough`)
    /// permits it and `tmux` shows that placeholders could work: passthrough
    /// `on` or `all`, `RGB`, and an allowlisted client terminal (see
    /// [`tmux_placeholder_candidate`]). DA1 is always the last query.
    pub fn new(
        needs: Needs,
        in_tmux: bool,
        tmux: Option<&TmuxInfo>,
        allow_passthrough: bool,
    ) -> Batch {
        let wrapped = in_tmux
            && needs.graphics
            && allow_passthrough
            && tmux.is_some_and(tmux_placeholder_candidate);
        let mut bytes = Vec::with_capacity(160);
        if wrapped {
            tmux_wrap(KITTY_QUERY, &mut bytes);
            tmux_wrap(STATUS_QUERY, &mut bytes);
        }
        if needs.graphics {
            if !in_tmux {
                bytes.extend_from_slice(KITTY_QUERY);
            }
            for query in [
                XTVERSION_QUERY,
                CELL_SIZE_QUERY,
                TEXT_AREA_PX_QUERY,
                TEXT_AREA_CELLS_QUERY,
            ] {
                bytes.extend_from_slice(query);
            }
        }
        if needs.background {
            bytes.extend_from_slice(BACKGROUND_QUERY);
        }
        bytes.extend_from_slice(DA1_QUERY);
        Batch {
            bytes,
            needs,
            in_tmux,
            wrapped,
        }
    }

    /// The bytes to write, in one `write`.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// What the batch asks for.
    pub fn needs(&self) -> Needs {
        self.needs
    }

    /// The batch is answered by tmux (plus the outer terminal if wrapped).
    pub fn in_tmux(&self) -> bool {
        self.in_tmux
    }

    /// The batch includes the wrapped queries for the outer terminal.
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    /// A parser for this batch's replies.
    pub fn parser(&self) -> ReplyParser {
        ReplyParser::new(self.in_tmux, self.wrapped)
    }
}

/// Wrap `seq` for tmux passthrough: `ESC Ptmux;` + `seq` with every ESC
/// doubled + `ESC \`.
fn tmux_wrap(seq: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == ESC {
            out.push(ESC);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
}

// ---------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------

/// Everything learned from the replies to one batch.
///
/// Pixel sizes are `(width, height)`; cell counts are `(columns, rows)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeReplies {
    /// The terminal itself answered the kitty query with `OK` (outside tmux).
    pub kitty_ok: bool,
    /// The outer terminal answered the wrapped kitty query with `OK` (inside tmux).
    pub outer_kitty_ok: bool,
    /// XTVERSION text, e.g. `kitty(0.49.1)`, `iTerm2 3.6.9` or `tmux 3.4`.
    pub xtversion: Option<String>,
    /// Cell size in pixels (`CSI 16 t`).
    pub cell_px: Option<(u16, u16)>,
    /// Text area size in pixels (`CSI 14 t`; iTerm2 answers in points).
    pub text_area_px: Option<(u16, u16)>,
    /// Text area size in cells (`CSI 18 t`).
    pub size_cells: Option<(u16, u16)>,
    /// Background colour (OSC 11).
    pub background: Option<Rgb>,
    /// DA1 attributes (`CSI ? 62 ; 4 c` gives `[62, 4]`); `4` means sixel.
    pub da1: Option<Vec<u16>>,
    /// The outer terminal answered the wrapped status request (`CSI 0 n`).
    pub outer_dsr_seen: bool,
    /// Inside tmux: tmux's own DA1 answer has `4`, i.e. tmux draws sixel.
    pub tmux_own_da1_sixel: bool,
}

impl ProbeReplies {
    /// Cell size in pixels: `CSI 16 t`, else `CSI 14 t` divided by `CSI 18 t`.
    pub fn cell_size(&self) -> Option<(u16, u16)> {
        self.cell_px.or_else(|| {
            let (w, h) = self.text_area_px?;
            let (cols, rows) = self.size_cells?;
            let cell = (w.checked_div(cols)?, h.checked_div(rows)?);
            (cell.0 > 0 && cell.1 > 0).then_some(cell)
        })
    }

    /// Whether DA1 was answered and lists `attr`.
    pub fn da1_has(&self, attr: u16) -> bool {
        self.da1.as_ref().is_some_and(|a| a.contains(&attr))
    }
}

/// Parser states. Strings are OSC, DCS, APC (and ignored SOS/PM).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Csi,
    Str(StrKind),
    /// Inside a string, just after an ESC: `\` ends the string (ST).
    StrEscape(StrKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StrKind {
    Osc,
    Dcs,
    Apc,
    /// SOS and PM: consumed so their payload is not misread, then dropped.
    Other,
}

/// A CSI reply the probe understands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CsiReply {
    Da1(Vec<u16>),
    CellPx(u16, u16),
    TextAreaPx(u16, u16),
    TextAreaCells(u16, u16),
    Status,
}

/// An OSC, DCS or APC reply the probe understands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StrReply {
    Background(Rgb),
    XtVersion(String),
    Kitty { ok: bool },
}

/// Incremental parser for terminal replies.
///
/// A byte-level state machine for CSI, OSC, DCS and APC sequences with ST or
/// BEL terminators. It keeps its state between [`feed`](Self::feed) calls, so
/// replies may be split at any byte; bytes outside sequences (keys typed
/// during the probe, junk) are ignored, and unknown or malformed sequences
/// are skipped. It never panics.
#[derive(Clone, Debug)]
pub struct ReplyParser {
    in_tmux: bool,
    wrapped: bool,
    state: State,
    buf: Vec<u8>,
    overflow: bool,
    replies: ProbeReplies,
}

impl ReplyParser {
    /// A parser for replies to a batch sent inside tmux (`in_tmux`), with or
    /// without the wrapped queries for the outer terminal (`wrapped`).
    pub fn new(in_tmux: bool, wrapped: bool) -> ReplyParser {
        ReplyParser {
            in_tmux,
            wrapped,
            state: State::Ground,
            buf: Vec::with_capacity(64),
            overflow: false,
            replies: ProbeReplies::default(),
        }
    }

    /// Feed bytes read from the terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.advance(b);
        }
    }

    /// All sentinels are in: DA1, plus the outer status answer when the
    /// wrapped queries were sent.
    pub fn is_done(&self) -> bool {
        self.replies.da1.is_some() && (!self.wrapped || self.replies.outer_dsr_seen)
    }

    /// What has been learned so far.
    pub fn replies(&self) -> &ProbeReplies {
        &self.replies
    }

    /// Finish, returning what was learned.
    pub fn into_replies(self) -> ProbeReplies {
        self.replies
    }

    fn advance(&mut self, b: u8) {
        match self.state {
            State::Ground => {
                if b == ESC {
                    self.state = State::Escape;
                }
            }
            State::Escape => self.escape(b),
            State::Csi => self.csi(b),
            State::Str(kind) => self.string(kind, b),
            State::StrEscape(kind) => {
                if b == b'\\' {
                    self.dispatch_string(kind);
                    self.state = State::Ground;
                } else {
                    // The ESC starts a new sequence; the unterminated string is dropped.
                    self.escape(b);
                }
            }
        }
    }

    /// The byte after an ESC.
    fn escape(&mut self, b: u8) {
        self.state = match b {
            b'[' => State::Csi,
            b']' => State::Str(StrKind::Osc),
            b'P' => State::Str(StrKind::Dcs),
            b'_' => State::Str(StrKind::Apc),
            b'X' | b'^' => State::Str(StrKind::Other),
            ESC => State::Escape,
            _ => State::Ground,
        };
        self.buf.clear();
        self.overflow = false;
    }

    fn csi(&mut self, b: u8) {
        match b {
            0x20..=0x3f => self.push(b, MAX_CSI),
            0x40..=0x7e => {
                self.dispatch_csi(b);
                self.state = State::Ground;
            }
            ESC => self.state = State::Escape,
            CAN | SUB => self.state = State::Ground,
            // Other C0 controls are executed, not part of the sequence.
            0x00..=0x1f | 0x7f => {}
            // 8-bit bytes never occur in replies: drop the sequence.
            _ => self.state = State::Ground,
        }
    }

    fn string(&mut self, kind: StrKind, b: u8) {
        match b {
            BEL => {
                self.dispatch_string(kind);
                self.state = State::Ground;
            }
            ESC => self.state = State::StrEscape(kind),
            CAN | SUB => self.state = State::Ground,
            _ => self.push(b, MAX_STRING),
        }
    }

    fn push(&mut self, b: u8, limit: usize) {
        if self.buf.len() < limit {
            self.buf.push(b);
        } else {
            self.overflow = true;
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) {
        if self.overflow {
            return;
        }
        if let Some(reply) = csi_reply(&self.buf, final_byte) {
            self.apply_csi(reply);
        }
    }

    fn dispatch_string(&mut self, kind: StrKind) {
        if self.overflow {
            return;
        }
        if let Some(reply) = string_reply(kind, &self.buf) {
            self.apply_string(reply);
        }
    }

    /// Record a CSI reply; for each fact the first answer wins.
    fn apply_csi(&mut self, reply: CsiReply) {
        let r = &mut self.replies;
        match reply {
            CsiReply::Da1(attrs) => {
                if r.da1.is_none() {
                    // Inside tmux, DA1 is tmux's own answer (`?1;2;4c` with sixel).
                    r.tmux_own_da1_sixel = self.in_tmux && attrs.contains(&4);
                    r.da1 = Some(attrs);
                }
            }
            CsiReply::CellPx(w, h) => r.cell_px = r.cell_px.or(Some((w, h))),
            CsiReply::TextAreaPx(w, h) => r.text_area_px = r.text_area_px.or(Some((w, h))),
            CsiReply::TextAreaCells(c, l) => r.size_cells = r.size_cells.or(Some((c, l))),
            // Only the wrapped batch asks for a status report.
            CsiReply::Status => r.outer_dsr_seen |= self.wrapped,
        }
    }

    /// Record a string reply; for each fact the first answer wins.
    fn apply_string(&mut self, reply: StrReply) {
        let r = &mut self.replies;
        match reply {
            StrReply::Background(rgb) => r.background = r.background.or(Some(rgb)),
            StrReply::XtVersion(text) => {
                if r.xtversion.is_none() {
                    r.xtversion = Some(text);
                }
            }
            // Inside tmux only the wrapped query is sent, so a kitty answer
            // there comes from the outer terminal.
            StrReply::Kitty { ok } => {
                if self.wrapped {
                    r.outer_kitty_ok |= ok;
                } else if !self.in_tmux {
                    r.kitty_ok |= ok;
                }
            }
        }
    }
}

/// Interpret a complete CSI sequence: its parameter and intermediate bytes
/// (`buf`) and final byte.
fn csi_reply(buf: &[u8], final_byte: u8) -> Option<CsiReply> {
    let (private, params) = match buf.split_first() {
        Some((&p, rest)) if (0x3c..=0x3f).contains(&p) => (Some(p), rest),
        _ => (None, buf),
    };
    // Intermediates or misplaced private markers: some other reply (DECRPM, …).
    if params
        .iter()
        .any(|b| (0x20..=0x2f).contains(b) || (0x3c..=0x3f).contains(b))
    {
        return None;
    }
    let nums = parse_params(params)?;
    match private {
        Some(b'?') if final_byte == b'c' => return Some(CsiReply::Da1(nums)),
        Some(_) => return None,
        None => {}
    }
    let pair = |w: u16, h: u16| (w > 0 && h > 0).then_some((w, h));
    match (final_byte, nums.as_slice()) {
        (b't', &[6, h, w]) => pair(w, h).map(|(w, h)| CsiReply::CellPx(w, h)),
        (b't', &[4, h, w]) => pair(w, h).map(|(w, h)| CsiReply::TextAreaPx(w, h)),
        (b't', &[8, rows, cols]) => pair(cols, rows).map(|(c, r)| CsiReply::TextAreaCells(c, r)),
        // `0` is "OK", `3` "malfunction": either way the terminal answered.
        (b'n', &[0] | &[3]) => Some(CsiReply::Status),
        _ => None,
    }
}

/// Interpret a complete OSC, DCS or APC payload.
fn string_reply(kind: StrKind, payload: &[u8]) -> Option<StrReply> {
    match kind {
        StrKind::Osc => {
            let spec = payload.strip_prefix(b"11;")?;
            parse_x11_color(spec).map(StrReply::Background)
        }
        StrKind::Dcs => {
            let text = clean_text(payload.strip_prefix(b">|")?);
            (!text.is_empty()).then_some(StrReply::XtVersion(text))
        }
        StrKind::Apc => {
            let body = payload.strip_prefix(b"G")?;
            let split = body.iter().position(|&b| b == b';')?;
            let (keys, message) = (body.get(..split)?, body.get(split + 1..)?);
            let ours = keys.split(|&b| b == b',').any(|kv| kv == b"i=31");
            ours.then_some(StrReply::Kitty {
                ok: message == b"OK",
            })
        }
        StrKind::Other => None,
    }
}

/// `;`-separated decimal parameters, skipping empty ones (`62;` is `[62]`).
/// `None` if a parameter is not a plain decimal that fits in `u16`.
fn parse_params(bytes: &[u8]) -> Option<Vec<u16>> {
    bytes
        .split(|&b| b == b';')
        .filter(|p| !p.is_empty())
        .map(decimal)
        .collect()
}

fn decimal(digits: &[u8]) -> Option<u16> {
    let mut value: u32 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(d - b'0'))?;
    }
    u16::try_from(value).ok()
}

/// Parse an X11 colour as terminals report it: `rgb:R/G/B` or
/// `rgba:R/G/B/A` with 1–4 hex digits per channel, scaled to 8 bits.
pub fn parse_x11_color(spec: &[u8]) -> Option<Rgb> {
    let (body, channels) = match strip_prefix_ignore_case(spec, b"rgba:") {
        Some(body) => (body, 4),
        None => (strip_prefix_ignore_case(spec, b"rgb:")?, 3),
    };
    let mut parts = body.split(|&b| b == b'/');
    let r = hex_channel(parts.next()?)?;
    let g = hex_channel(parts.next()?)?;
    let b = hex_channel(parts.next()?)?;
    if channels == 4 {
        hex_channel(parts.next()?)?;
    }
    parts.next().is_none().then_some(Rgb(r, g, b))
}

fn strip_prefix_ignore_case<'a>(s: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| s.get(prefix.len()..))
        .flatten()
}

/// 1–4 hex digits scaled to 0–255 with rounding (`f` and `ffff` are 255,
/// `80` and `8080` are 128).
fn hex_channel(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let mut value: u32 = 0;
    for &d in digits {
        value = value * 16 + (d as char).to_digit(16)?;
    }
    let max = (1u32 << (4 * digits.len())) - 1;
    u8::try_from((value * 255 + max / 2) / max).ok()
}

/// Terminal-reported text as a printable string: invalid UTF-8 replaced,
/// control characters dropped, trimmed, at most [`MAX_TEXT`] characters.
fn clean_text(bytes: &[u8]) -> String {
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect();
    text.trim().to_string()
}

// ---------------------------------------------------------------------------
// Running the probe
// ---------------------------------------------------------------------------

/// How a probe ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Every sentinel came back before the deadline.
    Complete,
    /// The deadline passed first. Late replies may still arrive; see
    /// [`LateReplyFilter`].
    TimedOut,
    /// Answered from the per-session cache, written `age` ago.
    Cached {
        /// Age of the cache entry.
        age: Duration,
    },
    /// The terminal could not be probed (no `/dev/tty`, raw mode failed, …).
    Failed(String),
}

/// The result of a probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// What the replies said.
    pub replies: ProbeReplies,
    /// How the probe ended.
    pub status: ProbeStatus,
    /// Time from writing the batch to the last sentinel (or the deadline).
    pub elapsed: Duration,
    /// The wrapped tmux passthrough queries were sent.
    pub wrapped: bool,
}

impl ProbeOutcome {
    fn failed(error: &io::Error, batch: &Batch) -> ProbeOutcome {
        ProbeOutcome {
            replies: ProbeReplies::default(),
            status: ProbeStatus::Failed(error.to_string()),
            elapsed: Duration::ZERO,
            wrapped: batch.wrapped(),
        }
    }

    /// Replies may still be on their way (the probe timed out).
    pub fn replies_may_follow(&self) -> bool {
        self.status == ProbeStatus::TimedOut
    }
}

/// A probe to run: what to ask, and the context to ask it in.
#[derive(Clone, Copy, Debug)]
pub struct ProbeRequest<'a> {
    /// The environment snapshot (`$TMUX`, `$SSH_CONNECTION`, cache key, …).
    pub env: &'a Env,
    /// The tmux query result, when inside tmux.
    pub tmux: Option<&'a TmuxInfo>,
    /// What to find out.
    pub needs: Needs,
    /// Reply deadline (`terminal.probe_timeout_ms`); `None` picks
    /// [`default_timeout`]. Capped at [`MAX_TIMEOUT`].
    pub timeout: Option<Duration>,
    /// Ignore the cache and probe again (`--reprobe`); a fresh answer
    /// replaces the cached one.
    pub reprobe: bool,
    /// `images.tmux_passthrough`: wrapped queries may go through tmux.
    pub tmux_passthrough: bool,
}

/// Probe the terminal, or answer from the cache.
///
/// Returns `None` without touching the terminal when nothing is needed or
/// stdout is not a terminal.
pub fn probe(req: &ProbeRequest<'_>) -> Option<ProbeOutcome> {
    if !req.needs.any() || !io::stdout().is_terminal() {
        return None;
    }
    Some(probe_with(req, SystemTime::now(), run))
}

/// [`probe`] with the clock and the terminal exchange supplied (tests).
fn probe_with(
    req: &ProbeRequest<'_>,
    now: SystemTime,
    runner: impl FnOnce(&Batch, Duration) -> io::Result<ProbeOutcome>,
) -> ProbeOutcome {
    let behind_multiplexer = req.env.is_set("TMUX") || multiplexer_term(req.env);
    let batch = Batch::new(
        req.needs,
        behind_multiplexer,
        req.tmux,
        req.tmux_passthrough,
    );
    let cache = Cache::new(req.env, req.tmux, &batch);
    if !req.reprobe
        && let Some(hit) = cache.as_ref().and_then(|c| c.load(now))
    {
        return hit;
    }
    let timeout = req
        .timeout
        .unwrap_or_else(|| default_timeout(req.env, &batch))
        .min(MAX_TIMEOUT);
    let outcome = runner(&batch, timeout).unwrap_or_else(|e| ProbeOutcome::failed(&e, &batch));
    if let Some(cache) = &cache
        && outcome.status == ProbeStatus::Complete
        && outcome.elapsed > CACHE_MIN_ELAPSED
    {
        // A cache that cannot be written only costs the next run a probe.
        let _ = cache.store(&outcome, now);
    }
    outcome
}

/// The automatic deadline: [`SSH_TIMEOUT`] when the answers cross an SSH
/// connection, else [`LOCAL_TIMEOUT`]. Inside a local tmux (`$TMUX` set) the
/// answers come from tmux itself unless the batch includes wrapped queries
/// for the outer terminal.
pub fn default_timeout(env: &Env, batch: &Batch) -> Duration {
    let local_tmux = env.is_set("TMUX") && !batch.wrapped();
    if env.is_set("SSH_CONNECTION") && !local_tmux {
        SSH_TIMEOUT
    } else {
        LOCAL_TIMEOUT
    }
}

/// Run `batch` on the controlling terminal: open `/dev/tty`, switch to raw
/// mode, write the batch once, read replies until the sentinels or
/// `timeout`, then drain late bytes for [`DRAIN`].
///
/// Raw mode is restored on every path out, including errors and panics; a
/// terminal that already was in raw mode is left in it.
pub fn run(batch: &Batch, timeout: Duration) -> io::Result<ProbeOutcome> {
    let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    let _raw = RawMode::enable()?;
    exchange(&mut tty, batch, timeout, DRAIN)
}

/// Write the batch and collect the replies from a terminal-like stream.
fn exchange<T: Read + Write + AsRawFd>(
    tty: &mut T,
    batch: &Batch,
    timeout: Duration,
    drain_for: Duration,
) -> io::Result<ProbeOutcome> {
    let mut parser = batch.parser();
    let start = Instant::now();
    let deadline = start.checked_add(timeout).unwrap_or(start);
    tty.write_all(batch.bytes())?;
    tty.flush()?;
    let done = read_until_done(tty, &mut parser, deadline)?;
    let elapsed = start.elapsed();
    drain(tty, &mut parser, drain_for);
    Ok(ProbeOutcome {
        replies: parser.into_replies(),
        status: if done {
            ProbeStatus::Complete
        } else {
            ProbeStatus::TimedOut
        },
        elapsed,
        wrapped: batch.wrapped(),
    })
}

/// Feed replies to `parser` until it is done, the deadline passes or the
/// stream ends. Returns whether it is done.
fn read_until_done<R: Read + AsRawFd>(
    src: &mut R,
    parser: &mut ReplyParser,
    deadline: Instant,
) -> io::Result<bool> {
    let mut buf = [0u8; 512];
    while !parser.is_done() && wait_readable(src.as_raw_fd(), deadline)? {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => parser.feed(buf.get(..n).unwrap_or_default()),
            Err(e) if is_retry(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(parser.is_done())
}

/// Feed whatever arrives within `window` to `parser`, so stragglers never
/// reach the pager's input.
fn drain<R: Read + AsRawFd>(src: &mut R, parser: &mut ReplyParser, window: Duration) {
    let now = Instant::now();
    let deadline = now.checked_add(window).unwrap_or(now);
    let mut buf = [0u8; 512];
    while let Ok(true) = wait_readable(src.as_raw_fd(), deadline) {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => parser.feed(buf.get(..n).unwrap_or_default()),
            Err(e) if is_retry(&e) => {}
            Err(_) => break,
        }
    }
}

fn is_retry(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}

/// Wait until `fd` is readable (or hung up) or `deadline` passes; `Ok(false)`
/// means the deadline passed. Uses `filedescriptor::poll`, which falls back
/// to `select(2)` on macOS, where `poll(2)` does not work on ttys.
pub(crate) fn wait_readable(fd: RawFd, deadline: Instant) -> io::Result<bool> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        // poll(2) counts whole milliseconds: round up so short waits block.
        let wait = (deadline - now).max(Duration::from_millis(1));
        let mut fds = [pollfd {
            fd,
            events: POLLIN,
            revents: 0,
        }];
        match filedescriptor::poll(&mut fds, Some(wait)) {
            Ok(0) => {}
            // POLLIN, POLLHUP or POLLERR: the next read reports which.
            Ok(_) => return Ok(fds.iter().any(|p| p.revents != 0)),
            Err(filedescriptor::Error::Poll(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io::Error::other(e)),
        }
    }
}

/// Raw mode for the duration of a probe.
struct RawMode {
    /// Raw mode was off before and must be switched off again.
    restore: bool,
}

impl RawMode {
    fn enable() -> io::Result<RawMode> {
        if crossterm::terminal::is_raw_mode_enabled()? {
            return Ok(RawMode { restore: false });
        }
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawMode { restore: true })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.restore {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Variables whose values identify a session and its terminal.
pub const CACHE_KEY_VARS: [&str; 7] = [
    "SSH_CONNECTION",
    "TERM",
    "TERM_PROGRAM",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TMUX",
    "COLORFGBG",
];

/// First line of a cache file: the format and its version.
const CACHE_MAGIC: &str = "emde-probe-cache 1";
/// Longest cache file read.
const CACHE_MAX_BYTES: u64 = 4096;
/// Tolerated clock skew for entries written "in the future".
const CACHE_SKEW: Duration = Duration::from_secs(60);

/// The cache entry for one session and batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cache {
    path: PathBuf,
    needs: Needs,
    wrapped: bool,
}

impl Cache {
    /// The entry for this session and batch; `None` without `$XDG_RUNTIME_DIR`.
    ///
    /// The file name hashes [`CACHE_KEY_VARS`], what tmux says about its
    /// client (a different terminal may attach to the same session) and the
    /// shape of the batch.
    pub fn new(env: &Env, tmux: Option<&TmuxInfo>, batch: &Batch) -> Option<Cache> {
        let dir = env.non_empty("XDG_RUNTIME_DIR")?;
        let key = cache_key(env, tmux, batch);
        Some(Cache {
            path: Path::new(dir).join("emde").join(format!("caps-{key:016x}")),
            needs: batch.needs(),
            wrapped: batch.wrapped(),
        })
    }

    /// Where the entry lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The cached outcome, if present, well-formed, for this batch and
    /// younger than [`CACHE_TTL`].
    pub fn load(&self, now: SystemTime) -> Option<ProbeOutcome> {
        let mut text = String::new();
        File::open(&self.path)
            .ok()?
            .take(CACHE_MAX_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        let entry = decode(&text, now)?;
        (entry.needs == self.needs && entry.wrapped == self.wrapped).then_some(ProbeOutcome {
            replies: entry.replies,
            status: ProbeStatus::Cached { age: entry.age },
            elapsed: entry.elapsed,
            wrapped: entry.wrapped,
        })
    }

    /// Write `outcome` (atomically: a temporary file, then a rename).
    pub fn store(&self, outcome: &ProbeOutcome, now: SystemTime) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("cache path has no directory"))?;
        fs::create_dir_all(dir)?;
        let tmp = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        fs::write(&tmp, encode(outcome, self.needs, now))?;
        fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }
}

/// A decoded cache file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheEntry {
    replies: ProbeReplies,
    needs: Needs,
    wrapped: bool,
    elapsed: Duration,
    age: Duration,
}

fn cache_key(env: &Env, tmux: Option<&TmuxInfo>, batch: &Batch) -> u64 {
    let mut h = Fnv::new();
    for var in CACHE_KEY_VARS {
        h.field(var.as_bytes());
        // 0xfe never occurs in UTF-8: unset differs from every value.
        h.field(env.get(var).map_or(b"\xfe".as_slice(), str::as_bytes));
    }
    if let Some(t) = tmux {
        h.field(t.client_termtype.as_bytes());
        h.field(t.client_termname.as_bytes());
        h.field(t.features.join(",").as_bytes());
        h.field(format!("{:?}", t.cell).as_bytes());
        h.field(t.passthrough.as_str().as_bytes());
    }
    let needs = batch.needs();
    h.field(&[
        u8::from(needs.background),
        u8::from(needs.graphics),
        u8::from(batch.wrapped()),
    ]);
    h.0
}

/// 64-bit FNV-1a: small and stable across builds, for cache file names.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    /// Hash `bytes` plus a terminator, so `("ab", "c")` and `("a", "bc")` differ.
    fn field(&mut self, bytes: &[u8]) {
        for &b in bytes.iter().chain(&[0xff]) {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Serialise an outcome as `key=value` lines.
///
/// Window-size answers (`CSI 14 t`, `CSI 18 t`) are not stored, since they
/// change on resize; the cell size derived from them is.
fn encode(outcome: &ProbeOutcome, needs: Needs, now: SystemTime) -> String {
    let r = &outcome.replies;
    let flag = |b: bool| if b { "1" } else { "0" };
    let created = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut lines = vec![
        CACHE_MAGIC.to_string(),
        format!("created={created}"),
        format!("needs={}", encode_needs(needs)),
        format!("wrapped={}", flag(outcome.wrapped)),
        format!("elapsed_us={}", outcome.elapsed.as_micros()),
        format!("kitty_ok={}", flag(r.kitty_ok)),
        format!("outer_kitty_ok={}", flag(r.outer_kitty_ok)),
        format!("outer_dsr_seen={}", flag(r.outer_dsr_seen)),
        format!("tmux_own_da1_sixel={}", flag(r.tmux_own_da1_sixel)),
    ];
    if let Some(text) = &r.xtversion {
        lines.push(format!("xtversion={}", clean_text(text.as_bytes())));
    }
    if let Some((w, h)) = r.cell_size() {
        lines.push(format!("cell_px={w}x{h}"));
    }
    if let Some(Rgb(red, green, blue)) = r.background {
        lines.push(format!("background=#{red:02x}{green:02x}{blue:02x}"));
    }
    if let Some(attrs) = &r.da1 {
        let attrs: Vec<String> = attrs.iter().map(u16::to_string).collect();
        lines.push(format!("da1={}", attrs.join(";")));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Parse a cache file. `None` for a foreign or malformed file, or an entry
/// that is older than [`CACHE_TTL`] or from the future.
fn decode(text: &str, now: SystemTime) -> Option<CacheEntry> {
    let mut lines = text.lines();
    if lines.next()? != CACHE_MAGIC {
        return None;
    }
    let mut created = None;
    let mut needs = None;
    let mut wrapped = false;
    let mut elapsed = Duration::ZERO;
    let mut r = ProbeReplies::default();
    for line in lines.filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once('=')?;
        match key {
            "created" => created = Some(value.parse::<u64>().ok()?),
            "needs" => needs = Some(decode_needs(value)?),
            "wrapped" => wrapped = decode_flag(value)?,
            "elapsed_us" => elapsed = Duration::from_micros(value.parse().ok()?),
            "kitty_ok" => r.kitty_ok = decode_flag(value)?,
            "outer_kitty_ok" => r.outer_kitty_ok = decode_flag(value)?,
            "outer_dsr_seen" => r.outer_dsr_seen = decode_flag(value)?,
            "tmux_own_da1_sixel" => r.tmux_own_da1_sixel = decode_flag(value)?,
            "xtversion" => {
                r.xtversion = Some(clean_text(value.as_bytes())).filter(|t| !t.is_empty())
            }
            "cell_px" => r.cell_px = Some(decode_size(value)?),
            "background" => r.background = Some(Rgb::parse_hex(value)?),
            "da1" => r.da1 = Some(parse_params(value.as_bytes())?),
            // Keys from newer versions of the format.
            _ => {}
        }
    }
    let created = UNIX_EPOCH.checked_add(Duration::from_secs(created?))?;
    let age = match now.duration_since(created) {
        Ok(age) => age,
        Err(e) if e.duration() <= CACHE_SKEW => Duration::ZERO,
        Err(_) => return None,
    };
    (age <= CACHE_TTL).then_some(CacheEntry {
        replies: r,
        needs: needs?,
        wrapped,
        elapsed,
        age,
    })
}

fn encode_needs(needs: Needs) -> &'static str {
    match (needs.background, needs.graphics) {
        (true, true) => "background,graphics",
        (true, false) => "background",
        (false, true) => "graphics",
        (false, false) => "",
    }
}

fn decode_needs(value: &str) -> Option<Needs> {
    let mut needs = Needs::default();
    for word in value.split(',').filter(|w| !w.is_empty()) {
        match word {
            "background" => needs.background = true,
            "graphics" => needs.graphics = true,
            _ => return None,
        }
    }
    Some(needs)
}

fn decode_flag(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn decode_size(value: &str) -> Option<(u16, u16)> {
    let (w, h) = value.split_once('x')?;
    let size = (w.parse().ok()?, h.parse().ok()?);
    (size.0 > 0 && size.1 > 0).then_some(size)
}

// ---------------------------------------------------------------------------
// Late replies
// ---------------------------------------------------------------------------

/// One unit of terminal input, as the pager's input layer sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputUnit {
    /// `ESC` followed by a character (crossterm: Alt + the character).
    Alt(char),
    /// A printable character, possibly shifted.
    Char(char),
    /// `BEL` (crossterm: Ctrl+G).
    Bel,
    /// Anything else: named keys, other control keys, mouse, resize.
    Other,
}

impl InputUnit {
    /// Classify a crossterm key event.
    pub fn from_key(key: &KeyEvent) -> InputUnit {
        let mods = key.modifiers;
        let other_mods =
            KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META;
        match key.code {
            KeyCode::Char(c) if mods.contains(KeyModifiers::ALT) => InputUnit::Alt(c),
            KeyCode::Char('g') if mods == KeyModifiers::CONTROL => InputUnit::Bel,
            KeyCode::Char(c) if !mods.intersects(other_mods) => InputUnit::Char(c),
            _ => InputUnit::Other,
        }
    }
}

/// The kinds of late reply the filter recognises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LateReply {
    /// `ESC _ … ST` (kitty graphics).
    Apc,
    /// `ESC ] … ST` or `ESC ] … BEL` (OSC 11).
    Osc,
    /// `ESC P … ST` (XTVERSION).
    Dcs,
}

/// Longest late reply swallowed, in units.
const MAX_LATE_REPLY: usize = 1024;

/// Swallows terminal replies that arrive after the probe stopped waiting.
///
/// A late reply reaches the input layer as `Alt+_` (APC), `Alt+]` (OSC) or
/// `Alt+P` (DCS), then printable characters, then `Alt+\` (ST) or, for OSC,
/// `BEL`. During the grace period such sequences are dropped. A unit that
/// cannot belong to a reply ends the sequence and passes through; a reply
/// that started in the grace period is swallowed to its end even if that
/// comes later.
#[derive(Clone, Debug, Default)]
pub struct LateReplyFilter {
    until: Option<Instant>,
    reply: Option<LateReply>,
    swallowed: usize,
}

impl LateReplyFilter {
    /// A filter that passes everything.
    pub fn inactive() -> LateReplyFilter {
        LateReplyFilter::default()
    }

    /// Swallow late replies that start within `grace` from `now`.
    pub fn new(now: Instant, grace: Duration) -> LateReplyFilter {
        LateReplyFilter {
            until: now.checked_add(grace),
            ..LateReplyFilter::default()
        }
    }

    /// The filter the input layer needs after `outcome`: active for
    /// [`LATE_REPLY_GRACE`] when the probe timed out, else inactive.
    pub fn after(outcome: Option<&ProbeOutcome>, now: Instant) -> LateReplyFilter {
        if outcome.is_some_and(ProbeOutcome::replies_may_follow) {
            LateReplyFilter::new(now, LATE_REPLY_GRACE)
        } else {
            LateReplyFilter::inactive()
        }
    }

    /// Whether the filter may still swallow input.
    pub fn is_active(&self, now: Instant) -> bool {
        self.reply.is_some() || self.until.is_some_and(|until| now < until)
    }

    /// Whether `unit` belongs to a late reply and must be dropped.
    pub fn swallow(&mut self, now: Instant, unit: InputUnit) -> bool {
        if let Some(kind) = self.reply {
            let terminator =
                unit == InputUnit::Alt('\\') || (unit == InputUnit::Bel && kind == LateReply::Osc);
            if terminator {
                self.reply = None;
                return true;
            }
            if let InputUnit::Char(c) = unit
                && (' '..='~').contains(&c)
                && self.swallowed < MAX_LATE_REPLY
            {
                self.swallowed += 1;
                return true;
            }
            // Not a reply after all: stop swallowing.
            self.reply = None;
        }
        let start = match unit {
            InputUnit::Alt('_') => Some(LateReply::Apc),
            InputUnit::Alt(']') => Some(LateReply::Osc),
            InputUnit::Alt('P') => Some(LateReply::Dcs),
            _ => None,
        };
        match start {
            Some(kind) if self.until.is_some_and(|until| now < until) => {
                self.reply = Some(kind);
                self.swallowed = 0;
                true
            }
            _ => false,
        }
    }
}

/// Recorded and reconstructed terminal replies, shared by the tests of the
/// `term` modules.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{ProbeReplies, ReplyParser};

    // tmux 3.4's local answers (`TMUX_LOCAL`) were recorded in a tmux 3.4
    // pane. The other fixtures are rebuilt byte for byte from each
    // terminal's reply code (kitty screen.c and window.py, Ghostty
    // stream_handler.zig, iTerm2 VT100Output.m and VT100Terminal.m, WezTerm
    // terminalstate, foot csi.c and osc.c, xterm.js InputHandler.ts and its
    // image addon), with realistic sizes and colours.

    /// kitty 0.49.1: every string reply ends in ST; DA1 is `?62;52;c`.
    pub(crate) const KITTY: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1bP>|kitty(0.49.1)\x1b\\\x1b[6;36;17t\
        \x1b[4;1800;2720t\x1b[8;50;160t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;52;c";
    /// Ghostty 1.3.1.
    pub(crate) const GHOSTTY: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1bP>|ghostty 1.3.1\x1b\\\x1b[6;38;18t\
        \x1b[4;1900;2880t\x1b[8;50;160t\x1b]11;rgb:2828/2c2c/3434\x1b\\\x1b[?62;22;52c";
    /// iTerm2 3.6.9, directly: no `CSI 16 t` answer; `CSI 14 t` in points.
    pub(crate) const ITERM2: &[u8] =
        b"\x1b_Gi=31;OK\x1b\\\x1bP>|iTerm2 3.6.9\x1b\\\x1b[4;864;1712t\
        \x1b[8;54;214t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;1;2;4;6;22;28c";
    /// iTerm2 3.6.9 → SSH → tmux 3.4 with `allow-passthrough on`: tmux's
    /// local answers first, then the outer terminal's replies to the wrapped
    /// kitty query and status request.
    pub(crate) const ITERM2_VIA_TMUX: &[u8] =
        b"\x1bP>|tmux 3.4\x1b\\\x1b[6;32;16t\x1b[4;1728;3424t\
        \x1b[8;54;214t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?1;2;4c\x1b_Gi=31;OK\x1b\\\x1b[0n";
    /// WezTerm 20240203-110809-5046fc22 (kitty graphics without placeholders).
    pub(crate) const WEZTERM: &[u8] =
        b"\x1b_Gi=31;OK\x1b\\\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\
        \x1b[6;20;10t\x1b[4;1000;1600t\x1b[8;50;160t\x1b]11;rgb:1f1f/1f1f/2828\x1b\\\
        \x1b[?65;4;6;18;22c";
    /// VS Code with `terminal.integrated.enableImages`: the image addon
    /// answers kitty queries and reports sixel in DA1.
    pub(crate) const VSCODE_IMAGES: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1bP>|xterm.js(6.0.0)\x1b\\\
        \x1b[6;17;8t\x1b[4;850;1200t\x1b[8;50;150t\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\
        \x1b[?62;4;9;22c";
    /// VS Code without images: no kitty reply, plain xterm.js DA1.
    pub(crate) const VSCODE_PLAIN: &[u8] = b"\x1bP>|xterm.js(6.0.0)\x1b\\\x1b[8;50;150t\
        \x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\\x1b[?1;2c";
    /// foot 1.20.2: no kitty graphics; sixel in DA1.
    pub(crate) const FOOT: &[u8] = b"\x1bP>|foot(1.20.2)\x1b\\\x1b[6;20;10t\x1b[4;1000;1600t\
        \x1b[8;50;160t\x1b]11;rgb:2424/2424/2424\x1b\\\x1b[?62;4;22;28c";
    /// xterm patch 390 in its default VT420 mode (no sixel).
    pub(crate) const XTERM: &[u8] = b"\x1bP>|XTerm(390)\x1b\\\x1b[6;17;9t\x1b[4;408;720t\
        \x1b[8;24;80t\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?64;1;2;6;9;15;16;17;18;21;22;28c";
    /// xterm in VT340 mode (`-ti vt340`): sixel in DA1.
    pub(crate) const XTERM_VT340: &[u8] = b"\x1bP>|XTerm(390)\x1b\\\x1b[6;17;9t\x1b[4;408;720t\
        \x1b[8;24;80t\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?63;1;2;4;6;9;15;16;22;28c";
    /// tmux 3.4's local answers, recorded (no client attached, so no OSC 11).
    pub(crate) const TMUX_LOCAL: &[u8] =
        b"\x1bP>|tmux 3.4\x1b\\\x1b[6;32;16t\x1b[4;1280;1920t\x1b[8;40;120t\x1b[?1;2;4c";
    /// The user's session (passthrough off): tmux answers everything,
    /// including OSC 11 from its copy of the client's background.
    pub(crate) const USER_SESSION: &[u8] = b"\x1bP>|tmux 3.4\x1b\\\x1b[6;32;16t\x1b[4;1728;3424t\
        \x1b[8;54;214t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?1;2;4c";

    /// Every fixture with the context its batch was sent in: (name, bytes,
    /// in_tmux, wrapped).
    pub(crate) const FIXTURES: &[(&str, &[u8], bool, bool)] = &[
        ("kitty", KITTY, false, false),
        ("ghostty", GHOSTTY, false, false),
        ("iterm2", ITERM2, false, false),
        ("iterm2 via tmux", ITERM2_VIA_TMUX, true, true),
        ("wezterm", WEZTERM, false, false),
        ("vscode images", VSCODE_IMAGES, false, false),
        ("vscode plain", VSCODE_PLAIN, false, false),
        ("foot", FOOT, false, false),
        ("xterm", XTERM, false, false),
        ("xterm vt340", XTERM_VT340, false, false),
        ("tmux local", TMUX_LOCAL, true, false),
        ("user session", USER_SESSION, true, false),
    ];

    /// Parse a whole fixture.
    pub(crate) fn replies(bytes: &[u8], in_tmux: bool, wrapped: bool) -> ProbeReplies {
        let mut p = ReplyParser::new(in_tmux, wrapped);
        p.feed(bytes);
        p.into_replies()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use proptest::prelude::*;

    use super::fixtures::*;
    use super::*;
    use crate::term::tmux::{self, Passthrough};

    fn parse_all(bytes: &[u8], in_tmux: bool, wrapped: bool) -> ReplyParser {
        let mut p = ReplyParser::new(in_tmux, wrapped);
        p.feed(bytes);
        p
    }

    fn tmux_with(passthrough: Passthrough) -> TmuxInfo {
        TmuxInfo {
            passthrough,
            ..tmux::parse(tmux::fixtures::USER_SESSION).unwrap()
        }
    }

    #[test]
    fn kitty_fixture() {
        let p = parse_all(KITTY, false, false);
        assert!(p.is_done());
        let r = p.replies();
        assert!(r.kitty_ok);
        assert!(!r.outer_kitty_ok);
        assert_eq!(r.xtversion.as_deref(), Some("kitty(0.49.1)"));
        assert_eq!(r.cell_px, Some((17, 36)));
        assert_eq!(r.text_area_px, Some((2720, 1800)));
        assert_eq!(r.size_cells, Some((160, 50)));
        assert_eq!(r.background, Some(Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(r.da1, Some(vec![62, 52]));
        assert!(!r.tmux_own_da1_sixel);
        assert!(!r.outer_dsr_seen);
    }

    #[test]
    fn iterm2_fixture_derives_cell_size() {
        let r = parse_all(ITERM2, false, false).into_replies();
        assert!(r.kitty_ok);
        assert_eq!(r.xtversion.as_deref(), Some("iTerm2 3.6.9"));
        assert_eq!(r.cell_px, None);
        assert_eq!(r.cell_size(), Some((8, 16)));
        assert!(r.da1_has(4));
    }

    #[test]
    fn iterm2_via_tmux_waits_for_the_outer_sentinel() {
        let da1_end = ITERM2_VIA_TMUX
            .windows(9)
            .position(|w| w == b"[?1;2;4c\x1b")
            .map(|i| i + 8)
            .unwrap();
        let mut p = ReplyParser::new(true, true);
        p.feed(&ITERM2_VIA_TMUX[..da1_end]);
        assert!(p.replies().da1.is_some());
        assert!(!p.is_done(), "the outer terminal has not answered yet");
        p.feed(&ITERM2_VIA_TMUX[da1_end..]);
        assert!(p.is_done());
        let r = p.into_replies();
        assert!(r.outer_kitty_ok);
        assert!(!r.kitty_ok);
        assert!(r.outer_dsr_seen);
        assert!(r.tmux_own_da1_sixel);
        assert_eq!(r.xtversion.as_deref(), Some("tmux 3.4"));
        assert_eq!(r.cell_px, Some((16, 32)));
    }

    #[test]
    fn tmux_local_answers() {
        // tmux's two signature answers on their own.
        let mut p = ReplyParser::new(true, false);
        p.feed(b"\x1b[?1;2;4c");
        assert!(p.is_done());
        assert!(p.replies().tmux_own_da1_sixel);
        p.feed(b"\x1bP>|tmux 3.4\x1b\\");
        assert_eq!(p.replies().xtversion.as_deref(), Some("tmux 3.4"));
        // The full recorded batch.
        let r = parse_all(TMUX_LOCAL, true, false).into_replies();
        assert_eq!(r.da1, Some(vec![1, 2, 4]));
        assert!(r.tmux_own_da1_sixel);
        assert_eq!(r.cell_px, Some((16, 32)));
        assert_eq!(r.text_area_px, Some((1920, 1280)));
        assert_eq!(r.size_cells, Some((120, 40)));
        assert_eq!(r.background, None);
        // tmux without sixel support answers `?1;2c`.
        let plain = parse_all(b"\x1b[?1;2c", true, false).into_replies();
        assert!(!plain.tmux_own_da1_sixel);
        // Outside tmux the same DA1 describes the terminal, not tmux.
        let outside = parse_all(b"\x1b[?1;2;4c", false, false).into_replies();
        assert!(!outside.tmux_own_da1_sixel);
        assert!(outside.da1_has(4));
    }

    /// One line per fact the parser extracted.
    fn describe(r: &ProbeReplies, done: bool) -> String {
        let pair = |p: Option<(u16, u16)>| p.map_or("-".to_string(), |(a, b)| format!("{a}x{b}"));
        let da1 = r.da1.as_ref().map_or("-".to_string(), |attrs| {
            let attrs: Vec<String> = attrs.iter().map(u16::to_string).collect();
            attrs.join(";")
        });
        let background = r
            .background
            .map_or("-".to_string(), |Rgb(red, green, blue)| {
                format!("#{red:02x}{green:02x}{blue:02x}")
            });
        let flags = [
            (r.kitty_ok, "kitty_ok"),
            (r.outer_kitty_ok, "outer_kitty_ok"),
            (r.outer_dsr_seen, "outer_dsr_seen"),
            (r.tmux_own_da1_sixel, "tmux_own_da1_sixel"),
            (done, "done"),
        ];
        let flags: Vec<&str> = flags.iter().filter(|f| f.0).map(|f| f.1).collect();
        format!(
            "  xtversion   {}\n  cell_px     {}\n  text_area   {}\n  size_cells  {}\n  \
             cell_size   {}\n  background  {background}\n  da1         {da1}\n  flags       {}\n",
            r.xtversion.as_deref().unwrap_or("-"),
            pair(r.cell_px),
            pair(r.text_area_px),
            pair(r.size_cells),
            pair(r.cell_size()),
            flags.join(" "),
        )
    }

    #[test]
    fn fixture_replies() {
        let mut table = String::new();
        for &(name, bytes, in_tmux, wrapped) in FIXTURES {
            let p = parse_all(bytes, in_tmux, wrapped);
            table.push_str(&format!("{name} (in_tmux={in_tmux}, wrapped={wrapped})\n"));
            table.push_str(&describe(p.replies(), p.is_done()));
        }
        insta::assert_snapshot!(table);
    }

    #[test]
    fn every_fixture_split_at_every_byte() {
        for &(name, bytes, in_tmux, wrapped) in FIXTURES {
            let whole = parse_all(bytes, in_tmux, wrapped);
            assert!(whole.is_done(), "{name}");
            for i in 0..=bytes.len() {
                let mut p = ReplyParser::new(in_tmux, wrapped);
                p.feed(&bytes[..i]);
                p.feed(&bytes[i..]);
                assert_eq!(p.replies(), whole.replies(), "{name} split at {i}");
                assert!(p.is_done(), "{name} split at {i}");
            }
            let mut bytewise = ReplyParser::new(in_tmux, wrapped);
            for b in bytes.chunks(1) {
                bytewise.feed(b);
            }
            assert_eq!(bytewise.replies(), whole.replies(), "{name} byte by byte");
        }
    }

    #[test]
    fn done_only_after_the_last_sentinel() {
        for &(name, bytes, in_tmux, wrapped) in FIXTURES {
            let mut p = ReplyParser::new(in_tmux, wrapped);
            p.feed(&bytes[..bytes.len() - 1]);
            assert!(!p.is_done(), "{name}: done before its last byte");
        }
    }

    #[test]
    fn junk_between_replies_is_ignored() {
        let junk: &[&[u8]] = &[
            b"jjk",    // keys typed during the probe
            b"\x1bOA", // an arrow key (SS3)
            b"\x1b[A", // an arrow key (CSI)
            "héllo ✓".as_bytes(),
            b"\x07\x1b\\",    // stray BEL and ST
            b"\x1b[>1;10;0c", // DA2 is not DA1
            b"\x1b[?2026;2$y",
            b"\x1b[12;40R", // cursor position report
            b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\",
            b"\x1bP1$r0m\x1b\\",
            b"\x1b_Gi=99;OK\x1b\\", // someone else's kitty reply
            b"\x1bX sos \x1b\\\x1b^ pm \x1b\\",
        ];
        let replies: &[&[u8]] = &[
            b"\x1b_Gi=31;OK\x1b\\",
            b"\x1bP>|kitty(0.49.1)\x1b\\",
            b"\x1b[6;36;17t",
            b"\x1b[4;1800;2720t",
            b"\x1b[8;50;160t",
            b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\",
            b"\x1b[?62;52;c",
        ];
        assert_eq!(replies.concat(), KITTY);
        let mut noisy = Vec::new();
        for (i, reply) in replies.iter().enumerate() {
            noisy.extend_from_slice(junk[i % junk.len()]);
            noisy.extend_from_slice(junk[(i + 5) % junk.len()]);
            noisy.extend_from_slice(reply);
        }
        let clean = parse_all(KITTY, false, false);
        let dirty = parse_all(&noisy, false, false);
        assert_eq!(dirty.replies(), clean.replies());
        let mut all_junk = junk.concat();
        all_junk.extend_from_slice(KITTY);
        assert_eq!(
            parse_all(&all_junk, false, false).replies(),
            clean.replies()
        );
    }

    #[test]
    fn bel_terminates_strings() {
        let r = parse_all(
            b"\x1b]11;rgb:ff/80/00\x07\x1bP>|foot(1.20.2)\x07",
            false,
            false,
        )
        .into_replies();
        assert_eq!(r.background, Some(Rgb(255, 128, 0)));
        assert_eq!(r.xtversion.as_deref(), Some("foot(1.20.2)"));
    }

    #[test]
    fn interrupted_sequences_are_dropped() {
        // An OSC cut off by a new sequence, then a complete DA1.
        let r = parse_all(b"\x1b]11;rgb:ffff/ffff/ff\x1b[?1;2c", false, false).into_replies();
        assert_eq!(r.background, None);
        assert_eq!(r.da1, Some(vec![1, 2]));
        // CAN and SUB abort.
        let r = parse_all(b"\x1b[?1;2\x18c\x1b]11;rgb:0/0/0\x1a\x07", false, false).into_replies();
        assert_eq!(r.da1, None);
        assert_eq!(r.background, None);
        // C0 controls inside CSI are executed, the sequence continues.
        let r = parse_all(b"\x1b[?1;\r2c", false, false).into_replies();
        assert_eq!(r.da1, Some(vec![1, 2]));
        // 8-bit bytes end a CSI.
        let r = parse_all(b"\x1b[?1\xc3;2c", false, false).into_replies();
        assert_eq!(r.da1, None);
    }

    #[test]
    fn oversized_sequences_are_ignored() {
        let mut long = b"\x1bP>|".to_vec();
        long.extend(std::iter::repeat_n(b'x', MAX_STRING + 10));
        long.extend_from_slice(b"\x1b\\\x1b[");
        long.extend(std::iter::repeat_n(b'1', MAX_CSI + 10));
        long.extend_from_slice(b"t\x1b[?4c");
        let r = parse_all(&long, false, false).into_replies();
        assert_eq!(r.xtversion, None);
        assert_eq!(r.da1, Some(vec![4]));
    }

    #[test]
    fn first_answer_wins() {
        let r = parse_all(
            b"\x1b[6;20;10t\x1b[6;40;20t\x1bP>|a 1\x1b\\\x1bP>|b 2\x1b\\\x1b[?1c\x1b[?4c",
            false,
            false,
        )
        .into_replies();
        assert_eq!(r.cell_px, Some((10, 20)));
        assert_eq!(r.xtversion.as_deref(), Some("a 1"));
        assert_eq!(r.da1, Some(vec![1]));
    }

    #[test]
    fn kitty_replies() {
        let reply =
            |bytes: &[u8], in_tmux, wrapped| parse_all(bytes, in_tmux, wrapped).into_replies();
        assert!(reply(b"\x1b_Gi=31;OK\x1b\\", false, false).kitty_ok);
        // WezTerm may add the image number.
        assert!(reply(b"\x1b_GI=5,i=31;OK\x1b\\", false, false).kitty_ok);
        assert!(!reply(b"\x1b_Gi=31;ENOTSUPPORTED:no\x1b\\", false, false).kitty_ok);
        assert!(!reply(b"\x1b_Gi=311;OK\x1b\\", false, false).kitty_ok);
        assert!(!reply(b"\x1b_Gi=31\x1b\\", false, false).kitty_ok);
        assert!(!reply(b"\x1b_i=31;OK\x1b\\", false, false).kitty_ok);
        // Inside tmux without the wrapped query, a kitty answer is noise.
        let r = reply(b"\x1b_Gi=31;OK\x1b\\", true, false);
        assert!(!r.kitty_ok && !r.outer_kitty_ok);
        let r = reply(b"\x1b_Gi=31;OK\x1b\\", true, true);
        assert!(r.outer_kitty_ok && !r.kitty_ok);
    }

    #[test]
    fn status_replies_only_count_when_asked_for() {
        assert!(!parse_all(b"\x1b[0n", false, false).replies().outer_dsr_seen);
        assert!(parse_all(b"\x1b[0n", true, true).replies().outer_dsr_seen);
        assert!(parse_all(b"\x1b[3n", true, true).replies().outer_dsr_seen);
        assert!(!parse_all(b"\x1b[5n", true, true).replies().outer_dsr_seen);
    }

    #[test]
    fn window_reports() {
        let r = parse_all(
            b"\x1b[6;0;0t\x1b[4;;1600t\x1b[8;50t\x1b[6;20;10;1t",
            false,
            false,
        )
        .into_replies();
        assert_eq!(r.cell_px, None);
        assert_eq!(r.text_area_px, None);
        assert_eq!(r.size_cells, None);
        let r = parse_all(b"\x1b[4;1000;1600t\x1b[8;0;0t", false, false).into_replies();
        assert_eq!(r.cell_size(), None, "no division by zero");
        let r = parse_all(b"\x1b[4;10;16t\x1b[8;50;160t", false, false).into_replies();
        assert_eq!(r.cell_size(), None, "cells smaller than a pixel");
        let r = parse_all(b"\x1b[6;99999;10t\x1b[6;020;010t", false, false).into_replies();
        assert_eq!(r.cell_px, Some((10, 20)), "overflowing answers are skipped");
    }

    #[test]
    fn x11_colors() {
        let c = |s: &str| parse_x11_color(s.as_bytes());
        assert_eq!(c("rgb:ffff/0000/8080"), Some(Rgb(255, 0, 128)));
        assert_eq!(c("rgb:1e1e/1e1e/2e2e"), Some(Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(c("rgb:f/0/8"), Some(Rgb(255, 0, 136)));
        assert_eq!(c("rgb:fff/000/800"), Some(Rgb(255, 0, 128)));
        assert_eq!(c("rgb:1e/1e/2e"), Some(Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(c("RGB:FF/Ee/dD"), Some(Rgb(255, 0xee, 0xdd)));
        assert_eq!(c("rgba:ffff/0000/0000/ffff"), Some(Rgb(255, 0, 0)));
        for bad in [
            "rgb:ffff/0000",
            "rgb:fffff/0/0",
            "rgb://",
            "rgb:1/2/3/4",
            "rgba:1/2/3",
            "rgb:g/0/0",
            "#ffffff",
            "",
            "rgb:",
        ] {
            assert_eq!(c(bad), None, "{bad}");
        }
        // Rounding: every 16-bit value maps to the nearest 8-bit one.
        for v in (0..=0xffffu32).step_by(257) {
            let s = format!("rgb:{v:04x}/{v:04x}/{v:04x}");
            let expected = u8::try_from(v / 257).unwrap();
            assert_eq!(c(&s), Some(Rgb(expected, expected, expected)));
        }
    }

    #[test]
    fn xtversion_text_is_cleaned() {
        let r = parse_all(b"\x1bP>|  evil\x08\x7fname\xff 1.0 \x1b\\", false, false).into_replies();
        assert_eq!(r.xtversion.as_deref(), Some("evilname\u{fffd} 1.0"));
        let r = parse_all(b"\x1bP>|\x1b\\", false, false).into_replies();
        assert_eq!(r.xtversion, None);
    }

    // --- the batch -------------------------------------------------------

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Positions of `ESC _ G` that are not the second half of a doubled ESC.
    fn unwrapped_apc(bytes: &[u8]) -> Vec<usize> {
        (0..bytes.len())
            .filter(|&i| bytes.get(i..i + 3) == Some(b"\x1b_G"))
            .filter(|&i| i == 0 || bytes.get(i - 1) != Some(&ESC))
            .collect()
    }

    #[test]
    fn tmux_wrap_doubles_escapes() {
        let mut out = Vec::new();
        tmux_wrap(b"\x1b_Gm=0;AAAA\x1b\\", &mut out);
        assert_eq!(out, b"\x1bPtmux;\x1b\x1b_Gm=0;AAAA\x1b\x1b\\\x1b\\");
    }

    #[test]
    fn batch_outside_tmux() {
        let b = Batch::new(Needs::ALL, false, None, true);
        let expected = [
            KITTY_QUERY,
            XTVERSION_QUERY,
            CELL_SIZE_QUERY,
            TEXT_AREA_PX_QUERY,
            TEXT_AREA_CELLS_QUERY,
            BACKGROUND_QUERY,
            DA1_QUERY,
        ]
        .concat();
        assert_eq!(b.bytes(), expected);
        assert!(!b.wrapped() && !b.in_tmux());
        assert!(b.bytes().ends_with(DA1_QUERY));
        assert!(
            !contains(b.bytes(), b"q="),
            "the kitty query must never carry q="
        );
        assert!(!contains(b.bytes(), STATUS_QUERY));
    }

    #[test]
    fn batch_for_background_only() {
        let b = Batch::new(
            Needs {
                background: true,
                graphics: false,
            },
            false,
            None,
            true,
        );
        assert_eq!(b.bytes(), [BACKGROUND_QUERY, DA1_QUERY].concat());
        let t = tmux_with(Passthrough::On);
        let b = Batch::new(
            Needs {
                background: true,
                graphics: false,
            },
            true,
            Some(&t),
            true,
        );
        assert!(!b.wrapped(), "no graphics, nothing for the outer terminal");
        assert_eq!(b.bytes(), [BACKGROUND_QUERY, DA1_QUERY].concat());
    }

    #[test]
    fn batch_inside_tmux_never_sends_an_unwrapped_kitty_query() {
        let off = tmux_with(Passthrough::Off);
        let on = tmux_with(Passthrough::On);
        let all = tmux_with(Passthrough::All);
        for (tmux, allowed, wrapped) in [
            (None, true, false),
            (Some(&off), true, false),
            (Some(&on), true, true),
            (Some(&all), true, true),
            (Some(&on), false, false),
        ] {
            let b = Batch::new(Needs::ALL, true, tmux, allowed);
            assert!(unwrapped_apc(b.bytes()).is_empty(), "{b:?}");
            assert_eq!(b.wrapped(), wrapped);
            assert!(b.bytes().ends_with(DA1_QUERY));
            assert!(!contains(b.bytes(), b"q="));
            if wrapped {
                let mut kitty = Vec::new();
                tmux_wrap(KITTY_QUERY, &mut kitty);
                let mut status = Vec::new();
                tmux_wrap(STATUS_QUERY, &mut status);
                assert!(b.bytes().starts_with(&[kitty, status].concat()));
                assert!(b.parser().wrapped);
            } else {
                assert!(!contains(b.bytes(), b"tmux;"));
                assert!(!contains(b.bytes(), STATUS_QUERY));
            }
        }
        // Outer terminals that cannot show placeholders are not asked: an
        // outer tmux (nested sessions) would take the query as a pane title.
        for line in [
            tmux::fixtures::VSCODE_CLIENT,
            "3.4|tmux 3.4|tmux-256color|256,RGB|16x32|on|external",
            "3.4||xterm-256color|256,RGB|0x0|all|external",
            "3.4|iTerm2 3.6.9|xterm-256color|256,sixel|0x0|on|external",
        ] {
            let client = tmux::parse(line).unwrap();
            let b = Batch::new(Needs::ALL, true, Some(&client), true);
            assert!(!b.wrapped(), "{line}");
            assert!(unwrapped_apc(b.bytes()).is_empty());
            assert!(!contains(b.bytes(), b"_G"), "{line}");
        }
        // Outside tmux the kitty query is unwrapped (the check above is real).
        let outside = Batch::new(Needs::ALL, false, None, true);
        assert_eq!(unwrapped_apc(outside.bytes()), [0]);
    }

    #[test]
    fn default_timeouts() {
        let t = tmux_with(Passthrough::On);
        let ssh_var = ("SSH_CONNECTION", "10.0.0.1 5 10.0.0.2 22");
        let tmux_var = ("TMUX", "/tmp/tmux-1001/default,1,0");
        let local = Env::from_pairs(&[("TERM", "xterm-kitty")]);
        let ssh = Env::from_pairs(&[ssh_var]);
        let ssh_tmux = Env::from_pairs(&[ssh_var, tmux_var]);
        let local_tmux = Env::from_pairs(&[tmux_var]);
        // `ssh` from a tmux pane: the answering tmux is across the connection.
        let remote_tmux = Env::from_pairs(&[ssh_var, ("TERM", "tmux-256color")]);
        let direct = Batch::new(Needs::ALL, false, None, true);
        let tmux_answers = Batch::new(Needs::ALL, true, None, true);
        let tmux_wrapped = Batch::new(Needs::ALL, true, Some(&t), true);
        assert_eq!(default_timeout(&local, &direct), LOCAL_TIMEOUT);
        assert_eq!(default_timeout(&ssh, &direct), SSH_TIMEOUT);
        assert_eq!(default_timeout(&ssh_tmux, &tmux_answers), LOCAL_TIMEOUT);
        assert_eq!(default_timeout(&ssh_tmux, &tmux_wrapped), SSH_TIMEOUT);
        assert_eq!(default_timeout(&local_tmux, &tmux_wrapped), LOCAL_TIMEOUT);
        assert_eq!(default_timeout(&remote_tmux, &tmux_answers), SSH_TIMEOUT);
    }

    #[test]
    fn multiplexer_terms_never_get_an_unwrapped_kitty_query() {
        let dir = RuntimeDir::new();
        for term in ["tmux-256color", "screen-256color", "screen"] {
            let env = dir.env(&[("TERM", term)]);
            let seen = std::cell::RefCell::new(Vec::new());
            probe_with(&request(&env), test_clock(), |batch, _| {
                seen.borrow_mut().extend_from_slice(batch.bytes());
                Err(io::Error::other("no terminal in tests"))
            });
            let bytes = seen.into_inner();
            assert!(bytes.ends_with(DA1_QUERY), "{term}");
            assert!(!contains(&bytes, b"\x1b_G"), "{term}");
        }
    }

    // --- the exchange ------------------------------------------------------

    /// A fake terminal on the other end of a socket pair: reads the batch,
    /// then plays `script` (bytes to send, pause after them).
    fn fake_terminal(
        batch: &Batch,
        script: Vec<(Vec<u8>, Duration)>,
    ) -> (
        filedescriptor::FileDescriptor,
        std::thread::JoinHandle<Vec<u8>>,
    ) {
        let (ours, mut theirs) = filedescriptor::socketpair().unwrap();
        let len = batch.bytes().len();
        let handle = std::thread::spawn(move || {
            let mut query = vec![0; len];
            theirs.read_exact(&mut query).unwrap();
            for (bytes, pause) in script {
                theirs.write_all(&bytes).unwrap();
                std::thread::sleep(pause);
            }
            query
        });
        (ours, handle)
    }

    #[test]
    fn exchange_reads_until_the_sentinel() {
        let batch = Batch::new(Needs::ALL, false, None, true);
        let script = KITTY
            .chunks(7)
            .map(|c| (c.to_vec(), Duration::ZERO))
            .collect();
        let (mut tty, terminal) = fake_terminal(&batch, script);
        let outcome = exchange(&mut tty, &batch, Duration::from_secs(5), DRAIN).unwrap();
        assert_eq!(terminal.join().unwrap(), batch.bytes());
        assert_eq!(outcome.status, ProbeStatus::Complete);
        assert_eq!(&outcome.replies, parse_all(KITTY, false, false).replies());
        assert!(outcome.elapsed < Duration::from_secs(5));
    }

    #[test]
    fn exchange_times_out_without_the_sentinel() {
        let batch = Batch::new(Needs::ALL, false, None, true);
        let partial = KITTY[..KITTY.len() - 10].to_vec();
        let script = vec![(partial, Duration::from_millis(300))];
        let (mut tty, terminal) = fake_terminal(&batch, script);
        let timeout = Duration::from_millis(60);
        let outcome = exchange(&mut tty, &batch, timeout, Duration::ZERO).unwrap();
        assert_eq!(outcome.status, ProbeStatus::TimedOut);
        assert!(outcome.replies_may_follow());
        assert!(outcome.elapsed >= timeout);
        assert!(
            outcome.replies.kitty_ok,
            "replies before the deadline count"
        );
        assert_eq!(outcome.replies.da1, None);
        terminal.join().unwrap();
    }

    #[test]
    fn exchange_drains_late_bytes() {
        let batch = Batch::new(Needs::ALL, true, None, true);
        let script = vec![
            (TMUX_LOCAL.to_vec(), Duration::from_millis(20)),
            // A straggler after the sentinel; then the terminal hangs up.
            (b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(), Duration::ZERO),
        ];
        let (mut tty, terminal) = fake_terminal(&batch, script);
        let outcome = exchange(
            &mut tty,
            &batch,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        terminal.join().unwrap();
        assert_eq!(outcome.status, ProbeStatus::Complete);
        assert!(outcome.elapsed < Duration::from_secs(5));
        assert_eq!(outcome.replies.background, Some(Rgb(0, 0, 0)));
    }

    #[test]
    fn exchange_stops_at_hang_up() {
        let batch = Batch::new(Needs::ALL, false, None, true);
        // The terminal reads the batch and hangs up without answering.
        let (mut tty, terminal) = fake_terminal(&batch, vec![]);
        let outcome = exchange(&mut tty, &batch, Duration::from_secs(5), DRAIN).unwrap();
        terminal.join().unwrap();
        assert_eq!(outcome.status, ProbeStatus::TimedOut);
        assert!(outcome.elapsed < Duration::from_secs(5));
    }

    #[test]
    fn wait_readable_honours_the_deadline() {
        let (ours, _theirs) = filedescriptor::socketpair().unwrap();
        let start = Instant::now();
        let ready = wait_readable(ours.as_raw_fd(), start + Duration::from_millis(20)).unwrap();
        assert!(!ready);
        assert!(start.elapsed() >= Duration::from_millis(20));
        assert!(
            !wait_readable(ours.as_raw_fd(), start).unwrap(),
            "past deadline"
        );
    }

    // --- the cache ---------------------------------------------------------

    /// A private runtime directory for one test.
    struct RuntimeDir(PathBuf);

    impl RuntimeDir {
        fn new() -> RuntimeDir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("emde-probe-test-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            RuntimeDir(dir)
        }

        fn env(&self, extra: &[(&str, &str)]) -> Env {
            let mut pairs = vec![("XDG_RUNTIME_DIR", self.0.to_str().unwrap())];
            pairs.extend_from_slice(extra);
            Env::from_pairs(&pairs)
        }
    }

    impl Drop for RuntimeDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn request(env: &Env) -> ProbeRequest<'_> {
        ProbeRequest {
            env,
            tmux: None,
            needs: Needs::ALL,
            timeout: None,
            reprobe: false,
            tmux_passthrough: true,
        }
    }

    /// A runner that answers with `bytes` after pretending it took `elapsed`.
    fn answering<'c>(
        bytes: &'static [u8],
        elapsed: Duration,
        calls: &'c AtomicUsize,
    ) -> impl FnOnce(&Batch, Duration) -> io::Result<ProbeOutcome> + 'c {
        move |batch: &Batch, _timeout: Duration| {
            calls.fetch_add(1, Ordering::Relaxed);
            let mut parser = batch.parser();
            parser.feed(bytes);
            Ok(ProbeOutcome {
                status: if parser.is_done() {
                    ProbeStatus::Complete
                } else {
                    ProbeStatus::TimedOut
                },
                replies: parser.into_replies(),
                elapsed,
                wrapped: batch.wrapped(),
            })
        }
    }

    const SLOW: Duration = Duration::from_millis(30);

    /// A whole-second clock: cache files store seconds.
    fn test_clock() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_790_000_000)
    }

    #[test]
    fn slow_probes_are_cached() {
        let dir = RuntimeDir::new();
        let env = dir.env(&[("TERM", "xterm-256color"), ("LC_TERMINAL", "iTerm2")]);
        let req = request(&env);
        let calls = AtomicUsize::new(0);
        let now = test_clock();
        let first = probe_with(&req, now, answering(ITERM2, SLOW, &calls));
        assert_eq!(first.status, ProbeStatus::Complete);
        let later = now + Duration::from_secs(3600);
        let second = probe_with(&req, later, answering(ITERM2, SLOW, &calls));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "the second probe is cached"
        );
        assert_eq!(
            second.status,
            ProbeStatus::Cached {
                age: Duration::from_secs(3600)
            }
        );
        assert_eq!(second.elapsed, SLOW);
        // The cell size survives as a derived value; window sizes do not.
        let expected = ProbeReplies {
            cell_px: first.replies.cell_size(),
            text_area_px: None,
            size_cells: None,
            ..first.replies.clone()
        };
        assert_eq!(second.replies, expected);
        // --reprobe asks again and refreshes the entry.
        let reprobe = ProbeRequest {
            reprobe: true,
            ..req
        };
        let third = probe_with(&reprobe, later, answering(ITERM2, SLOW, &calls));
        assert_eq!(third.status, ProbeStatus::Complete);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let fourth = probe_with(&req, later, answering(ITERM2, SLOW, &calls));
        assert_eq!(
            fourth.status,
            ProbeStatus::Cached {
                age: Duration::ZERO
            }
        );
    }

    #[test]
    fn fast_timed_out_and_failed_probes_are_not_cached() {
        let dir = RuntimeDir::new();
        let env = dir.env(&[("TERM", "xterm-kitty")]);
        let req = request(&env);
        let calls = AtomicUsize::new(0);
        let now = test_clock();
        probe_with(
            &req,
            now,
            answering(KITTY, Duration::from_millis(3), &calls),
        );
        probe_with(&req, now, answering(&KITTY[..20], SLOW, &calls));
        let failed = probe_with(&req, now, |_, _| Err(io::Error::other("no tty")));
        assert_eq!(failed.status, ProbeStatus::Failed("no tty".into()));
        probe_with(&req, now, answering(KITTY, SLOW, &calls));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            3,
            "nothing was cached before"
        );
    }

    #[test]
    fn cache_entries_expire() {
        let dir = RuntimeDir::new();
        let env = dir.env(&[("TERM", "xterm-kitty")]);
        let req = request(&env);
        let calls = AtomicUsize::new(0);
        let now = test_clock();
        probe_with(&req, now, answering(KITTY, SLOW, &calls));
        let stale = now + CACHE_TTL + Duration::from_secs(1);
        let again = probe_with(&req, stale, answering(KITTY, SLOW, &calls));
        assert_eq!(again.status, ProbeStatus::Complete);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn cache_keys_separate_sessions_and_batches() {
        let dir = RuntimeDir::new();
        let a = dir.env(&[("TERM", "xterm-kitty"), ("SSH_CONNECTION", "1 2 3 4")]);
        let b = dir.env(&[("TERM", "xterm-kitty"), ("SSH_CONNECTION", "1 2 3 5")]);
        let c = dir.env(&[("TERM", "xterm-kitty"), ("SSH_CONNECTION", "")]);
        let d = dir.env(&[("TERM", "xterm-kitty")]);
        let batch = Batch::new(Needs::ALL, false, None, true);
        let path = |env: &Env| Cache::new(env, None, &batch).unwrap().path().to_path_buf();
        assert_ne!(path(&a), path(&b));
        assert_ne!(path(&c), path(&d), "empty differs from unset");
        assert_eq!(path(&a), path(&a.clone()));
        assert!(path(&a).starts_with(dir.0.join("emde")));
        let name = path(&a).file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.starts_with("caps-") && name.len() == 5 + 16, "{name}");
        // Another batch shape or another tmux client: another entry.
        let bg_only = Batch::new(
            Needs {
                background: true,
                graphics: false,
            },
            false,
            None,
            true,
        );
        assert_ne!(Cache::new(&a, None, &bg_only).unwrap().path(), path(&a));
        let off = tmux_with(Passthrough::Off);
        let on = tmux_with(Passthrough::On);
        let tmux_batch = Batch::new(Needs::ALL, true, None, true);
        assert_ne!(
            Cache::new(&a, Some(&off), &tmux_batch).unwrap().path(),
            Cache::new(&a, Some(&on), &tmux_batch).unwrap().path()
        );
        // No runtime directory, no cache.
        assert_eq!(Cache::new(&Env::default(), None, &batch), None);
        assert_eq!(
            Cache::new(&Env::from_pairs(&[("XDG_RUNTIME_DIR", "")]), None, &batch),
            None
        );
    }

    #[test]
    fn cache_format_round_trip() {
        let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let outcome = ProbeOutcome {
            replies: parse_all(ITERM2_VIA_TMUX, true, true).into_replies(),
            status: ProbeStatus::Complete,
            elapsed: Duration::from_micros(38_500),
            wrapped: true,
        };
        let text = encode(&outcome, Needs::ALL, now);
        assert_eq!(
            text,
            "emde-probe-cache 1\ncreated=1790000000\nneeds=background,graphics\nwrapped=1\n\
             elapsed_us=38500\nkitty_ok=0\nouter_kitty_ok=1\nouter_dsr_seen=1\n\
             tmux_own_da1_sixel=1\nxtversion=tmux 3.4\ncell_px=16x32\nbackground=#1e1e2e\n\
             da1=1;2;4\n"
        );
        let entry = decode(&text, now + Duration::from_secs(5)).unwrap();
        assert_eq!(entry.age, Duration::from_secs(5));
        assert_eq!(entry.needs, Needs::ALL);
        assert!(entry.wrapped);
        assert_eq!(entry.elapsed, outcome.elapsed);
        let expected = ProbeReplies {
            text_area_px: None,
            size_cells: None,
            ..outcome.replies
        };
        assert_eq!(entry.replies, expected);
    }

    #[test]
    fn malformed_cache_files_are_misses() {
        let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let good = "emde-probe-cache 1\ncreated=1790000000\nneeds=graphics\n";
        assert!(decode(good, now).is_some());
        assert!(decode(&format!("{good}future_key=whatever\n"), now).is_some());
        for bad in [
            "",
            "emde-probe-cache 2\ncreated=1790000000\nneeds=graphics\n",
            "emde-probe-cache 1\nneeds=graphics\n",
            "emde-probe-cache 1\ncreated=1790000000\n",
            "emde-probe-cache 1\ncreated=soon\nneeds=graphics\n",
            "emde-probe-cache 1\ncreated=1790000000\nneeds=colour\n",
            "emde-probe-cache 1\ncreated=1790000000\nneeds=graphics\nkitty_ok=yes\n",
            "emde-probe-cache 1\ncreated=1790000000\nneeds=graphics\ncell_px=0x0\n",
            "emde-probe-cache 1\ncreated=1790000000\nneeds=graphics\nbackground=red\n",
            "emde-probe-cache 1\ncreated=1790000000\nneeds=graphics\nda1=1;x\n",
            "emde-probe-cache 1\ncreated=1790000000\nneeds=graphics\nno equals sign\n",
            "emde-probe-cache 1\ncreated=18446744073709551615\nneeds=graphics\n",
            // Written an hour in the future.
            "emde-probe-cache 1\ncreated=1790003600\nneeds=graphics\n",
        ] {
            assert_eq!(decode(bad, now), None, "{bad:?}");
        }
        // Small clock skew is tolerated.
        let skewed = "emde-probe-cache 1\ncreated=1790000030\nneeds=graphics\n";
        assert_eq!(decode(skewed, now).unwrap().age, Duration::ZERO);
    }

    #[test]
    fn unreadable_cache_is_a_miss() {
        let dir = RuntimeDir::new();
        let env = dir.env(&[("TERM", "xterm-kitty")]);
        let batch = Batch::new(Needs::ALL, false, None, true);
        let cache = Cache::new(&env, None, &batch).unwrap();
        assert_eq!(cache.load(SystemTime::now()), None, "missing file");
        fs::create_dir_all(cache.path().parent().unwrap()).unwrap();
        fs::write(cache.path(), b"\xff\xfe binary junk").unwrap();
        assert_eq!(cache.load(SystemTime::now()), None, "junk file");
    }

    #[test]
    fn probe_skips_when_nothing_is_needed() {
        let env = Env::default();
        let req = ProbeRequest {
            needs: Needs::default(),
            ..request(&env)
        };
        assert_eq!(probe(&req), None);
    }

    // --- late replies ------------------------------------------------------

    /// How a late reply reaches the input layer, unit by unit.
    fn as_units(bytes: &[u8]) -> Vec<InputUnit> {
        let mut units = Vec::new();
        let mut it = bytes.iter().copied();
        while let Some(b) = it.next() {
            units.push(match b {
                ESC => InputUnit::Alt(it.next().map_or('\u{1b}', char::from)),
                BEL => InputUnit::Bel,
                0x20..=0x7e => InputUnit::Char(char::from(b)),
                _ => InputUnit::Other,
            });
        }
        units
    }

    fn swallowed(filter: &mut LateReplyFilter, now: Instant, units: &[InputUnit]) -> Vec<bool> {
        units.iter().map(|&u| filter.swallow(now, u)).collect()
    }

    #[test]
    fn late_replies_are_swallowed() {
        let now = Instant::now();
        for reply in [
            &b"\x1b_Gi=31;OK\x1b\\"[..],
            b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\",
            b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07",
            b"\x1bP>|iTerm2 3.6.9\x1b\\",
        ] {
            let mut f = LateReplyFilter::new(now, LATE_REPLY_GRACE);
            let units = as_units(reply);
            assert!(
                swallowed(&mut f, now, &units).iter().all(|&s| s),
                "{reply:?}"
            );
            // Keys after the reply pass.
            assert!(!f.swallow(now, InputUnit::Char('j')));
            assert!(!f.swallow(now, InputUnit::Other));
        }
    }

    #[test]
    fn ordinary_keys_pass() {
        let now = Instant::now();
        let mut f = LateReplyFilter::new(now, LATE_REPLY_GRACE);
        for unit in [
            InputUnit::Char('q'),
            InputUnit::Char('G'),
            InputUnit::Alt('x'),
            InputUnit::Alt('\\'),
            InputUnit::Bel,
            InputUnit::Other,
        ] {
            assert!(!f.swallow(now, unit), "{unit:?}");
        }
    }

    #[test]
    fn a_broken_reply_stops_swallowing() {
        let now = Instant::now();
        let mut f = LateReplyFilter::new(now, LATE_REPLY_GRACE);
        assert!(f.swallow(now, InputUnit::Alt(']')));
        assert!(f.swallow(now, InputUnit::Char('1')));
        // Enter cannot be part of a reply: it passes, and so does what follows.
        assert!(!f.swallow(now, InputUnit::Other));
        assert!(!f.swallow(now, InputUnit::Char('j')));
        // BEL only ends OSC; in a DCS it is a key.
        assert!(f.swallow(now, InputUnit::Alt('P')));
        assert!(!f.swallow(now, InputUnit::Bel));
        assert!(!f.swallow(now, InputUnit::Char('k')));
    }

    #[test]
    fn the_grace_period_ends() {
        let start = Instant::now();
        let mut f = LateReplyFilter::new(start, Duration::from_millis(100));
        assert!(f.is_active(start));
        let late = start + Duration::from_millis(150);
        assert!(!f.is_active(late));
        assert!(!f.swallow(late, InputUnit::Alt('_')));
        // A reply that starts in time is swallowed to its end.
        let mut f = LateReplyFilter::new(start, Duration::from_millis(100));
        assert!(f.swallow(start, InputUnit::Alt('_')));
        assert!(f.is_active(late));
        assert!(f.swallow(late, InputUnit::Char('G')));
        assert!(f.swallow(late, InputUnit::Alt('\\')));
        assert!(!f.is_active(late));
        assert!(!f.swallow(late, InputUnit::Char('G')));
    }

    #[test]
    fn filter_follows_the_probe_outcome() {
        let now = Instant::now();
        let outcome = |status| ProbeOutcome {
            replies: ProbeReplies::default(),
            status,
            elapsed: Duration::ZERO,
            wrapped: false,
        };
        assert!(LateReplyFilter::after(Some(&outcome(ProbeStatus::TimedOut)), now).is_active(now));
        assert!(!LateReplyFilter::after(Some(&outcome(ProbeStatus::Complete)), now).is_active(now));
        assert!(!LateReplyFilter::after(None, now).is_active(now));
        assert!(!LateReplyFilter::inactive().is_active(now));
    }

    #[test]
    fn crossterm_keys_map_to_units() {
        let key = |code, mods| InputUnit::from_key(&KeyEvent::new(code, mods));
        // What crossterm makes of `ESC _`, `ESC P`, `ESC \`, `G`, BEL.
        assert_eq!(
            key(KeyCode::Char('_'), KeyModifiers::ALT),
            InputUnit::Alt('_')
        );
        assert_eq!(
            key(KeyCode::Char('P'), KeyModifiers::ALT | KeyModifiers::SHIFT),
            InputUnit::Alt('P')
        );
        assert_eq!(
            key(KeyCode::Char('\\'), KeyModifiers::ALT),
            InputUnit::Alt('\\')
        );
        assert_eq!(
            key(KeyCode::Char('G'), KeyModifiers::SHIFT),
            InputUnit::Char('G')
        );
        assert_eq!(
            key(KeyCode::Char('g'), KeyModifiers::CONTROL),
            InputUnit::Bel
        );
        assert_eq!(
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            InputUnit::Other
        );
        assert_eq!(key(KeyCode::Enter, KeyModifiers::NONE), InputUnit::Other);
        assert_eq!(key(KeyCode::Esc, KeyModifiers::NONE), InputUnit::Other);
    }

    proptest! {
        #[test]
        fn parser_is_total_and_split_invariant(
            bytes in proptest::collection::vec(any::<u8>(), 0..400),
            split in 0usize..400,
            in_tmux: bool,
            wrapped: bool,
        ) {
            let whole = parse_all(&bytes, in_tmux, wrapped);
            let split = split.min(bytes.len());
            let mut p = ReplyParser::new(in_tmux, wrapped);
            p.feed(&bytes[..split]);
            p.feed(&bytes[split..]);
            prop_assert_eq!(p.replies(), whole.replies());
        }

        #[test]
        fn parser_survives_reply_shaped_noise(
            parts in proptest::collection::vec(
                prop_oneof![
                    Just(b"\x1b".to_vec()),
                    Just(b"\x1b\\".to_vec()),
                    Just(b"\x07".to_vec()),
                    Just(b"\x1b[".to_vec()),
                    Just(b"\x1b]11;".to_vec()),
                    Just(b"\x1bP>|".to_vec()),
                    Just(b"\x1b_G".to_vec()),
                    Just(b"rgb:".to_vec()),
                    Just(b"i=31;OK".to_vec()),
                    proptest::collection::vec(any::<u8>(), 0..8),
                ],
                0..60,
            ),
        ) {
            let bytes = parts.concat();
            let mut p = ReplyParser::new(true, true);
            p.feed(&bytes);
            let _ = p.is_done();
        }

        #[test]
        fn cache_decoding_is_total(text in ".{0,300}") {
            let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
            let _ = decode(&text, now);
            let _ = decode(&format!("{CACHE_MAGIC}\n{text}"), now);
        }

        #[test]
        fn filter_is_total(units in proptest::collection::vec(
            prop_oneof![
                any::<char>().prop_map(InputUnit::Alt),
                any::<char>().prop_map(InputUnit::Char),
                Just(InputUnit::Bel),
                Just(InputUnit::Other),
            ],
            0..200,
        )) {
            let now = Instant::now();
            let mut f = LateReplyFilter::new(now, LATE_REPLY_GRACE);
            for u in units {
                let _ = f.swallow(now, u);
            }
        }
    }
}
