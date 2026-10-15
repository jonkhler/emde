# emde: user acceptance checklist

Use this list in each real terminal. Automated tests cover the logic, but
only a real terminal shows glyphs, colours, pixels and clicks.

**Setup.** Install the binary (`install.sh`, or `cargo build --release`) and
work from the repository, so the fixtures are at hand:

| File | Use |
|---|---|
| `tests/fixtures/md/kitchen-sink.md` | every Markdown element |
| `tests/fixtures/md/math.md` | inline and 2D display math |
| `tests/fixtures/md/tables.md` · `cjk-emoji.md` | wide tables, East Asian text, emoji |
| `tests/fixtures/md/html.md` | badges, centred blocks, `<details>`, `<picture>` |
| `tests/fixtures/images/gallery.md` | PNG, JPEG, GIF, WebP, a broken file, SVG |
| any long README with code | scrolling, search, horizontal scroll |

Note what fails, with the output of `emde --doctor=json`.

## 1. In every terminal

| # | Try | Expected |
|---|---|---|
| 1 | `emde --doctor` | One line per decision with its reason. Colour, links, underline and images match the terminal's row in section 2. |
| 2 | `emde tests/fixtures/md/kitchen-sink.md` | The pager opens on the alternate screen. The h1 is a full-width bar fading blue to mauve. The h2 has a heavy rule fading out. Code is highlighted on a panel with a right-aligned language label. Tables have rounded borders. Alerts have a coloured bar, an icon and a tint. Task boxes show `☐`/`☒`. |
| 3 | Same file: hover over and ⌘/Ctrl-click a link | Links are underlined and open in the browser (OSC 8). Over SSH, `y` copies the URL instead. |
| 4 | `emde tests/fixtures/md/math.md` | Inline math is italic Unicode (`α² + a/b`). Display math is stacked and centred: fractions on a bar, `∑` with limits above and below, `⎛a  b⎞`/`⎝c  d⎠` matrices, a `⎧⎨⎩` cases brace, `⌠⎮⌡` integrals. Glyph pieces join without gaps (iTerm2: use a font with them, such as JuliaMono). |
| 5 | `emde tests/fixtures/md/cjk-emoji.md`, then `tables.md` | CJK and emoji line up in tables, and table borders are straight. |
| 6 | Scroll: `j` `k` `Space` `b` `d` `u` `gg` `G` `50%`, the mouse wheel | Smooth, with no stale rows. The status bar shows the file, the section path and `L n/N  p%`. |
| 7 | `/` a phrase that wraps over two lines, then `n` `N` `?` | Both halves are highlighted, and the current match differently. A match inside a collapsed `<details>` opens it. |
| 8 | `]` `[` `}` `{`, `t` (type to filter, `Enter`), `T` `T` `T` | Heading jumps work. The outline overlay filters. The sidebar appears on the left, takes the keys, then hides. |
| 9 | `f` (follow), `o` (links), `y` (copy), `v` (select) | Labels appear on the targets. Typing one acts on it. Labels over CJK cover the whole character. |
| 10 | `V` `5j` `y`, then `Y`; `ma`, scroll, `'a`, `''` | `y` copies the Markdown source of the selection, `Y` the text as shown. Paste both to check. Marks return to the place. |
| 11 | `Tab` to a link to another `.md` file or `#anchor`, `Enter`, `Backspace`, `L` | The link opens in the pager. Back and forward restore the place and the focus. |
| 12 | `emde tests/fixtures/images/gallery.md`, `gi`, `n`, `p`, `q` | Figures keep their size while loading. The image view pages through the images. A broken image shows its alt text and a reason. |
| 13 | Edit the open file in another window and save; `r`; `R` | The view updates in place within about half a second (atomic-rename saves too). `R` toggles watching. |
| 14 | Resize the window, narrower and wider | The text re-wraps, and the top line stays the same paragraph. Images are redrawn at the new size. |
| 15 | `emde --no-wrap-code` on a README with long code, and `tables.md`; `zl` `zL` `zh` `$` `^`, sideways wheel | Only wide code and tables move, with `‹`/`›` at the cut edges. Text stays put. |
| 16 | `html.md`: `Tab` to a `▸ summary`, `Enter`; `za` | The `<details>` block opens and closes, and its line count shows when it is closed. |
| 17 | `emde a.md b.md`, `:n`, `:p` | Moves between the files, back at the place last seen. |
| 18 | `e` (with `$VISUAL` or `$EDITOR` set) | Your editor opens at the block shown. On exit the pager comes back and reloads. |
| 19 | Quit with `q`, `^C`, `^Z` then `fg`, and `kill -TERM <pid>` from another shell | The prompt is clean. The cursor is visible. No mouse reports (`^[[<…`) appear when clicking. `stty -a` shows `icanon echo`. After `^Z`/`fg` the pager redraws. |
| 20 | `emde -p --color=always kitchen-sink.md \| less -R`, `NO_COLOR=1 emde -p …`, `TERM=dumb emde …`, `emde -p … \| cat` | Colours survive `less -R`, and no line bleeds into the next. `NO_COLOR` gives attributes only. Piped output has no escapes. |
| 21 | `emde --background light …` (or a light terminal profile) | Every colour is readable on the light page, yellow and peach included. |
| 22 | `emde -w 20 kitchen-sink.md`, and a 10 MB file | Nothing is wider than 20 columns. A large file opens without a stall. |

## 2. Per terminal

| Terminal | `--doctor` should say | Images (`gallery.md`) | Extra checks |
|---|---|---|---|
| **iTerm2 over SSH** | colour truecolor (`LC_TERMINAL`), OSC 8 ✓, curly ✓, ssh | iTerm2 3.6 or newer: kitty Unicode placeholders that scroll with the text. Older: iTerm2 inline images, shown 120 ms after scrolling stops. | The first run probes over the network; a second run starts at once (probe cache). `--reprobe` asks again. `y` copies through OSC 52 (allow clipboard access in iTerm2's settings). |
| **VS Code / code-server terminal** | truecolor (`TERM_PROGRAM=vscode`), OSC 8 ✓, curly ✓ | With `"terminal.integrated.enableImages": true`: kitty graphics that move when scrolling and disappear after `q`. Without it: block characters. | Ctrl/⌘-click opens links. Resize the panel: no images are left behind. |
| **tmux, `allow-passthrough off`** | truecolor (in tmux), OSC 8 ✓ (tmux 3.4 or newer), blocks, the warning that passthrough is off and the tip `set -g allow-passthrough on` | Block characters (half blocks), which scroll as text. | No stray `ESC _G` or OSC 1337 bytes appear. `y` copies through the tmux buffer when `set-clipboard` is `external`. Detach and reattach: the screen is redrawn correctly. |
| **tmux attached over mosh** (`mosh-server -c 256`) | colour 256 colours (tmux client without RGB), OSC 8 ✗, curly ✗, blocks, and `warn` lines for colour, images and links (likely mosh) | Block characters, dithered in 256 colours: smooth shading, no bands; the pattern stays put while scrolling. The pager says `images: 256 colours (tmux client without RGB) — see emde --doctor` the first time an image shows. | `images.dither = false` gives the plain nearest colours. Attaching the same session over SSH from iTerm2 instead gives truecolor again (on the next run). |
| **tmux, `allow-passthrough on`** (in iTerm2) | kitty placeholders via the outer terminal | Real pixels that scroll and hide with the text. Splitting the window and resizing the pane redraws them. No images are left after `q`. | With VS Code outside instead of iTerm2: blocks (expected). |
| **kitty (local)** | truecolor, OSC 8, curly, kitty placeholders | Real pixels. `--images blocks --blocks octant` gives sharp octants (kitty 0.40 or newer). | `kitty +kitten icat --clear` should find nothing left after `q`. |
| **Ghostty (local)** | as kitty | Placeholders; octant blocks. | Same as kitty. |
| **WezTerm (local)** | truecolor, OSC 8, curly, iTerm2 inline images | Inline images, redrawn after scrolling. `--images blocks --blocks sextant` for blocks. | Ctrl-click opens links. |
| **iTerm2 (local)** | truecolor, OSC 8, curly, kitty placeholders (3.6 or newer) | Real pixels. | As iTerm2 over SSH, but without the probe delay. |

**Sign-off.** Record the terminal, its version, tmux's version, the date,
and which rows failed. Attach screenshots for rows 2, 4, 12 and 21.
