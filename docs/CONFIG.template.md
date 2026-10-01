# Configuring emde

emde needs no configuration. What you want to change goes in one TOML file,
and whatever you leave out keeps its default.

- [The config file](#the-config-file)
- [Settings](#settings)
- [Colours](#colours)
- [Styles](#styles)
- [Themes](#themes)
- [Key bindings](#key-bindings)

## The config file

emde reads one config file, the first of:

1. the file named by `--config PATH`;
2. the file named by `$EMDE_CONFIG`;
3. `$XDG_CONFIG_HOME/emde/config.toml`, when `XDG_CONFIG_HOME` is an
   absolute path and the file exists;
4. `~/.config/emde/config.toml`, when it exists.

The paths are the same on Linux and macOS. `--no-config` reads no file.
Config and theme files can be up to 1 MiB.

`emde --print-default-config` prints every setting with its default and an
explanation, which makes a good start for your own file. `emde
--check-config` checks your file: unknown keys (with a suggestion when a key
looks misspelt), values of the wrong type or out of range, colours that do
not exist, and unknown code themes and languages. It exits with status 1
when it finds a problem.

A mistake never stops emde from showing a document. It says what is wrong
on standard error (in full with `-v`) and goes on without it: a number out
of range is ignored, a value of the wrong kind (a word that is not one of
the choices, text where a number goes) drops the whole table it is in, such
as `[render]`, and a file that is not valid TOML is ignored altogether.

### Layers

Settings are applied in layers, each overriding the ones before it:

1. emde's defaults (below);
2. the theme, and the themes it inherits from (colours and styles only);
3. your config file;
4. each `--set KEY=VALUE`, in order;
5. options such as `--theme` and `--width`.

`--set` takes a key and a value as one line of TOML, such as `--set
render.max_width=80`. A value that is not valid TOML is taken as a string,
so `--set theme.code=Nord` and `--set "pager.open=open -a Safari"` need no
quotes. Any key works, also in tables: `--set style.h1.fg=red`.

Your `[palette]` entries are merged with the theme's, name by name. A
`[style.ELEMENT]` table replaces the theme's style for that element: the
fields you leave out come from the element's parent, not from the theme's
style for the element.

## Settings

Every key, with its default and the option that also sets it, if any.

{{settings}}

## Colours

Wherever a colour goes (`fg`, `bg`, `bg_to`, `underline_color`, and the
palette), you can write:

| Syntax | Example | Meaning |
|---|---|---|
| `"#rrggbb"`, `"#rgb"` | `"#89b4fa"`, `"#8bf"` | A 24-bit colour. |
| a palette name | `"accent"` | A colour of the theme's palette or of your `[palette]`. |
| an ANSI name | `"red"`, `"bright-black"`, `"grey"` | One of the terminal's 16 colours: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan` and `white`, also as `bright-…`. |
| a number | `208` | `0`–`15` are the terminal's 16 colours, `16`–`255` the xterm palette. |
| `"default"` | | The terminal's own foreground or background. |
| `"base"` | | The page background: the terminal's own when emde can detect it, else the palette's `base`. |
| `"surface"` | | A panel colour derived from `base` (code blocks, table headers, keys). |
| a tint | `"yellow/30%"`, `"#313244/50%"` | A colour mixed over the page background. |

Names are looked up in this order: `base`, `surface`, the palette, the ANSI
names. A palette entry may refer to another one.

On terminals with fewer colours, 24-bit colours become the nearest colour
of the 256-colour palette (nearest as the eye sees it, in OKLab). On
16-colour terminals emde uses the `ansi` theme, which only uses the
terminal's own colours, unless you chose a theme yourself.

```toml
[palette]
accent = "#ff8800"
link = "accent"

[style.link]
fg = "link"

[style.mark]
bg = "yellow/30%"

[style.h4]
fg = 208
```

## Styles

`[style.ELEMENT]` restyles one element (see the list below), with these
fields:

| Field | Value |
|---|---|
| `fg`, `bg` | The text and background colours. |
| `bg_to` | The end colour of a background gradient (the h1 bar; 24-bit colour only). |
| `underline` | `"none"`, `"single"`, `"double"`, `"curly"`, `"dotted"` or `"dashed"` (`true` is single, `false` none). Where the terminal has only one kind of underline, every kind is single. |
| `underline_color` | The colour of the underline. |
| `bold`, `italic`, `dim`, `strikethrough`, `reverse`, `overline` | `true` or `false`. |

A field you leave out comes from the element's parent: `h1` inherits from
`heading`, and `heading` from `text`, so `[style.heading]` changes every
level of heading that does not say otherwise. `[dark.style.ELEMENT]` and
`[light.style.ELEMENT]` apply to one variant of the theme, on top of
`[style.ELEMENT]`. Body text keeps the terminal's own colour unless you give
`text` one.

```toml
# Level-1 headings as accent text with a curly underline, not a bar.
[style.h1]
fg = "accent"
bold = true
underline = "curly"

# Quotes in italics, darker on light backgrounds.
[style.quote]
italic = true

[light.style.quote]
fg = "#5c5f77"
```

### Elements

Every element, the element it inherits from, and how the default theme
draws it (`—`: as its parent).

{{elements}}

## Themes

`--theme NAME`, or `name` in `[theme]`, picks a theme: a built-in one, a
file `NAME.toml` in a themes directory (`$XDG_CONFIG_HOME/emde/themes/` when
`XDG_CONFIG_HOME` is set, then `~/.config/emde/themes/`), or the path of a
theme file. `emde --list-themes` lists them. Unless you choose a theme, emde
uses `ansi` on 16-colour terminals and `mono` when colours are off
(`NO_COLOR`).

{{themes}}

Each theme has a dark and a light variant. `background` in `[theme]`
chooses one; `auto` asks the terminal for its background colour (or reads
`COLORFGBG`) and uses dark when it cannot tell.

### The palette

The colours of the default theme, by name, in its two variants. All the
built-in themes have these names (in `ansi` they are ANSI colours, in
`mono` the terminal's own), so styles that use them work with every theme.

{{palette}}

### Theme files

A theme file has the `[palette]` and `[style.ELEMENT]` tables of the config
file, and these keys:

| Key | |
|---|---|
| `name` | A display name; the file name when left out. |
| `inherits` | A theme to start from: a built-in theme, a theme in the themes directory, or a path (relative to this file). Up to 8 themes may inherit from each other, without loops. |
| `code` | The code theme: one name, or `{ dark = "…", light = "…" }`; `"auto"` keeps the inherited theme's choice. |
| `[palette.dark]`, `[palette.light]` | Palette entries for one variant. |
| `[dark.style.ELEMENT]`, `[light.style.ELEMENT]` | Styles for one variant. |

A theme that inherits from another merges its palette into the other's,
name by name, and each of its `[style.ELEMENT]` tables replaces the other's
style for that element.

```toml theme
name = "dusk"
inherits = "emde"
code = { dark = "Nord", light = "GitHub" }

[palette.dark]
accent = "#88c0d0"

[palette.light]
accent = "#5e81ac"

[style.h2]
fg = "accent"
bold = true

[dark.style.table_zebra]
bg = "accent/10%"
```

Saved as `~/.config/emde/themes/dusk.toml`, it is used by `emde --theme
dusk`, or by `name = "dusk"` in `[theme]`.

## Key bindings

The pager's keys (they cannot be changed yet).

{{keys}}
