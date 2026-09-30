//! Styleable document and UI elements, with their fallback (parent) chain.
//!
//! Config keys use the snake_case [`Element::name`] (`[style.code_inline]`).
//! An element whose style is not set inherits from its parent, ending at
//! [`Element::Text`].

macro_rules! elements {
    ($( $(#[$doc:meta])* $variant:ident = $name:literal => $parent:ident ),* $(,)?) => {
        /// A styleable element.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum Element {
            $( $(#[$doc])* $variant, )*
        }

        impl Element {
            /// Every element, in declaration order (`e as usize` indexes this).
            pub const ALL: &'static [Element] = &[$(Element::$variant),*];
            /// Number of elements.
            pub const COUNT: usize = Self::ALL.len();

            /// The config name (`h1`, `code_inline`, `alert_note`, ...).
            pub const fn name(self) -> &'static str {
                match self { $(Element::$variant => $name,)* }
            }

            /// The element this one inherits from (`None` only for `Text`).
            pub const fn parent(self) -> Option<Element> {
                let parent = match self {
                    $(Element::$variant => Element::$parent,)*
                };
                if matches!(self, Element::Text) { None } else { Some(parent) }
            }
        }
    };
}

elements! {
    /// Body text (root of every chain).
    Text = "text" => Text,
    Heading = "heading" => Text,
    H1 = "h1" => Heading,
    H2 = "h2" => Heading,
    H3 = "h3" => Heading,
    H4 = "h4" => Heading,
    H5 = "h5" => Heading,
    H6 = "h6" => Heading,
    /// Heading decorations: h2 rule, markers, numbering.
    HeadingRule = "heading_rule" => Heading,
    Emph = "emph" => Text,
    Strong = "strong" => Text,
    Strike = "strike" => Text,
    Mark = "mark" => Text,
    Kbd = "kbd" => Text,
    Link = "link" => Text,
    /// A URL shown as text (numbered references, fallback when OSC 8 is off).
    LinkUrl = "link_url" => Link,
    /// The link focused in the pager.
    LinkFocus = "link_focus" => Link,
    /// `[1]`-style link reference numbers.
    LinkRef = "link_ref" => Muted,
    FootnoteRef = "footnote_ref" => Link,
    Code = "code" => Text,
    CodeInline = "code_inline" => Code,
    CodeBlock = "code_block" => Code,
    /// Language label / title row of a code block.
    CodeLabel = "code_label" => Muted,
    /// Line numbers and wrap markers.
    CodeGutter = "code_gutter" => Muted,
    Quote = "quote" => Text,
    QuoteBar = "quote_bar" => Muted,
    Alert = "alert" => Quote,
    AlertNote = "alert_note" => Alert,
    AlertTip = "alert_tip" => Alert,
    AlertImportant = "alert_important" => Alert,
    AlertWarning = "alert_warning" => Alert,
    AlertCaution = "alert_caution" => Alert,
    ListMarker = "list_marker" => Text,
    TaskDone = "task_done" => ListMarker,
    TaskTodo = "task_todo" => ListMarker,
    TableBorder = "table_border" => Muted,
    TableHeader = "table_header" => Strong,
    /// Background of every other table row (truecolor only).
    TableZebra = "table_zebra" => Text,
    Rule = "rule" => Muted,
    Footnote = "footnote" => Text,
    DefTerm = "def_term" => Strong,
    FrontMatterKey = "front_matter_key" => Muted,
    FrontMatterValue = "front_matter_value" => Text,
    /// Raw HTML shown as text.
    Html = "html" => Muted,
    ImageAlt = "image_alt" => ImageFrame,
    ImageCaption = "image_caption" => Muted,
    ImageFrame = "image_frame" => Muted,
    Math = "math" => Text,
    MathVar = "math_var" => Math,
    MathNum = "math_num" => Math,
    MathOp = "math_op" => Math,
    MathRel = "math_rel" => Math,
    MathFunc = "math_func" => Math,
    MathText = "math_text" => Math,
    MathDelim = "math_delim" => Math,
    MathError = "math_error" => Math,
    /// De-emphasised decoration (labels, borders, rules).
    Muted = "muted" => Text,
    Status = "status" => Text,
    StatusMsg = "status_msg" => Status,
    SearchMatch = "search_match" => Text,
    SearchCurrent = "search_current" => SearchMatch,
    Prompt = "prompt" => Status,
    Hint = "hint" => Text,
    Toc = "toc" => Text,
    TocCurrent = "toc_current" => Toc,
}

impl Element {
    /// Look up an element by its config name.
    pub fn from_name(name: &str) -> Option<Element> {
        Element::ALL.iter().copied().find(|e| e.name() == name)
    }

    /// Index into per-element tables.
    pub const fn index(self) -> usize {
        self as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_unique_and_roundtrip() {
        let mut seen = std::collections::HashSet::new();
        for &e in Element::ALL {
            assert!(seen.insert(e.name()), "duplicate name {}", e.name());
            assert_eq!(Element::from_name(e.name()), Some(e));
            assert_eq!(Element::ALL[e.index()], e);
        }
    }

    #[test]
    fn parent_chains_terminate_at_text() {
        for &e in Element::ALL {
            let mut cur = e;
            let mut steps = 0;
            while let Some(p) = cur.parent() {
                cur = p;
                steps += 1;
                assert!(steps < 16, "cycle from {}", e.name());
            }
            assert_eq!(cur, Element::Text);
        }
    }
}
