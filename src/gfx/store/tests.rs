//! Tests of the image store: sources, sizes, and the rows of every graphics
//! path.

use std::path::{Path, PathBuf};

use super::*;
use crate::config::paths::testdir::TestDir;
use crate::ir::PictureSource;

// --- Sources -------------------------------------------------------------

#[test]
fn locations() {
    let base = Path::new("/docs");
    let file = |s: &str| match locate(s, Some(base), false) {
        Ok(Location::File(p)) => p,
        other => panic!("{s}: {other:?}"),
    };
    assert_eq!(file("a.png"), PathBuf::from("/docs/a.png"));
    assert_eq!(
        file("img/a.png?raw=true#x"),
        PathBuf::from("/docs/img/a.png")
    );
    assert_eq!(file("/abs/a.png"), PathBuf::from("/abs/a.png"));
    assert_eq!(file("file:///abs/b.png"), PathBuf::from("/abs/b.png"));
    assert_eq!(
        file("file://localhost/abs/my%20b.png"),
        PathBuf::from("/abs/my b.png")
    );
    // A one-letter "scheme" is a Windows drive, i.e. a path.
    assert_eq!(file("c:x.png"), PathBuf::from("/docs/c:x.png"));
    assert!(
        locate("https://example.com/a.png", None, false)
            .unwrap_err()
            .contains("remote images are off")
    );
    assert_eq!(
        locate("https://example.com/a b.png", None, true),
        Ok(Location::Remote("https://example.com/a%20b.png".into()))
    );
    assert_eq!(
        locate("//cdn.example.com/a.png", None, true),
        Ok(Location::Remote("https://cdn.example.com/a.png".into()))
    );
    // A control character (shown as a control picture) is never passed on.
    assert!(locate("https://a.b/\u{241b}[2J", None, true).is_err());
    assert!(
        locate("ftp://example.com/a.png", None, true)
            .unwrap_err()
            .contains("`ftp:`")
    );
    assert!(locate("  ", None, true).is_err());
}

#[test]
fn data_uris() {
    assert_eq!(
        locate("data:image/png;base64,aGVsbG8=", None, false),
        Ok(Location::Data(b"hello".to_vec()))
    );
    assert_eq!(
        locate("DATA:image/svg+xml,%3Csvg%3E", None, false),
        Ok(Location::Data(b"<svg>".to_vec()))
    );
    assert!(data_uri("data:image/png;base64").is_err());
    assert!(data_uri("data:;base64,*!").is_err());
    assert_eq!(short_name("data:image/png;base64,AAAA"), "data:image/png");
    assert_eq!(short_name(&"x".repeat(200)).chars().count(), 121);
}

#[test]
fn percent_decoding() {
    assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz%4"), "%zz%4");
    assert_eq!(percent_decode("%C3%A9"), "é");
    assert_eq!(percent_decode("%FF"), "\u{fffd}");
}

#[test]
fn local_files_are_found_as_written_or_decoded() {
    let dir = TestDir::new("store-paths");
    dir.write("my image.png", "x");
    dir.write("sub/a%20b.png", "x");
    let base = Some(dir.path());
    assert_eq!(
        local_path("my%20image.png", base),
        dir.path().join("my image.png")
    );
    // A file whose name really has `%20` is taken as written.
    assert_eq!(
        local_path("sub/a%20b.png", base),
        dir.path().join("sub/a%20b.png")
    );
    // GitHub reads `/path` from the repository root.
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    dir.write("assets/logo.png", "x");
    dir.write("docs/guide.md", "x");
    let docs = dir.path().join("docs");
    let found = local_path("/assets/logo.png", Some(&docs));
    assert!(
        found.ends_with("assets/logo.png") && found.is_file(),
        "{found:?}"
    );
    assert_eq!(
        local_path("/nowhere/x.png", Some(&docs)),
        PathBuf::from("/nowhere/x.png")
    );
}

#[test]
fn reading_files() {
    let dir = TestDir::new("store-read");
    let file = dir.write("a.png", "abc");
    assert_eq!(read_file(&file).unwrap(), b"abc");
    assert!(read_file(dir.path()).unwrap_err().contains("regular file"));
    assert!(read_file(&dir.path().join("missing.png")).is_err());
    #[cfg(unix)]
    assert!(
        read_file(Path::new("/dev/zero"))
            .unwrap_err()
            .contains("regular file")
    );
}

#[test]
fn pictures_follow_the_theme_variant() {
    let img = ImageRef {
        src: "light.png".into(),
        sources: vec![
            PictureSource {
                srcset: "dark.png 1x, dark@2x.png 2x".into(),
                media: Some("( prefers-color-scheme : DARK )".into()),
            },
            PictureSource {
                srcset: "other.png".into(),
                media: None,
            },
        ],
        ..ImageRef::default()
    };
    assert_eq!(chosen_src(&img, Variant::Dark), "dark.png");
    assert_eq!(chosen_src(&img, Variant::Light), "light.png");
    let no_img = ImageRef {
        src: "".into(),
        sources: vec![PictureSource {
            srcset: "only.png".into(),
            media: Some("(prefers-color-scheme: dark)".into()),
        }],
        ..ImageRef::default()
    };
    assert_eq!(chosen_src(&no_img, Variant::Light), "only.png");
    assert_eq!(chosen_src(&ImageRef::default(), Variant::Dark), "");
}

#[test]
fn curl_gets_a_fixed_safe_command_line() {
    let cmd = curl_command("https://example.com/-x.png");
    assert_eq!(cmd.get_program(), "curl");
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args.first().map(String::as_str), Some("-q"), "no ~/.curlrc");
    let joined = args.join(" ");
    for part in [
        "--proto =http,https",
        "--proto-redir =http,https",
        "--max-time 5",
        "--max-filesize 20M",
        "--fail",
        "--output -",
    ] {
        assert!(joined.contains(part), "{part}: {joined}");
    }
    assert_eq!(
        args.get(args.len() - 2..),
        Some(&["--".to_owned(), "https://example.com/-x.png".to_owned()][..]),
        "the URL is never an option"
    );
}

// --- Sizes ---------------------------------------------------------------

#[test]
fn html_size_attributes() {
    let natural = (400, 200);
    let cell = (8, 16);
    let px = |w, h, cols| display_px(natural, (w, h), cell, cols);
    assert_eq!(px(None, None, 100), (400, 200));
    assert_eq!(px(Some(Length::Px(100)), None, 100), (100, 50));
    assert_eq!(px(None, Some(Length::Px(50)), 100), (100, 50));
    assert_eq!(
        px(Some(Length::Px(10)), Some(Length::Px(90)), 100),
        (10, 90)
    );
    // 50% of 100 columns of 8 pixels; percentages above 100 are 100.
    assert_eq!(px(Some(Length::Percent(50)), None, 100), (400, 200));
    assert_eq!(px(Some(Length::Percent(250)), None, 10), (80, 40));
    // Zero sizes and percentage heights are ignored.
    assert_eq!(
        px(Some(Length::Px(0)), Some(Length::Percent(10)), 100),
        (400, 200)
    );
}

#[test]
fn boxes() {
    assert_eq!(box_px(8, 2, (8, 16)), (64, 32));
    assert_eq!(fit_box((64, 32), (64, 30)), (60, 30));
    assert_eq!(fit_box((10, 10), (100, 50)), (50, 50), "scales up");
    assert_eq!(fit_box((0, 0), (5, 5)), (5, 5));
    assert_eq!(fit_box((1000, 1), (10, 10)), (10, 1));
}

#[test]
fn drawing_over_reserved_rows() {
    let out = over_reserved_rows(b"IMG", 3, 5);
    assert_eq!(out, b"\n\n\x1b[2A\r\x1b[5C\x1b7IMG\x1b8");
    // No zero-length moves: `CSI 0 A` would move one row.
    let out = over_reserved_rows(b"IMG", 1, 0);
    assert_eq!(out, b"\r\x1b7IMG\x1b8");
}

// --- Loading and rendering -------------------------------------------------

#[cfg(feature = "images")]
mod rendering {
    use super::*;
    use crate::highlight::PlainHighlighter;
    use crate::layout::layout;
    use crate::options::RenderOptions;
    use crate::parse::{ParseOptions, parse};

    fn opts(graphics: Graphics) -> StoreOptions {
        StoreOptions {
            graphics,
            glyphs: BlockGlyphSet::Half,
            depth: ColorDepth::TrueColor,
            background: Some(Rgb(0, 0, 0)),
            page: Rgb(0, 0, 0),
            cell_px: Some((8, 16)),
            passthrough: Passthrough::Direct,
            max_pixels: 40_000_000,
            remote: false,
            variant: Variant::Dark,
        }
    }

    /// A document in `dir`, with its base directory set as the program does.
    fn doc_in(dir: &Path, md: &str) -> Document {
        let mut doc = parse(md, &ParseOptions::default());
        doc.base_dir = Some(dir.to_path_buf());
        doc
    }

    /// The figure images of a document (top-level figures only).
    fn figures(doc: &Document) -> Vec<ImageId> {
        doc.blocks
            .iter()
            .filter_map(|block| match block {
                crate::ir::Block::Figure(f) => Some(f.image),
                _ => None,
            })
            .collect()
    }

    /// A PNG of `w × h` pixels of one colour.
    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = Rgba::filled(w, h, rgba).unwrap();
        crate::gfx::png::encode(&img).unwrap()
    }

    /// A directory with `a.png` (64×32 red) and `b.png` (16×16 blue).
    fn pictures() -> TestDir {
        let dir = TestDir::new("store-render");
        fs::write(dir.path().join("a.png"), png(64, 32, [255, 0, 0, 255])).unwrap();
        fs::write(dir.path().join("b.png"), png(16, 16, [0, 0, 255, 255])).unwrap();
        dir
    }

    /// Load, lay out at `width` and prepare for stream output.
    fn staged(doc: &Document, opts: StoreOptions, width: u16) -> (ImageStore, Layout) {
        let mut render = RenderOptions {
            max_width: 0,
            ..RenderOptions::default()
        };
        render.images.max_height = crate::options::Height::Rows(30);
        let mut store = ImageStore::load(doc, &figures(doc), opts);
        let theme = Theme::test();
        let l = layout(
            doc,
            width,
            &theme,
            &Caps::full(),
            &render,
            &PlainHighlighter,
            &store,
        );
        store.prepare_stream(&l);
        (store, l)
    }

    fn bytes<'a>(store: &'a ImageStore, p: &Placement, row: u16) -> &'a [u8] {
        match store.row(p, row) {
            Some(RowContent::Bytes(b)) => b,
            other => panic!("row {row}: {other:?}"),
        }
    }

    #[test]
    fn sizes_come_from_headers() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)\n\n![b](b.png)\n\n![c](c.png)");
        let store = ImageStore::load(&doc, &figures(&doc), opts(Graphics::Blocks));
        let ids = figures(&doc);
        assert_eq!(store.cells(ids[0], 100, 30), Some((8, 2)));
        assert_eq!(store.cells(ids[1], 100, 30), Some((2, 1)));
        assert_eq!(store.cells(ids[2], 100, 30), None, "missing file");
        assert_eq!(store.cells(ids[0], 4, 30), Some((4, 1)), "the measure");
        assert_eq!(store.problems().len(), 1);
        assert!(store.problems()[0].starts_with("c.png: "));
        // With images off nothing is sized, nothing read.
        let off = ImageStore::load(&doc, &figures(&doc), opts(Graphics::None));
        assert_eq!(off.cells(ids[0], 100, 30), None);
        assert!(off.problems().is_empty());
    }

    #[test]
    fn blocks_rows_are_raster_cells() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)");
        let (store, l) = staged(&doc, opts(Graphics::Blocks), 40);
        let p = l.images[0];
        assert_eq!((p.cols, p.rows), (8, 2));
        let red = crate::style::Color::Rgb(Rgb(255, 0, 0));
        for row in 0..2 {
            let Some(RowContent::Cells(cells)) = store.row(&p, row) else {
                panic!("row {row}");
            };
            assert_eq!(cells.len(), 8);
            assert!(cells.iter().all(|c| c.bg == red), "{cells:?}");
        }
        assert_eq!(store.row(&p, 2), None);
    }

    #[test]
    fn undecodable_images_keep_their_box() {
        let dir = TestDir::new("store-broken");
        let full = png(64, 32, [1, 2, 3, 255]);
        fs::write(dir.path().join("cut.png"), &full[..40]).unwrap();
        let doc = doc_in(dir.path(), "![cut](cut.png)");
        let (store, l) = staged(&doc, opts(Graphics::Blocks), 40);
        assert_eq!(l.images.len(), 1, "sized from the intact header");
        assert_eq!(store.row(&l.images[0], 0), None);
        assert!(
            store.problems()[0].contains("cannot decode"),
            "{:?}",
            store.problems()
        );
    }

    #[test]
    fn too_many_pixels_are_refused_before_decoding() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)");
        let small = StoreOptions {
            max_pixels: 100,
            ..opts(Graphics::Blocks)
        };
        let store = ImageStore::load(&doc, &figures(&doc), small);
        assert_eq!(store.cells(figures(&doc)[0], 100, 30), None);
        assert!(store.problems()[0].contains("images.max_pixels"));
    }

    #[test]
    fn data_uri_images() {
        let uri = format!(
            "![icon](data:image/png;base64,{})",
            b64::encode_string(&png(8, 8, [9, 9, 9, 255]))
        );
        let doc = parse(&uri, &ParseOptions::default());
        let store = ImageStore::load(&doc, &figures(&doc), opts(Graphics::Blocks));
        assert_eq!(store.cells(figures(&doc)[0], 100, 30), Some((1, 1)));
    }

    /// The image id of a kitty upload (`i=…`).
    fn upload_id(text: &str) -> kitty::ImageId {
        let start = text.find(",i=").unwrap() + 3;
        let digits: String = text[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        kitty::ImageId::new(digits.parse().unwrap()).unwrap()
    }

    #[test]
    fn kitty_placeholders_upload_once_then_print_text() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)\n\n![again](a.png)");
        let (mut store, l) = staged(&doc, opts(Graphics::KittyPlaceholders), 40);
        let (first, second) = (l.images[0], l.images[1]);
        let row0 = String::from_utf8(bytes(&store, &first, 0).to_vec()).unwrap();
        assert!(row0.starts_with("\x1b_Ga=T,U=1,i="), "{row0:?}");
        assert!(row0.contains(",f=100,t=d,c=8,r=2,q=2,m=0;"), "{row0:?}");
        let id = upload_id(&row0);
        let upload_end = row0.find("\x1b\\").unwrap() + 2;
        let text = |row| kitty::placeholder_row(id, row, 0..8).map(String::into_bytes);
        assert_eq!(
            row0.get(upload_end..).map(str::as_bytes),
            text(0).as_deref()
        );
        assert_eq!(Some(bytes(&store, &first, 1)), text(1).as_deref());
        // Another figure of the file is another image, with a fresh id.
        let other = String::from_utf8(bytes(&store, &second, 0).to_vec()).unwrap();
        assert_ne!(upload_id(&other), id);
        // Rows prepared again for the same sizes do not upload again.
        store.prepare_stream(&l);
        assert_eq!(Some(bytes(&store, &first, 0)), text(0).as_deref());
    }

    #[test]
    fn inside_tmux_no_kitty_command_is_unwrapped() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)");
        let tmux = StoreOptions {
            passthrough: Passthrough::Tmux,
            ..opts(Graphics::KittyPlaceholders)
        };
        let (store, l) = staged(&doc, tmux, 40);
        let row0 = bytes(&store, &l.images[0], 0);
        assert!(row0.starts_with(b"\x1bPtmux;\x1b\x1b_G"));
        let apcs: Vec<usize> = row0
            .windows(3)
            .enumerate()
            .filter(|(_, w)| *w == b"\x1b_G")
            .map(|(i, _)| i)
            .collect();
        assert!(!apcs.is_empty());
        for i in apcs {
            // Inside the passthrough DCS every ESC is doubled.
            assert!(i > 0 && row0[i - 1] == 0x1b, "unwrapped APC at {i}");
        }
    }

    #[test]
    fn kitty_classic_places_each_figure_with_its_own_id() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)\n\n![again](a.png)");
        let (mut store, l) = staged(&doc, opts(Graphics::KittyClassic), 40);
        let text = |store: &ImageStore, i: usize| {
            String::from_utf8(bytes(store, &l.images[i], 0).to_vec()).unwrap()
        };
        let first = text(&store, 0);
        // The column is absolute: the layout's indent plus the box's place.
        let col = l.indent + l.images[0].col;
        assert!(
            first.starts_with(&format!("\n\x1b[1A\r\x1b[{col}C\x1b7\x1b_Ga=t,i=")),
            "{first:?}"
        );
        assert!(
            first.ends_with(",p=1,c=8,r=2,C=1,q=2\x1b\\\x1b8"),
            "{first:?}"
        );
        assert_eq!(bytes(&store, &l.images[0], 1), b"", "row 1 is reserved");
        // Placed again, the image is not sent again, and the new placement
        // gets its own id (the same one would move the first).
        store.prepare_stream(&l);
        let again = text(&store, 0);
        assert!(!again.contains("a=t,"), "{again:?}");
        assert!(again.contains(",p=2,c=8,r=2,C=1,q=2"), "{again:?}");
        for t in [&first, &again, &text(&store, 1)] {
            for cmd in t.split("\x1b_G").skip(1) {
                assert!(cmd.contains("q=2"), "{cmd:?}");
            }
        }
    }

    #[test]
    fn iterm_gets_the_original_file_when_it_can() {
        use image::ImageEncoder as _;
        let dir = pictures();
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(
                &[0, 255, 0, 255].repeat(256),
                16,
                16,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        fs::write(dir.path().join("c.webp"), webp).unwrap();
        let doc = doc_in(dir.path(), "![a](a.png)\n\n![c](c.webp)");
        let (store, l) = staged(&doc, opts(Graphics::Iterm), 40);
        let original = fs::read(dir.path().join("a.png")).unwrap();
        let a = String::from_utf8(bytes(&store, &l.images[0], 0).to_vec()).unwrap();
        let want = format!(
            "\x1b]1337;File=inline=1;size={};width=8;height=2;preserveAspectRatio=1:{}\x07",
            original.len(),
            b64::encode_string(&original)
        );
        assert!(a.contains(&want), "{a:?}");
        // WebP is re-encoded as PNG.
        let c = String::from_utf8(bytes(&store, &l.images[1], 0).to_vec()).unwrap();
        assert!(c.contains(":iVBORw0KGgo"), "a PNG payload: {c:?}");
    }

    #[cfg(feature = "sixel")]
    #[test]
    fn sixel_is_scaled_to_whole_bands_of_its_box() {
        let dir = pictures();
        let doc = doc_in(dir.path(), "![a](a.png)");
        let (store, l) = staged(&doc, opts(Graphics::Sixel), 40);
        let row0 = String::from_utf8_lossy(bytes(&store, &l.images[0], 0)).into_owned();
        let dcs = row0.find("\x1bP").unwrap();
        assert!(row0[dcs..].contains("q\"1;1;60;30"), "{row0:?}");
        assert!(row0.ends_with("\x1b\\\x1b8"));
    }

    #[test]
    fn pixel_failures_fall_back_to_blocks_only_with_colours() {
        let dir = TestDir::new("store-fallback");
        // More columns than kitty placeholders can address.
        fs::write(dir.path().join("wide.png"), png(3200, 8, [5, 5, 5, 255])).unwrap();
        let doc = doc_in(dir.path(), "![w](wide.png)");
        let wide = |depth| {
            let o = StoreOptions {
                depth,
                ..opts(Graphics::KittyPlaceholders)
            };
            staged(&doc, o, 400)
        };
        let (store, l) = wide(ColorDepth::TrueColor);
        assert!(l.images[0].cols > kitty::MAX_CELLS, "{:?}", l.images[0]);
        assert!(matches!(
            store.row(&l.images[0], 0),
            Some(RowContent::Cells(_))
        ));
        let (store, l) = wide(ColorDepth::Mono);
        assert_eq!(
            store.row(&l.images[0], 0),
            None,
            "no blocks without colours"
        );
    }
}
