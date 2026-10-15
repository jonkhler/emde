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
  fit (in the pager, the widest scroll sideways; elsewhere they turn into
  cards), and long lines wrap instead of being cut off.
  Every line ends with a reset, so styles never bleed, and a document can
  never send escape sequences of its own to the terminal.
- **A built-in pager.** On a terminal, a document opens in the pager:
  incremental search (smart case, and a phrase is found even where it
  wraps), an outline and an outline sidebar that follows your reading, an
  image view that steps through a document's pictures, `<details>` that
  open and close, wide tables and code that scroll sideways, jumps between
  headings, marks, Vimium-style hints to
  follow links or copy code, math and tables as written, a Visual mode
  that copies lines as Markdown or text, `e` to edit the file at the line
  you are reading, back and forward through linked documents, reload when
  the file changes, the mouse wheel, and re-wrapping on resize that keeps
  your place. Pipes and `--plain` get the rendered text, like `bat` does
  (and with `pager.enabled = "auto"`, documents that fit on the screen).
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
  fetched when you ask (`--remote-images`). SVG images need the optional
  `svg` feature ([building](#building)).
- **Themes.** The default theme has Catppuccin colours for dark and light
  backgrounds and picks the variant from the terminal's background colour.
  `dracula`, `gruvbox`, `nord`, `solarized` and `tokyonight` bring those
  colour schemes (with readable contrast), `ansi` follows your terminal's
  colour scheme and `mono` uses text attributes only. Every element can be restyled in a TOML file, and
  24-bit colours are converted for 256- and 16-colour terminals.
- **Clickable links** (OSC 8) where the terminal supports them, numbered
  references (`docs[1]`) where it does not.

## Installing

The installer picks the release archive for your system (Linux or macOS,
x86-64 or ARM), checks its SHA-256, and puts `emde` in `~/.local/bin`, the
man page in `~/.local/share/man` and shell completions in `~/.local/share`:

```sh
curl -fsSL https://raw.githubusercontent.com/jonkhler/emde/main/install.sh | sh
```

The same command updates emde. Variants:

| Command | What it does |
|---|---|
| `… \| EMDE_VERSION=v0.1.0 sh` | install that release instead of the latest |
| `… \| EMDE_INSTALL_DIR=/usr/local/bin sh` | install the binary elsewhere |
| `… \| sh -s -- --from-source` | build with `cargo install` (any platform with Rust) |
| `… \| sh -s -- --uninstall` | remove everything the installer put in place |
| `./install.sh --local` | in a checkout: build it and install that build |
| `… \| EMDE_REPO=you/emde sh` | install from a fork (a private one through `gh auth login` or `GITHUB_TOKEN`) |

Releases are built by GitHub Actions when a `v*` tag is pushed
(`.github/workflows/release.yml`), using `cargo xtask dist`, which you can
also run yourself to get the archive for your machine in
`$CARGO_TARGET_DIR/dist/`.

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
| `svg` | no | SVG images (resvg; off by default because of its size, see below). |
| `simd` | yes | Faster Markdown parsing with SSSE3 (detected at run time; x86-64 only). |

`cargo build --release --no-default-features --features highlight,fancy,tmtheme,images,sixel,simd`
builds without Oniguruma: then emde has no C code at all.

**SVG images** are an optional feature:

```sh
cargo build --release --features svg
```

Without it, an SVG figure shows its alt text. With it, SVG files, `data:`
URIs and remote SVGs (recognised by their content, so URLs without `.svg`
work too) are drawn with [resvg](https://github.com/linebender/resvg) at
the exact pixel size of the figure's box, through every graphics path
(blocks, kitty, iTerm2, sixel). The feature makes the stripped release
binary about 2.6 MiB larger (x86-64 Linux: 6.41 MiB without it, 8.97 MiB
with it). An SVG never makes emde read another file or fetch a URL;
system fonts are loaded once, only for SVGs with text. Badges such as
shields.io's are remote SVGs, so they also need `--remote-images` (or
`images.remote = true`).

## Usage

```sh
emde README.md                       # read a document in the pager
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
| `--paging auto\|always\|never` | When to use the pager on a terminal (default `always`; `auto`: for long documents only). |
| `-w`, `--width N` · `-m`, `--max-width N` | The width, and the widest text column (default 100). |
| `--color auto\|always\|never\|truecolor\|256\|16` | Colours, also when piping. |
| `-t`, `--theme NAME\|PATH` · `--background auto\|dark\|light` | The theme and its variant. |
| `--code-theme NAME\|PATH` | The code theme (`--list-code-themes`; languages: `--list-languages`). |
| `--images auto\|kitty\|iterm\|sixel\|blocks\|none` | How to show images. |
| `--toc` · `--anchor SLUG` | Open the pager with the outline shown, or at a heading. |
| `-s`, `--set KEY=VALUE` | Change any setting for this run (see [Configuration](#configuration)). |
| `--doctor[=json]` | Show every terminal decision and why it was made. |
| `--print-default-config` · `--check-config` | The settings with their defaults; check your config file. |

## The pager

Keys work like in `less` and `vim`; `h` shows them all. A count before a
command repeats it or gives it a line or a percentage: `5j`, `120gg`, `50%`.
Links can be followed into other Markdown files, and `Backspace` comes back.

Hints put a short label on everything worth acting on, as Vimium does in a
browser: type it. `f` follows a link, goes to a heading, a code block, a
table or a footnote (and back), or shows an image in the image view; `o`
labels links only. `y` copies what the label is on as written in the file: the
code of a code block (no fences), the TeX of math, a table, a paragraph, a
list item or a quote as Markdown, a link's URL, `file.md#slug` for a
heading. `V` selects lines (`v` and a label: a block), and in Visual mode
`y` copies the Markdown of the selected blocks, `Y` their text as shown.
`e` opens the file in `$VISUAL` or `$EDITOR` at the line you are reading,
and shows it again when the editor exits. `ma` marks a place, `'a` goes
back to it, and `''` to where you were before the last jump.

The **image view** shows one image as large as the screen allows. Links to
images open it (local files, `data:` images, and remote ones with
`--remote-images`), as do `f` on a figure, a click on one, and `gi` (the
first image of the document). `n` and `p` step through every image of the
document, figures and links alike; `y` copies the image's path, `o` opens
it in your image viewer, and `Esc` brings you back to where you were.

The **outline sidebar** lists the headings on the left and marks the
section you are reading. It shows on its own on terminals wide enough for it
beside the text (`[pager] toc_sidebar`, `toc_width`); `T` shows it, a
second `T` moves the keys into it (`j`, `k`, `Enter`, `Esc`), a third hides
it, and a click on an entry goes there.

**`<details>`** blocks start collapsed to their summary line, which says
how much they hide: `▸ Installation (42 lines)` (`[pager] details =
"expanded"` opens them all; `<details open>` starts open). `Enter` opens or
closes a summary that `Tab` focused, as does `f` with its label, a click on
it, or `za` (the one at the top of the screen). A search finds text inside
collapsed blocks and opens the one with the match it goes to; the outline
and the sidebar list headings inside collapsed blocks dimmed, and going to
one opens its block. Output that is not paged shows every block open.

**Wide tables and code** keep their natural width in the pager: a table
too wide for its columns (instead of becoming one card per row; `[tables]
overflow = "records"` keeps the cards), and code when wrapping is off
(`--no-wrap-code`, `[code] wrap = false`). `zl` and `zh` (or `→` and `←`,
or the wheel sideways) shift every such block a few columns, `zL` and `zH`
half a screen, `$` and `^` to their end and start; ordinary text stays
where it is. `‹` and `›` mark the edges where a block goes on, and a search
match or a focused link in one is shifted into view.

<!-- BEGIN GENERATED keys (`cargo xtask docs`, from src/pager/keymap.rs) -->

**Scrolling**

| Keys | Action |
|---|---|
| `0`–`9` | count for the next command (5j, 50%, 120gg) |
| `j` `↓` `^E` `^N` | down a line |
| `k` `↑` `^Y` `^P` | up a line |
| `Space` `PgDn` `^F` | down a page |
| `b` `PgUp` `^B` | up a page |
| `d` `^D` | down half a page |
| `u` `^U` | up half a page |
| `gg` `g` `Home` `<` | top (line N with a count) |
| `G` `End` `>` | bottom (line N with a count) |
| `%` | N percent into the document |
| `zz` | centre the current match, link or middle line |
| `zt` | … to the top |
| `zb` | … to the bottom |
| `zh` `←` | shift wide code and tables left |
| `zl` `→` | … right |
| `zH` | … half a screen left |
| `zL` | … half a screen right |
| `^` | … back to their start |
| `$` | … to their end |

**Jumping**

| Keys | Action |
|---|---|
| `]` | next heading |
| `[` | previous heading |
| `}` | next heading of the same or a higher level |
| `{` | previous heading of the same or a higher level |
| `t` | outline (type to filter) |
| `T` | outline sidebar: show it; again: keys in it; again: hide it |
| `m` | mark the place: m and a letter (ma) |
| `'` | go to a mark ('a); '' back to before the last jump |

**Searching**

| Keys | Action |
|---|---|
| `/` | search forward |
| `?` | search backward |
| `n` | next match |
| `N` | previous match |
| `Esc` | clear the search, the link focus |

**Links**

| Keys | Action |
|---|---|
| `Tab` | focus the next link or details summary |
| `S-Tab` | focus the previous one |
| `Enter` | follow the focused link, open or close a focused details block |
| `za` | open or close the details block at the top (or click its summary) |
| `o` | link hints: type a label to follow |
| `Backspace` `H` | back |
| `L` | forward |

**Hints, copying and editing**

| Keys | Action |
|---|---|
| `f` | hints: follow a link, go to a heading or block, view an image |
| `y` | copy the focused link's URL, else hints: copy code, math, a table… |
| `v` | hints: select a block (Visual mode) |
| `V` | Visual mode: select lines |
| `e` | edit the file ($VISUAL, $EDITOR) at the top block |

**Other**

| Keys | Action |
|---|---|
| `r` | reload the file |
| `R` | watch the file for changes, on or off |
| `i` | cycle the image mode |
| `gi` | image view: the document's images, from the first |
| `w` | full width, on or off |
| `M` | mouse, on or off (off: select text) |
| `:` | command: :n next file, :p previous file, :q quit |
| `h` `F1` | this help |
| `^L` | redraw the screen (anywhere) |
| `^Z` | suspend (anywhere) |
| `q` `^C` | quit |

**In Visual mode**

| Keys | Action |
|---|---|
| `0`–`9` | count for the next motion |
| `j` `↓` `^N` | extend down a line |
| `k` `↑` `^P` | extend up a line |
| `^D` `PgDn` | down half a page |
| `^U` `PgUp` | up half a page |
| `}` | to the end of the block, or of the next |
| `{` | to the start of the block, or of the previous |
| `]` | to the next heading |
| `[` | to the previous heading |
| `gg` `g` `Home` | to the top |
| `G` `End` | to the bottom |
| `n` | to the next match |
| `N` | to the previous match |
| `zz` | the cursor line to the middle |
| `zt` | … to the top |
| `zb` | … to the bottom |
| `o` | go to the other end |
| `y` | copy the Markdown of the selected blocks |
| `Y` | copy the text of the selected lines |
| `e` | edit the file at the selection |
| `Esc` `V` `q` `^C` | leave Visual mode |

**In the search prompt**

| Keys | Action |
|---|---|
| `Enter` | search |
| `Esc` `^C` | cancel |
| `Backspace` | delete a character |
| `^U` | clear the pattern |

**At the : prompt**

| Keys | Action |
|---|---|
| `Enter` | run the command |
| `Esc` `^C` | cancel |
| `Backspace` | delete a character |
| `^U` | clear the command |

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

**In the outline sidebar**

| Keys | Action |
|---|---|
| `j` `↓` `^N` | next entry |
| `k` `↑` `^P` | previous entry |
| `Space` `PgDn` `^F` | down a page |
| `b` `PgUp` `^B` | up a page |
| `g` `Home` | first entry |
| `G` `End` | last entry |
| `Enter` | go to the heading |
| `Esc` `q` `^C` | back to the document |
| `T` | hide the sidebar |

**In the image view**

| Keys | Action |
|---|---|
| `n` `j` `→` `Space` `PgDn` | next image |
| `p` `k` `←` `Backspace` `PgUp` | previous image |
| `g` `Home` | first image |
| `G` `End` | last image |
| `y` | copy the image's path or URL |
| `o` | open the image with the system's opener |
| `i` | cycle the image mode |
| `Esc` `q` `^C` | back to the text, where it was |

**With hints**

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

In the search prompt, the outline and hints, other keys type text. The keys
can be changed in `[pager.keys]` ([key bindings](CONFIG.md#key-bindings)).

## Configuration

emde reads `~/.config/emde/config.toml` (the same on Linux and macOS), or
`$XDG_CONFIG_HOME/emde/config.toml` when `XDG_CONFIG_HOME` is set and that
file exists. `--config PATH` or `$EMDE_CONFIG` name another file, and
`--no-config` reads none. Themes go in `~/.config/emde/themes/NAME.toml`.

The built-in themes are `emde` (the default), `dracula`, `gruvbox`, `nord`,
`solarized`, `tokyonight`, `ansi` and `mono`; each has a dark and a light
variant, chosen from the terminal's background colour. `emde --list-themes`
lists them with your own:

```sh
emde --theme gruvbox notes.md                  # one run
emde --theme nord --background light notes.md  # and the light variant
```

or, in the config file:

```toml
[theme]
name = "solarized"
```

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
- Inside tmux `COLORTERM` is often unset: SSH does not forward it, and tmux
  does not copy it from the terminal you attach from. With
  `TERM=tmux-256color`, programs then think they have 256 colours. emde
  knows that tmux converts colours for each client and uses 24-bit colour
  inside tmux anyway. For documents with images it also asks tmux what the
  attached terminal can show: when tmux reports no 24-bit colour for it,
  images are drawn in the 256 (or 16) colours it has, dithered
  (`images.dither`). If colours
  look wrong in tmux, tell tmux your terminal has 24-bit colour:
  `set -as terminal-features ',xterm-256color:RGB'`.
- Copying (`y` in the pager) uses OSC 52. With tmux's default
  `set-clipboard external`, tmux ignores OSC 52 from programs, so emde puts
  the URL into a tmux buffer with `tmux load-buffer -w`, which also passes
  it on to your terminal's clipboard. `set -g set-clipboard on` lets OSC 52
  through directly. Either way the outer terminal has to allow clipboard
  access (in iTerm2: Settings → General → Selection → *Applications in
  terminal may access clipboard*).

**mosh.** mosh carries text only: images over mosh are block characters,
never pixels, and their colours depend on how many mosh passes on
(`mosh-server -c 256` gives 256, which emde dithers). Inside tmux, the
client then reports no 24-bit colour and no links, and `emde --doctor`
says so. Use SSH for pixels and full colour.

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

<!-- BEGIN GENERATED doctor (`cargo xtask docs`, from the snapshot the tests of src/term/doctor.rs keep of this session) -->

```text
 terminal  tmux 3.4 → iTerm2 3.6.9, 214×54, SSH   colour truecolor (in tmux)   background dark (tmux OSC 11)
 links     OSC 8 ✓   underline curly ✓   images blocks(half) — kitty/iterm ✗ passthrough off · sixel ✗ client cell 0x0
 probe     tmux answered in 2 ms   cell size unknown (client cell 0x0)
 warn      images are block characters: tmux's allow-passthrough is off
 tip       `set -g allow-passthrough on` → kitty Unicode placeholders via iTerm2: real pixels that scroll with the text
```

<!-- END GENERATED doctor -->

- `emde --doctor=json` gives the same as JSON, with the terminal's raw
  answers.
- emde caches the terminal's answers for a session when asking was slow
  (over SSH); `--reprobe` asks again, for example after changing a setting.
- Images look posterised, or `warn` says tmux's client has no 24-bit
  colour: likely mosh (see the mosh tip); with a terminal that has 24-bit
  colour, tell tmux so with `terminal-features`.
- Wrong colours or a light theme on a dark background: set
  `theme.background` (`--background dark`), or `--color 256` for terminals
  that claim more colours than they have.
- Odd characters or misaligned tables: a font without the glyphs (see the
  iTerm2 tip), or a terminal that draws East Asian ambiguous characters
  wide (`render.ambiguous_width = 2`); `--ascii` uses ASCII decorations
  (math stays Unicode; `--math raw` shows its TeX).
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

## Performance

Measured on the development machine (x86-64 Linux, 24 cores, glibc 2.39)
with the stripped release build and [hyperfine](https://github.com/sharkdp/hyperfine)
(`-N`, 300 runs or more, medians), writing to a pipe with
`-p --color=always -w 100`. glow 3.0.0 ran with `-s dark -w 100`.

| README | emde | glow |
|---|---|---|
| [ripgrep](https://github.com/BurntSushi/ripgrep) (21 KB, plain code blocks) | 3.4 ms | 233 ms |
| [bat](https://github.com/sharkdp/bat) (34 KB, Bash) | 12.0 ms | 141 ms |
| [tokio](https://github.com/tokio-rs/tokio) (9.5 KB, Rust, TOML) | 11.1 ms | 63 ms |
| [serde](https://github.com/serde-rs/serde) (4.4 KB, Rust, TOML) | 11.0 ms | 38 ms |

| | |
|---|---|
| `emde --version` | 1.19 ms (a Rust hello world built the same way: 1.01 ms; `/bin/true`: 0.72 ms) |
| A 20 KB README without code | 2.98 ms (95th percentile 3.18 ms) |
| The same with 10 code blocks in Rust, TOML and Bash | 14.8 ms |
| 10 MB of prose with links and emphasis, to a pipe | 0.43 s (24 MB/s) |
| A 10 MB document of prose, lists, tables and code, to a pipe | 0.68 s (15 MB/s) |
| The throughput test (10 MB of generated prose, in process, truecolor) | 51 MB/s |
| Peak memory: the 20 KB README / the 10 MB prose | 7.0 MiB / 88 MiB |
| Pager, first frame: the README / a 1 MB document / a 10 MB one | 5.4 ms / 29 ms / 252 ms |
| Pager, resizing or opening a `<details>` near the top of the 10 MB document | 7 ms / 5 ms (plus the 30 ms resize debounce; a key waits for the layout slice under way) |
| Stripped binary, default features | 6.41 MiB |

Where the time goes:

* **Code.** A document with code loads the syntax set (about 3 ms, on a
  background thread from the moment the file is read), and the first block
  in each language compiles its grammar's regular expressions: about 4 ms
  for Rust and 3 ms for Bash. Oniguruma compiles one expression at a time
  for the whole process, so languages cannot be compiled side by side;
  that is most of the 15 ms of a README with code in three languages. The
  blocks are highlighted on background threads while the rest is laid out.
* **Long documents in the pager.** Documents of 256 KiB and more are laid
  out a few blocks at a time: the first three screens before the first
  frame, the rest in slices of at most 4 ms while the reader reads (the
  status bar shows `L 1/…` until the total is known). `G`, `%`, a search,
  the outline and following links finish the layout first. Reading and
  parsing take most of the first frame of a 10 MB document.

To see where a run spends its time, and to measure:

```sh
EMDE_TRACE=1 emde -p README.md > /dev/null     # milliseconds per stage
hyperfine -N --warmup 30 --runs 300 'emde -p --color=always -w 100 README.md'
cargo test --release --test throughput -- --ignored --nocapture
cargo xtask size                               # the stripped release binary
```

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
palettes (MIT). The other built-in themes use the palettes of
[Dracula](https://draculatheme.com) (MIT),
[gruvbox](https://github.com/morhetz/gruvbox) (MIT),
[Nord](https://github.com/nordtheme/nord) (MIT),
[Solarized](https://github.com/altercation/solarized) (MIT) and
[Tokyo Night](https://github.com/folke/tokyonight.nvim) (Apache-2.0).

emde is released under the [MIT licence](LICENSE). Every dependency is
under a permissive licence, checked by `cargo deny`.
