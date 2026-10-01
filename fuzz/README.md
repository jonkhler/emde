# emde's fuzz targets

cargo-fuzz (libFuzzer) targets for the parts of emde that take untrusted
input. Run them with `cargo xtask fuzz` from the repository root:

```sh
rustup toolchain install nightly --profile minimal   # once
cargo install --locked cargo-fuzz                    # once
cargo xtask fuzz                    # every target for 60 s
cargo xtask fuzz --secs 3600 math   # one target for an hour
cargo xtask fuzz --jobs 8 render    # eight processes
```

| Target | Input | Checks |
|---|---|---|
| `render` | option bytes + a document | parse → layout at two widths → plain and styled output, with figures as boxes and as block-glyph images: no line wider than the width; only emde's own escapes (SGR, and OSC 8 links closed on each line with safe, encoded URIs); the layout's invariants and side tables (link boxes, headings, figures, `line_at`); the same layout after laying out at another width |
| `math` | option bytes + TeX | `emde_math::inline` and `display`: spans cover the text, widths are right, boxes fit and are rectangular, deterministic |
| `probe` | option bytes + terminal replies (+ NUL + tmux output) | `ReplyParser` gives the same answers however the bytes are split; sizes and names are sane; `--doctor` output stays printable |
| `html` | text | the HTML lexer's invariants (`emde::parse::fuzz_html`) |
| `config` | config file, theme files and `--set`s, NUL-separated | loading never panics, diagnostics are printable, a document laid out with the settings fits and holds only emde's escapes |

`cargo xtask fuzz` seeds each target from `tests/fixtures/`, `assets/`, the
TOML examples of the documentation and recorded terminal replies (see
`xtask/src/fuzz.rs`). What libFuzzer finds is kept in `corpus/<target>/`;
an input that fails a check, or takes more than 10 seconds, is saved in
`artifacts/<target>/`. Replay one with

```sh
rustup run nightly cargo fuzz run --fuzz-dir fuzz -s none <target> fuzz/artifacts/<target>/<file>
```

and add it as a regression test to the tests of the module it broke.

libFuzzer first runs every input of the corpus, and `--secs` only counts
the time after that, so a large corpus makes runs start slowly. Shrink it
to the inputs that matter now and then:

```sh
rustup run nightly cargo fuzz cmin --fuzz-dir fuzz -s none <target>
```

Panics are findings, except panics inside pulldown-latex, which emde catches
on purpose (the formula is shown as TeX); `src/lib.rs` has that policy. The
targets are built without AddressSanitizer by default (emde has no unsafe
code; `--asan` turns it on).

This directory is a Cargo workspace of its own, outside emde's: it needs
nightly, and libFuzzer is never part of emde.
