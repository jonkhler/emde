//! Resolved rendering options.
//!
//! This is the contract between configuration (`config/`, which fills these
//! from `default.toml`, the user file, `--set` and CLI flags) and the core
//! (`layout/`, `render/`, `pager/`). `Default` matches `assets/default.toml`.

/// `auto | always | never` switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum When {
    #[default]
    Auto,
    Always,
    Never,
}

/// Horizontal placement of the text column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    /// Centred when the terminal is wider than the measure (TTY only).
    #[default]
    Center,
    Left,
}

/// How an `h1` is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum H1Style {
    /// Full-measure background bar (gradient in truecolor).
    #[default]
    Bar,
    /// Styled text with its underline attribute.
    Underline,
    /// Styled text only.
    Plain,
}

/// How an `h2` is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum H2Style {
    /// Heavy rule under the text fading into a light tail.
    #[default]
    Rule,
    Plain,
}

/// Table border glyph sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TableBorder {
    #[default]
    Rounded,
    Light,
    Heavy,
    Double,
    Ascii,
    None,
}

/// Icon glyph sets (alerts etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IconSet {
    #[default]
    Unicode,
    Nerd,
    Ascii,
}

/// Front matter rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FrontMatterMode {
    /// Key/value card for flat YAML/TOML, else a code block.
    #[default]
    Card,
    Code,
    Hide,
}

/// Raw HTML handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HtmlMode {
    /// Render the supported tag subset, drop other tags (keep their text).
    #[default]
    Subset,
    /// Drop all tags, keep text.
    Strip,
    /// Show tags as dim text.
    Raw,
}

/// Code block presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CodeStyle {
    /// Panel at ≥ 256 colours, frame below.
    #[default]
    Auto,
    Panel,
    Frame,
    Gutter,
}

/// Inline math rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InlineMath {
    #[default]
    Unicode,
    Ascii,
    Raw,
}

/// Display math rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DisplayMath {
    #[default]
    TwoD,
    Linear,
    Raw,
}

/// Requested image output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ImageMode {
    #[default]
    Auto,
    Kitty,
    Iterm,
    Sixel,
    Blocks,
    None,
}

/// Glyphs for text-mode images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlockGlyphs {
    #[default]
    Half,
    Quadrant,
    Sextant,
    Octant,
    /// Octant/sextant only on terminals known to draw them natively.
    Auto,
}

/// A height limit in rows or percent of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Height {
    Rows(u16),
    Percent(u8),
}

/// Heading presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadingOptions {
    pub h1: H1Style,
    pub h2: H2Style,
    /// Prefix per level (h1..h6), e.g. `"▎ "` for h3.
    pub markers: [String; 6],
    /// Prefix headings with `1.2.3` numbering.
    pub numbers: bool,
}

impl Default for HeadingOptions {
    fn default() -> Self {
        HeadingOptions {
            h1: H1Style::Bar,
            h2: H2Style::Rule,
            markers: [
                String::new(),
                String::new(),
                "▎ ".into(),
                String::new(),
                String::new(),
                String::new(),
            ],
            numbers: false,
        }
    }
}

/// Decoration glyphs (all non-emoji, no VS16; ASCII fallbacks when `ascii`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// Bullet per nesting depth (cycled).
    pub bullets: Vec<String>,
    /// `[open, done]` task markers.
    pub task: [String; 2],
    pub quote: String,
    pub rule: String,
    pub table: TableBorder,
    pub wrap_marker: String,
    pub icons: IconSet,
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs {
            bullets: ["•", "◦", "‣", "⁃"].map(String::from).to_vec(),
            task: ["☐".into(), "☒".into()],
            quote: "▎".into(),
            rule: "─".into(),
            table: TableBorder::Rounded,
            wrap_marker: "↳".into(),
            icons: IconSet::Unicode,
        }
    }
}

/// Code block options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeOptions {
    pub wrap: bool,
    pub line_numbers: bool,
    pub tab_width: u8,
    pub label: bool,
    /// Blocks larger than this are shown without highlighting.
    pub max_highlight_bytes: usize,
    pub style: CodeStyle,
    /// Extra fence-token aliases (`token → language`), applied before the built-ins.
    pub aliases: Vec<(String, String)>,
}

impl Default for CodeOptions {
    fn default() -> Self {
        CodeOptions {
            wrap: true,
            line_numbers: false,
            tab_width: 4,
            label: true,
            max_highlight_bytes: 512 * 1024,
            style: CodeStyle::Auto,
            aliases: Vec::new(),
        }
    }
}

/// Table options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableOptions {
    pub zebra: bool,
}

impl Default for TableOptions {
    fn default() -> Self {
        TableOptions { zebra: true }
    }
}

/// Math options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathMode {
    pub inline: InlineMath,
    pub display: DisplayMath,
    /// Recognise `\( … \)` and `\[ … \]` delimiters.
    pub tex_delimiters: bool,
    pub opts: emde_math::MathOptions,
}

impl Default for MathMode {
    fn default() -> Self {
        MathMode {
            inline: InlineMath::Unicode,
            display: DisplayMath::TwoD,
            tex_delimiters: true,
            opts: emde_math::MathOptions::default(),
        }
    }
}

/// Image options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageOptions {
    pub mode: ImageMode,
    pub blocks: BlockGlyphs,
    /// Maximum figure height (pager); stream mode caps at 30 rows.
    pub max_height: Height,
    /// Fetch remote images with `curl`.
    pub remote: bool,
    /// Use tmux passthrough only when the user enabled it (never change tmux options).
    pub tmux_passthrough: bool,
    /// Refuse to decode larger images (pixels).
    pub max_pixels: u64,
}

impl Default for ImageOptions {
    fn default() -> Self {
        ImageOptions {
            mode: ImageMode::Auto,
            blocks: BlockGlyphs::Half,
            max_height: Height::Percent(60),
            remote: false,
            tmux_passthrough: true,
            max_pixels: 40_000_000,
        }
    }
}

/// Markdown dialect switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkdownOptions {
    pub math: bool,
    pub linkify: bool,
    pub definition_lists: bool,
    pub smart_punctuation: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        MarkdownOptions {
            math: true,
            linkify: true,
            definition_lists: true,
            smart_punctuation: false,
        }
    }
}

/// Everything layout and rendering need besides the theme and terminal caps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderOptions {
    /// Forced total width (`--width`); `None` = terminal width, else 80.
    pub width: Option<u16>,
    /// Text column cap (0 = none).
    pub max_width: u16,
    pub margin: u16,
    pub align: Align,
    /// Numbered link references when OSC 8 is unavailable.
    pub link_refs: When,
    /// Plain ASCII decorations only.
    pub ascii: bool,
    /// Treat East Asian Ambiguous characters as double width.
    pub ambiguous_wide: bool,
    /// Gradients (h1 bar, rules) — `Auto` means truecolor only.
    pub gradients: When,
    pub front_matter: FrontMatterMode,
    pub html: HtmlMode,
    pub heading: HeadingOptions,
    pub glyphs: Glyphs,
    pub code: CodeOptions,
    pub tables: TableOptions,
    pub math: MathMode,
    pub images: ImageOptions,
    pub markdown: MarkdownOptions,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: None,
            max_width: 100,
            margin: 2,
            align: Align::Center,
            link_refs: When::Auto,
            ascii: false,
            ambiguous_wide: false,
            gradients: When::Auto,
            front_matter: FrontMatterMode::Card,
            html: HtmlMode::Subset,
            heading: HeadingOptions::default(),
            glyphs: Glyphs::default(),
            code: CodeOptions::default(),
            tables: TableOptions::default(),
            math: MathMode::default(),
            images: ImageOptions::default(),
            markdown: MarkdownOptions::default(),
        }
    }
}
