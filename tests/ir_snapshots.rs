//! Snapshots of the parsed document model (`Document::dump`) for the
//! fixture documents in `tests/fixtures/md/`. A change here means the parser
//! changed what a document *is*; layout snapshots cover how it looks.

use emde::parse::{ParseOptions, parse_source};
use emde::source::Source;

fn dump(name: &str) -> String {
    let path = format!("{}/tests/fixtures/md/{name}.md", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let source = Source::from_bytes(bytes, emde::source::Origin::Memory);
    let doc = parse_source(&source, &ParseOptions::default());
    if let Err(e) = doc.validate() {
        panic!("{name}: invalid document: {e}");
    }
    doc.dump()
}

macro_rules! ir_snapshot {
    ($($test:ident => $name:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                insta::assert_snapshot!(concat!("ir-", $name), dump($name));
            }
        )*
    };
}

ir_snapshot! {
    kitchen_sink => "kitchen-sink",
    readme_html => "readme-html",
    llm_math => "llm-math",
    edge_cases => "edge-cases",
}
