# emde

An ultra-fast terminal Markdown reader: full colour, syntax-highlighted
code, LaTeX math, images, clickable links and a built-in pager, in one small
binary.

```sh
emde README.md
```

## Features

- **Rendering for the terminal.** GitHub-flavoured Markdown, with tables,
  task lists, footnotes, alerts, definition lists, front matter and the
  HTML that READMEs use (`<img>`, `<details>`, `<kbd>`, `align="center"`,
  …). Text is wrapped to the window with hanging indents, tables shrink to
  fit (or turn into cards), and long lines wrap instead of being cut off.
  Every line ends with a reset, so styles never bleed, and a document can
  never send escape sequences of its own to the terminal.
- **A built-in pager.** On a terminal, a document taller than the screen
  opens in the pager: incremental search (smart case, regular expressions,
  matches across wrapped lines), an outline, jumps between headings, link
  focus and link hints, back and forward through linked documents, reload
  when the file changes, the mouse wheel, and re-wrapping on resize that
  keeps your place. Short documents and pipes get the rendered text, like
  `bat` does.
- **Math.** `$…$`, `$$…$$`, `\(…\)`, `\[…\]` and ` ```math ` blocks.
  Inline math is one line of Unicode (`α² + a/b`); display math is laid out
  in two dimensions, with stacked fractions, limits above and below, tall
  brackets, matrices and cases:

  ```text
           n       n(n+1)(2n+1)          A = ⎛a  b⎞          ⎧ 1  x > 0
           ∑  i² = ────────────              ⎝c  d⎠   f(x) = ⎨
          i=1           6                                    ⎩ 0  otherwise
  ```

- **Syntax highlighting** for nearly 200 languages, with bat's code themes
  (`emde --list-code-themes`), any `.tmTheme` file, or the `ansi` code theme,
  which uses your terminal's own colours.
- **Images.** Real pixels where the terminal can show them: kitty graphics
  (Unicode placeholders, which scroll with the text, also through tmux),
  iTerm2 inline images and sixel. Everywhere else, images are drawn with
  coloured block characters. Figures are sized from the image's header
  before it is decoded, so the text never jumps. Remote images are only
  fetched when you ask (`--remote-images`).
- **Themes.** The default theme has Catppuccin colours for dark and light
  backgrounds and picks the variant from the terminal's background colour.
  `ansi` follows your terminal's colour scheme and `mono` uses text
  attributes only. Every element can be restyled in a TOML file, and
  24-bit colours are converted for 256- and 16-colour terminals.
- **Clickable links** (OSC 8) where the terminal supports them, numbered
  references (`docs[1]`) where it does not.

## Building

emde is written in Rust (1.90 or newer). In this repository the toolchain
and the build output live outside the source tree; `scripts/env.sh` sets
that up (see the comments in it):

```sh
source scripts/env.sh
cargo build --release          # → $CARGO_TARGET_DIR/release/emde
cargo install --path .         # or install it into $CARGO_HOME/bin
```

Cargo features (`cargo build --release --no-default-features --features …`):

| Feature | Default | What it adds |
|---|---|---|
| `highlight` | yes | Syntax highlighting (syntect with two-face's syntaxes and themes). Needs a regex engine: `onig` or `fancy`. |
| `onig` | yes | The Oniguruma regex engine (C). 10–35 times faster than `fancy` the first time a language is used. |
| `fancy` | no | The pure-Rust fancy-regex engine instead (when both are on, `onig` wins). |
| `tmtheme` | yes | Code themes from `.tmTheme` files. |
| `images` | yes | PNG, JPEG, GIF and WebP images. |
| `sixel` | yes | Sixel output, for terminals without kitty or iTerm2 graphics. |
| `svg` | no | SVG images (resvg; off by default because of its size). |
| `simd` | yes | Faster Markdown parsing with SSSE3 (detected at run time; x86-64 only). |

`cargo build --release --no-default-features --features highlight,fancy,tmtheme,images,sixel,simd`
builds without Oniguruma, so no C compiler is needed.

## Usage

```sh
emde README.md                       # read a document (in the pager when it is long)
emde docs/guide.md#installation      # open the pager at a heading
emde .                               # a directory shows its README
curl -sL https://example.org/README.md | emde   # read standard input
emde -p notes.md                     # write the rendered text instead of paging
emde -p --color=always notes.md | less -R       # …and keep the colours
emde --set render.max_width=80 --set theme.code=Nord notes.md
emde --doctor                        # what emde found out about the terminal
```

`emde --help` lists every option; the most useful ones:

| Option | |
|---|---|
| `-p`, `--plain` | No pager: write the rendered document to standard output. |
| `--paging auto\|always\|never` | When to use the pager (`auto`: on a terminal, for long documents). |
| `-w`, `--width N` · `-m`, `--max-width N` | The width, and the widest text column (default 100). |
| `--color auto\|always\|never\|truecolor\|256\|16` | Colours, also when piping. |
| `-t`, `--theme NAME\|PATH` · `--background dark\|light` | The theme and its variant. |
| `--code-theme NAME\|PATH` | The code theme (`--list-code-themes`). |
| `--images auto\|kitty\|iterm\|sixel\|blocks\|none` | How to show images. |
| `--toc` | Open the pager with the outline shown. |
| `-s`, `--set KEY=VALUE` | Change any setting for this run (see [Configuration](#configuration)). |
| `--doctor[=json]` | Show every terminal decision and why it was made. |
| `--print-default-config` · `--check-config` | The settings with their defaults; check your config file. |

## The pager

Keys work like in `less` and `vim`; `h` shows them all. A count before a
command repeats it or gives it a line or a percentage: `5j`, `120g`, `50%`.
Links can be followed into other Markdown files, and `Backspace` comes back.

<!-- BEGIN GENERATED keys (`cargo xtask docs`, from src/pager/keymap.rs) -->

**Scrolling**

| Keys | Action |
|---|---|
| `0`–`9` | count for the next command (5j, 50%, 120g) |
| `j` `↓` `^E` `^N` | down a line |
| `k` `↑` `^Y` `^P` | up a line |
| `Space` `f` `PgDn` `^F` | down a page |
| `b` `PgUp` `^B` | up a page |
| `d` `^D` | down half a page |
| `u` `^U` | up half a page |
| `g` `Home` `<` | top (line N with a count) |
| `G` `End` `>` | bottom (line N with a count) |
| `%` | N percent into the document |

**Jumping**

| Keys | Action |
|---|---|
| `]` | next heading |
| `[` | previous heading |
| `}` | next heading of the same or a higher level |
| `{` | previous heading of the same or a higher level |
| `t` | outline (type to filter) |

**Searching**

| Keys | Action |
|---|---|
| `/` | search forward |
| `?` | search backward |
| `n` | next match |
| `N` | previous match |
| `Esc` | clear the search (then the link focus) |

**Links**

| Keys | Action |
|---|---|
| `Tab` | focus the next link |
| `S-Tab` | focus the previous link |
| `Enter` | follow the focused link |
| `o` | link hints: type a label to follow |
| `Backspace` `H` | back |
| `L` | forward |
| `y` | copy the focused link's URL |

**Other**

| Keys | Action |
|---|---|
| `r` | reload the file |
| `R` | watch the file for changes, on or off |
| `i` | cycle the image mode |
| `w` | full width, on or off |
| `m` | mouse, on or off (off: select text) |
| `h` `F1` | this help |
| `^L` | redraw the screen (anywhere) |
| `^Z` | suspend (anywhere) |
| `q` `^C` | quit |

**In the search prompt**

| Keys | Action |
|---|---|
| `Enter` | search |
| `Esc` `^C` | cancel |
| `Backspace` | delete a character |
| `^U` | clear the pattern |

**In the outline**

| Keys | Action |
|---|---|
| `↓` `^N` `^J` | next entry |
| `↑` `^P` `^K` | previous entry |
| `PgDn` | down a page |
| `PgUp` | up a page |
| `Enter` | go to the heading |
| `Esc` `^C` | close |
| `Backspace` | delete a filter character |

**With link hints**

| Keys | Action |
|---|---|
| `Esc` `^C` | cancel |
| `Backspace` | delete a label character |

**In this help**

| Keys | Action |
|---|---|
| `j` `↓` | down a line |
| `k` `↑` | up a line |
| `Space` `f` `PgDn` | down a page |
| `b` `PgUp` | up a page |
| `q` `h` `Esc` `F1` `^C` | close |

<!-- END GENERATED keys -->

In the search prompt, the outline and link hints, other keys type text.

## Configuration

emde reads `~/.config/emde/config.toml`, or `$XDG_CONFIG_HOME/emde/config.toml`
when `XDG_CONFIG_HOME` is set (the same on Linux and macOS). `--config PATH`
or `$EMDE_CONFIG` name another file, and `--no-config` reads none. Themes
go in `~/.config/emde/themes/NAME.toml`.

Everything is optional: a key you leave out keeps its default. For example,
an accent colour of your own, and level-1 headings as bold accent text with
a curly underline:

```toml
[palette]
accent = "#89b4fa"
muted = "#6c7086"

[style.h1]
fg = "accent"
bold = true
underline = "curly"
```

Settings are applied in layers: the defaults, the theme, your file, each
`--set KEY=VALUE`, and the options. `emde --print-default-config` prints
every setting with its default and an explanation, and `emde --check-config`
reports unknown keys (with suggestions) and bad values.

**[CONFIG.md](CONFIG.md)** describes every setting, the colour syntax, the
styleable elements and the theme file format.

## Terminal support

emde detects what the terminal can do from the environment and, when it
needs to, from one quick query to the terminal (`emde --doctor` shows the
result). What to expect:

| Terminal | Colour, links, underlines | Images |
|---|---|---|
| iTerm2, also over SSH | 24-bit colour, OSC 8 links, curly underlines | kitty placeholders (iTerm2 3.6 or newer), else iTerm2 inline images |
| VS Code and code-server | 24-bit colour, OSC 8 links, curly underlines | kitty graphics when `terminal.integrated.enableImages` is on, else blocks |
| tmux in either of those | 24-bit colour, OSC 8 links (tmux 3.4 or newer), curly underlines | kitty placeholders with `allow-passthrough on` and iTerm2 outside; else blocks |
| kitty, Ghostty, WezTerm, iTerm2 on a Mac | 24-bit colour, OSC 8 links, curly underlines | kitty placeholders (kitty, Ghostty, iTerm2), iTerm2 inline images (WezTerm) |

Other terminals get what they announce: 256 or 16 colours (with the `ansi`
theme at 16), sixel images where the terminal reports sixel support, and
block images elsewhere. `NO_COLOR` turns colours off (the `mono` theme);
`TERM=dumb` and pipes get plain text.

## Tips

**tmux.**

- Real images inside tmux need passthrough, which is off by default. Add
  this to `~/.tmux.conf`, and emde uses kitty Unicode placeholders through
  iTerm2 (or kitty, or Ghostty), real pixels that scroll with the text:

  ```tmux
  set -g allow-passthrough on
  ```

  emde never changes tmux options itself. Without passthrough, images are
  drawn with block characters.
- Inside tmux `COLORTERM` is usually unset (tmux does not pass it on, and
  SSH does not forward it), so programs think they have 256 colours. emde
  knows that tmux converts colours for each client and uses 24-bit colour
  inside tmux anyway. If colours look wrong in tmux, tell tmux your
  terminal has 24-bit colour: `set -as terminal-features ',xterm-256color:RGB'`.
- Copying a link's URL (`y` in the pager) uses OSC 52. With tmux's default
  `set-clipboard external`, tmux ignores OSC 52 from programs, so emde puts
  the URL into a tmux buffer with `tmux load-buffer -w`, which also passes
  it on to your terminal's clipboard. `set -g set-clipboard on` lets OSC 52
  through directly. Either way the outer terminal has to allow clipboard
  access (in iTerm2: Settings → General → Selection → *Applications in
  terminal may access clipboard*).

**iTerm2.** Math uses bracket pieces (`⎛ ⎜ ⎝`, `⌠ ⎮ ⌡`), and sextant and
octant images use characters from Unicode's legacy computing blocks, which
many fonts lack. Set a font that has them as the font for non-ASCII text
(Settings → Profiles → Text → *Use a different font for non-ASCII text*),
for example [JuliaMono](https://juliamono.netlify.app/).

**VS Code.** The integrated terminal shows images only with
`"terminal.integrated.enableImages": true` in your settings. Restart the
terminal afterwards and run `emde --doctor --reprobe`.

## Troubleshooting

`emde --doctor` shows each decision with its reason, and tips for getting
more out of the terminal. In iTerm2 → SSH → tmux 3.4 it says:

```text
 terminal  tmux 3.4 → iTerm2 3.6.9, 214×54, SSH   colour truecolor (in tmux)   background dark (tmux OSC 11)
 links     OSC 8 ✓   underline curly ✓   images blocks(half) — kitty/iterm ✗ passthrough off · sixel ✗ client cell 0x0
 probe     tmux answered in 2 ms   cell size unknown (client cell 0x0)
 tip       `set -g allow-passthrough on` → kitty Unicode placeholders via iTerm2: real pixels that scroll with the text
```

- `emde --doctor=json` gives the same as JSON, with the terminal's raw
  answers.
- emde caches the terminal's answers for a session when asking was slow
  (over SSH); `--reprobe` asks again, for example after changing a setting.
- Wrong colours or a light theme on a dark background: set
  `theme.background` (`--background dark`), or `--color 256` for terminals
  that claim more colours than they have.
- Odd characters or misaligned tables: a font without the glyphs (see the
  iTerm2 tip), or a terminal that draws East Asian ambiguous characters
  wide (`render.ambiguous_width = 2`); `--ascii` uses ASCII decorations.
- `emde --check-config` explains problems in your config file; `-v` shows
  every problem in full, also problems with the document.
- Slow? `EMDE_TRACE=1 emde -p notes.md > /dev/null` prints how long each
  stage took.

## Man page and shell completions

`cargo xtask docs` writes a man page and completion scripts for bash, zsh
and fish into the target directory (`$CARGO_TARGET_DIR`, or `target/`):

```sh
cargo xtask docs
man "$CARGO_TARGET_DIR/man/emde.1"     # a path, so man reads that file
cp "$CARGO_TARGET_DIR/completions/emde.bash" ~/.local/share/bash-completion/completions/emde
cp "$CARGO_TARGET_DIR/completions/_emde" ~/.zfunc/        # a directory on your $fpath
cp "$CARGO_TARGET_DIR/completions/emde.fish" ~/.config/fish/completions/
```

The same command regenerates [CONFIG.md](CONFIG.md) and the key table above
from the sources (`assets/default.toml`, the theme elements, the key table
of the pager).

## Development

```sh
source scripts/env.sh
cargo test --workspace
cargo xtask ci          # everything CI checks: formatting, lints, tests,
                        # docs up to date, banned crates, licences, MSRV, binary size
cargo xtask docs        # regenerate CONFIG.md, the key table, man page, completions
cargo xtask fuzz        # run each fuzz target for 60 s (--secs N, or name targets)
```

`cargo xtask fuzz` needs the nightly toolchain and cargo-fuzz
(`rustup toolchain install nightly --profile minimal`,
`cargo install --locked cargo-fuzz`). The targets in `fuzz/` cover the whole
rendering pipeline, the math typesetter, the terminal reply parser, the HTML
lexer and the configuration loader; [fuzz/README.md](fuzz/README.md) says
what each one checks.

## Credits and licence

emde bundles syntax definitions and code themes from
[two-face](https://github.com/CosmicHorrorDev/two-face) (bat's collection),
used with [syntect](https://github.com/trishume/syntect). They come under
MIT, BSD and Apache licences whose notices `emde --credits` prints. The
default theme uses the [Catppuccin](https://github.com/catppuccin/palette)
palettes (MIT).

emde's own licence has not been chosen yet. Every dependency is under a
permissive licence (checked by `cargo deny`), so any choice remains open.
