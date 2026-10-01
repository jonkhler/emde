//! The HTML tag lexer and entity decoder on arbitrary text (lossy UTF-8).
//!
//! Checked (by `emde::parse::fuzz_html`, next to the lexer): every token
//! consumes input and the tokens cover it in order, tags and comments are
//! delimited as their kind says, and attribute lookup, entity decoding and
//! length parsing never fail.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(init: emde_fuzz::init(), |data: &[u8]| {
    emde::parse::fuzz_html(&String::from_utf8_lossy(data));
});
