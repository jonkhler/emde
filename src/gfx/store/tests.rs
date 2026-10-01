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
    assert_eq!(file("file:/abs/c.png?x#y"), PathBuf::from("/abs/c.png"));
    // No file here: another host, or no absolute path.
    for uri in ["file://example.com/abs/a.png", "file:a.png", "file://"] {
        assert!(locate(uri, Some(base), false).is_err(), "{uri}");
    }
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
fn large_images_are_decoded_on_fewer_threads() {
    // Screenshots and icons: every thread.
    assert_eq!(decode_threads(0), MAX_THREADS);
    assert_eq!(decode_threads(1920 * 1080), MAX_THREADS);
    // 12 MP photos (48 MB decoded): five at a time; 40 MP: one.
    assert_eq!(decode_threads(4000 * 3000), 5);
    assert_eq!(decode_threads(40_000_000), 1);
    assert_eq!(decode_threads(u64::MAX), 1);
}

#[test]
fn drawing_over_reserved_rows() {
    // The box's 3 rows and the one below it on screen, back up to the box,
    // the image drawn from the saved cursor, then over to the right edge.
    let out = over_reserved_rows(b"IMG", 3, 5, 8);
    assert_eq!(out, b"\n\n\n\x1b[3A\r\x1b[5C\x1b7IMG\x1b8\x1b[8C");
    // No zero-length moves: `CSI 0 C` would move one column.
    let out = over_reserved_rows(b"IMG", 1, 0, 2);
    assert_eq!(out, b"\n\x1b[1A\r\x1b7IMG\x1b8\x1b[2C");
    assert_eq!(cursor_forward(0), b"");
    assert_eq!(cursor_forward(12), b"\x1b[12C");
}

#[test]
fn figures_are_found_at_any_depth() {
    use crate::parse::{ParseOptions, parse};
    let md = "![top](a.png)\n\n> ![quoted](b.png)\n\n- ![listed](c.png)\n\n\
              Inline ![chip](d.png) only.\n\n![again](a.png)\n\n[^n]\n\n[^n]: ![noted](e.png)\n";
    let doc = parse(md, &ParseOptions::default());
    let srcs: Vec<&str> = figure_images(&doc)
        .into_iter()
        .map(|id| &*doc.image(id).unwrap().src)
        .collect();
    // Chips are not figures; each figure is its own image.
    assert_eq!(srcs, ["a.png", "b.png", "c.png", "a.png", "e.png"]);
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
            blocks: true,
        }
    }

    /// A document in `dir`, with its base directory set as the program does.
    fn doc_in(dir: &Path, md: &str) -> Document {
        let mut doc = parse(md, &ParseOptions::default());
        doc.base_dir = Some(dir.to_path_buf());
        doc
    }

    /// The figure images of a document.
    fn figures(doc: &Document) -> Vec<ImageId> {
        figure_images(doc)
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
            first.starts_with(&format!("\n\n\x1b[2A\r\x1b[{col}C\x1b7\x1b_Ga=t,i=")),
            "{first:?}"
        );
        assert!(
            first.ends_with(",p=1,c=8,r=2,C=1,q=2\x1b\\\x1b8\x1b[8C"),
            "{first:?}"
        );
        // Row 1 is reserved: the cursor only moves over the image.
        assert_eq!(bytes(&store, &l.images[0], 1), b"\x1b[8C");
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
    fn large_iterm_images_shrink_until_they_fit_one_sequence() {
        // Noise does not compress: 1600×800 is a 3.8 MB PNG, and even at
        // twice the 50×13-cell box (800×400 px) more than iTerm2 takes in
        // one sequence. At the box's own size it fits.
        let dir = TestDir::new("store-iterm-large");
        let mut seed = 0x2545_f491_u32;
        let mut noise = Vec::with_capacity(1600 * 800 * 4);
        for _ in 0..1600 * 800 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let [r, g, b, _] = seed.to_le_bytes();
            noise.extend_from_slice(&[r, g, b, 255]);
        }
        let img = Rgba::new(1600, 800, noise).unwrap();
        fs::write(
            dir.path().join("noise.png"),
            crate::gfx::png::encode(&img).unwrap(),
        )
        .unwrap();
        let doc = doc_in(dir.path(), "![n](noise.png)");
        let (store, l) = staged(&doc, opts(Graphics::Iterm), 54);
        let p = l.images[0];
        assert_eq!((p.cols, p.rows), (50, 13));
        let row0 = String::from_utf8(bytes(&store, &p, 0).to_vec()).unwrap();
        let start = row0.find("\x1b]1337;File=").expect("pixels, not blocks");
        let end = row0.find('\x07').unwrap();
        assert!(end - start <= iterm::MAX_SEQUENCE, "{}", end - start);
        let payload = row0[start..end].split_once(':').unwrap().1;
        let png = b64::decode(payload.as_bytes()).unwrap();
        assert_eq!(crate::gfx::decode::dimensions(&png), Some((400, 200)));
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
        assert!(row0.ends_with("\x1b\\\x1b8\x1b[8C"), "{row0:?}");
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
    // --- The pager's renditions ----------------------------------------------

    /// The source of the first figure of `md` in `dir`.
    fn source_of(dir: &Path, md: &str, opts: StoreOptions) -> (ImageStore, Source) {
        let doc = doc_in(dir, md);
        let store = ImageStore::load_figures(&doc, opts);
        let src = store.source(figure_images(&doc)[0]).unwrap();
        (store, src)
    }

    /// `make` with a decoder that counts its calls.
    fn made(
        src: &Source,
        what: Make,
        cols: u16,
        rows: u16,
        opts: &StoreOptions,
    ) -> (Option<Made>, usize) {
        let mut calls = 0;
        let mut decode = || {
            calls += 1;
            src.decode(opts.max_pixels).map(Arc::new)
        };
        let out = make(src, &what, cols, rows, opts, &mut decode);
        (out, calls)
    }

    #[test]
    fn sources_carry_the_file_to_other_threads() {
        let dir = pictures();
        let (store, src) = source_of(dir.path(), "![a](a.png)", opts(Graphics::Blocks));
        assert_eq!(src.size(), (64, 32));
        assert_eq!(src.name(), "a.png");
        let img = src.decode(10_000).unwrap();
        assert_eq!((img.width, img.height), (64, 32));
        assert!(src.decode(10).is_none(), "the pixel limit holds");
        assert_eq!(store.options().graphics, Graphics::Blocks);
        assert!(store.source(ImageId(7)).is_none());
        std::thread::spawn(move || assert_eq!(src.size(), (64, 32)))
            .join()
            .unwrap();
    }

    #[test]
    fn pager_blocks_and_kitty_uploads() {
        let dir = pictures();
        let o = opts(Graphics::KittyPlaceholders);
        let (_, src) = source_of(dir.path(), "![a](a.png)", o.clone());
        let (blocks, calls) = made(&src, Make::Blocks, 8, 2, &o);
        let Some(Made::Blocks(raster)) = blocks else {
            panic!("{blocks:?}");
        };
        assert_eq!((raster.cols, raster.rows, calls), (8, 2, 1));
        // A small PNG is uploaded as it is: nothing decoded.
        let (ph, calls) = made(&src, Make::Placeholders, 8, 2, &o);
        let Some(Made::Placeholders(ph)) = ph else {
            panic!("{ph:?}");
        };
        assert_eq!(calls, 0);
        assert_eq!(ph.rows.len(), 2);
        assert_eq!(
            ph.rows[1],
            kitty::placeholder_row(ph.id, 1, 0..8).unwrap().into_bytes()
        );
        let head = format!("\x1b_Ga=T,U=1,i={},f=100,t=d,c=8,r=2,q=2,m=0;", ph.id.get());
        assert!(ph.upload.starts_with(head.as_bytes()));
        // Every upload is a new image.
        let (again, _) = made(&src, Make::Placeholders, 8, 2, &o);
        assert!(matches!(again, Some(Made::Placeholders(p)) if p.id != ph.id));
        let (k, calls) = made(&src, Make::Kitty, 8, 2, &o);
        let Some(Made::Kitty(k)) = k else {
            panic!("{k:?}");
        };
        assert_eq!((k.height, calls), (32, 0), "the original PNG's height");
        let head = format!("\x1b_Ga=t,i={},f=100,t=d,q=2,m=0;", k.id.get());
        assert!(k.upload.starts_with(head.as_bytes()));
        // Too many cells for placeholders; no cells at all.
        assert_eq!(made(&src, Make::Placeholders, 300, 2, &o).0, None);
        assert_eq!(made(&src, Make::Blocks, 0, 2, &o).0, None);
    }

    #[test]
    fn large_kitty_images_are_downscaled_to_twice_their_box() {
        let dir = TestDir::new("store-kitty-large");
        fs::write(dir.path().join("big.png"), png(800, 400, [1, 2, 3, 255])).unwrap();
        let o = opts(Graphics::KittyClassic);
        let (_, src) = source_of(dir.path(), "![b](big.png)", o.clone());
        // 10×2 cells are 80×32 pixels: sent at 160×64 at most.
        let (k, calls) = made(&src, Make::Kitty, 10, 2, &o);
        let Some(Made::Kitty(k)) = k else {
            panic!("{k:?}");
        };
        assert_eq!(calls, 1);
        assert_eq!(k.height, 64);
    }

    /// The `height=` of an iTerm2 sequence and the size of its PNG.
    fn iterm_slice(bytes: &[u8]) -> (u16, (u32, u32)) {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let args = text.strip_prefix("\x1b]1337;File=").unwrap();
        let (args, payload) = args.split_once(':').unwrap();
        let height = args
            .split(';')
            .find_map(|a| a.strip_prefix("height="))
            .unwrap()
            .parse()
            .unwrap();
        let png = b64::decode(payload.trim_end_matches('\x07').as_bytes()).unwrap();
        (height, crate::gfx::decode::dimensions(&png).unwrap())
    }

    #[test]
    fn iterm_slices_of_partly_visible_images() {
        let dir = TestDir::new("store-iterm-slices");
        fs::write(dir.path().join("tall.png"), png(64, 128, [9, 9, 9, 255])).unwrap();
        let o = opts(Graphics::Iterm);
        let (_, src) = source_of(dir.path(), "![t](tall.png)", o.clone());
        // Whole: the original file, nothing decoded.
        let (whole, calls) = made(&src, Make::Iterm(0..8), 8, 8, &o);
        let Some(Made::Pixels(whole)) = whole else {
            panic!("{whole:?}");
        };
        assert_eq!(calls, 0);
        assert_eq!(iterm_slice(&whole), (8, (64, 128)));
        // Rows 2..5 of 8: 48 of the 128 pixel rows, as a slice drawn over 3
        // rows (scaled to twice the slice's box at most: 128×96).
        let (slice, calls) = made(&src, Make::Iterm(2..5), 8, 8, &o);
        let Some(Made::Pixels(slice)) = slice else {
            panic!("{slice:?}");
        };
        assert_eq!(calls, 1);
        assert_eq!(iterm_slice(&slice), (3, (64, 48)));
        // Rows past the box are cut; nothing left is nothing.
        let (cut, _) = made(&src, Make::Iterm(6..20), 8, 8, &o);
        let Some(Made::Pixels(cut)) = cut else {
            panic!("{cut:?}");
        };
        assert_eq!(iterm_slice(&cut).0, 2);
        assert_eq!(made(&src, Make::Iterm(8..9), 8, 8, &o).0, None);
    }

    #[cfg(feature = "sixel")]
    #[test]
    fn sixel_slices_end_on_whole_bands() {
        let dir = TestDir::new("store-sixel-slices");
        fs::write(dir.path().join("tall.png"), png(64, 128, [9, 9, 9, 255])).unwrap();
        let o = opts(Graphics::Sixel);
        let (_, src) = source_of(dir.path(), "![t](tall.png)", o.clone());
        let raster = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes).into_owned();
            let attrs = text.split('"').nth(1).unwrap().to_owned();
            attrs
                .split(|c: char| !c.is_ascii_digit())
                .take(4)
                .map(|f| f.parse::<u32>().unwrap())
                .collect::<Vec<_>>()
        };
        // 8×8 cells of 8×16 pixels: a 64×128 box, all whole bands (126).
        let (whole, _) = made(&src, Make::Sixel(0..8), 8, 8, &o);
        let Some(Made::Pixels(whole)) = whole else {
            panic!("{whole:?}");
        };
        assert_eq!(raster(&whole), [1, 1, 63, 126]);
        // Rows 1..3: 32 pixels, cut to 30 (five whole bands).
        let (slice, _) = made(&src, Make::Sixel(1..3), 8, 8, &o);
        let Some(Made::Pixels(slice)) = slice else {
            panic!("{slice:?}");
        };
        assert_eq!(raster(&slice), [1, 1, 63, 30]);
    }

    // --- SVG -------------------------------------------------------------------

    /// A 64×32 SVG (from its viewBox) of one colour.
    fn svg_of(fill: &str) -> String {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 32\">\
             <rect width=\"64\" height=\"32\" fill=\"{fill}\"/></svg>"
        )
    }

    #[cfg(not(feature = "svg"))]
    #[test]
    fn without_the_svg_feature_svg_keeps_its_alt_text() {
        let dir = TestDir::new("store-svg-off");
        fs::write(dir.path().join("r.svg"), svg_of("red")).unwrap();
        let doc = doc_in(dir.path(), "![r](r.svg)");
        let store = ImageStore::load(&doc, &figures(&doc), opts(Graphics::Blocks));
        assert_eq!(store.cells(figures(&doc)[0], 100, 30), None);
        assert!(
            store.problems()[0].starts_with("r.svg: SVG images are not supported"),
            "{:?}",
            store.problems()
        );
    }

    #[cfg(feature = "svg")]
    mod svg_figures {
        use super::*;

        /// A directory with `r.svg` (red), `badge` (green, no extension)
        /// and `broken.svg`.
        fn drawings() -> TestDir {
            let dir = TestDir::new("store-svg");
            fs::write(dir.path().join("r.svg"), svg_of("red")).unwrap();
            fs::write(dir.path().join("badge"), svg_of("lime")).unwrap();
            fs::write(dir.path().join("broken.svg"), "<svg><g></svg>").unwrap();
            dir
        }

        #[test]
        fn sized_from_the_svg_by_extension_type_or_content() {
            let dir = drawings();
            let red = svg_of("red");
            let md = format!(
                "![r](r.svg)\n\n![b](badge)\n\n![x](broken.svg)\n\n\
                 ![p](data:image/svg+xml,{})\n\n![64](data:image/svg+xml;base64,{})",
                red.replace('<', "%3C")
                    .replace('>', "%3E")
                    .replace('"', "%22")
                    .replace(' ', "%20"),
                b64::encode_string(red.as_bytes()),
            );
            let doc = doc_in(dir.path(), &md);
            let store = ImageStore::load(&doc, &figures(&doc), opts(Graphics::Blocks));
            let ids = figures(&doc);
            // 64×32 px at 8×16 px cells, like a PNG of that size.
            for i in [0, 1, 3, 4] {
                assert_eq!(store.cells(ids[i], 100, 30), Some((8, 2)), "figure {i}");
            }
            assert_eq!(
                store.cells(ids[2], 100, 30),
                None,
                "malformed: its alt text"
            );
            assert_eq!(store.problems().len(), 1, "{:?}", store.problems());
            assert!(
                store.problems()[0].starts_with("broken.svg: cannot read SVG"),
                "{:?}",
                store.problems()
            );
        }

        #[test]
        fn svg_is_not_held_to_the_pixel_limit_of_its_size() {
            // The intrinsic size is only a hint: a huge viewBox is drawn at
            // the size of its box, within images.max_pixels.
            let dir = TestDir::new("store-svg-huge");
            fs::write(
                dir.path().join("huge.svg"),
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1e7 5e6\">\
                 <rect width=\"1e7\" height=\"5e6\" fill=\"red\"/></svg>",
            )
            .unwrap();
            let small = StoreOptions {
                max_pixels: 1000,
                ..opts(Graphics::Blocks)
            };
            let doc = doc_in(dir.path(), "![h](huge.svg)");
            let (store, l) = staged(&doc, small.clone(), 40);
            let p = l.images[0];
            assert_eq!(
                (p.cols, p.rows),
                (36, 9),
                "the measure and the aspect ratio"
            );
            let src = store.source(figures(&doc)[0]).unwrap();
            let at = src.decode_size(p.cols, p.rows, &small).unwrap();
            assert!(u64::from(at.0) * u64::from(at.1) <= 1000, "{at:?}");
            assert!(matches!(store.row(&p, 0), Some(RowContent::Cells(_))));
        }

        #[test]
        fn drawn_at_the_size_of_each_path() {
            let dir = drawings();
            let decode_size = |graphics| {
                let o = opts(graphics);
                let (_, src) = source_of(dir.path(), "![r](r.svg)", o.clone());
                src.decode_size(8, 2, &o)
            };
            // An 8×2 box of 8×16 px cells is 64×32 pixels.
            assert_eq!(decode_size(Graphics::Blocks), Some((64, 32)));
            assert_eq!(decode_size(Graphics::KittyPlaceholders), Some((128, 64)));
            assert_eq!(decode_size(Graphics::KittyClassic), Some((128, 64)));
            assert_eq!(decode_size(Graphics::Iterm), Some((128, 64)));
            // Sixel: whole bands of six rows (30), keeping the aspect ratio.
            assert_eq!(decode_size(Graphics::Sixel), Some((60, 30)));
            // Raster images are decoded at their own size.
            let pngs = pictures();
            let o = opts(Graphics::Blocks);
            let (_, png) = source_of(pngs.path(), "![a](a.png)", o.clone());
            assert_eq!(png.decode_size(8, 2, &o), None);
        }

        #[test]
        fn blocks_of_an_svg() {
            let dir = drawings();
            let doc = doc_in(dir.path(), "![r](r.svg)");
            let (store, l) = staged(&doc, opts(Graphics::Blocks), 40);
            let p = l.images[0];
            assert_eq!((p.cols, p.rows), (8, 2));
            let red = crate::style::Color::Rgb(Rgb(255, 0, 0));
            for row in 0..2 {
                let Some(RowContent::Cells(cells)) = store.row(&p, row) else {
                    panic!("row {row}");
                };
                assert!(cells.iter().all(|c| c.bg == red), "{cells:?}");
            }
        }

        /// Decode the PNG inside a kitty upload.
        fn uploaded_png(upload: &[u8]) -> Rgba {
            let text = String::from_utf8_lossy(upload);
            let payload: String = text
                .split("\x1b_G")
                .filter_map(|cmd| cmd.split_once(';'))
                .map(|(_, rest)| rest.trim_end_matches("\x1b\\").to_owned())
                .collect();
            let bytes = b64::decode(payload.as_bytes()).unwrap();
            crate::gfx::decode::decode(&bytes, 1 << 20).unwrap()
        }

        #[test]
        fn pixel_protocols_get_the_drawing_at_their_size() {
            let dir = drawings();
            // The pager: kitty classic at twice the box, drawn once for
            // that size, sent as a PNG of exactly that size.
            let o = opts(Graphics::KittyClassic);
            let (_, src) = source_of(dir.path(), "![r](r.svg)", o.clone());
            let at = src.decode_size(8, 2, &o);
            let mut calls = 0;
            let mut decode = || {
                calls += 1;
                src.decode_at(o.max_pixels, at).map(Arc::new)
            };
            let Some(Made::Kitty(k)) = make(&src, &Make::Kitty, 8, 2, &o, &mut decode) else {
                panic!("no kitty upload");
            };
            assert_eq!((k.height, calls), (64, 1));
            let png = uploaded_png(&k.upload);
            assert_eq!((png.width, png.height), (128, 64));
            assert_eq!(png.pixel(64, 32), [255, 0, 0, 255]);
            // Stream output: kitty placeholders and iTerm2 PNGs of the
            // drawing, never the SVG file itself.
            let doc = doc_in(dir.path(), "![r](r.svg)");
            let (store, l) = staged(&doc, opts(Graphics::KittyPlaceholders), 40);
            let first = String::from_utf8_lossy(bytes(&store, &l.images[0], 0)).into_owned();
            assert!(first.contains(",f=100,"), "{first:?}");
            let (store, l) = staged(&doc, opts(Graphics::Iterm), 40);
            let first = bytes(&store, &l.images[0], 0);
            let start = first.windows(4).position(|w| w == b"\x1b]13").unwrap();
            let end = start + first[start..].iter().position(|&b| b == 7).unwrap();
            let (height, size) = iterm_slice(&first[start..=end]);
            assert_eq!((height, size), (2, (128, 64)));
        }

        #[cfg(feature = "sixel")]
        #[test]
        fn sixel_of_an_svg() {
            let dir = drawings();
            let doc = doc_in(dir.path(), "![r](r.svg)");
            let (store, l) = staged(&doc, opts(Graphics::Sixel), 40);
            let first = String::from_utf8_lossy(bytes(&store, &l.images[0], 0)).into_owned();
            assert!(first.contains("\"1;1;60;30"), "{first:?}");
        }
    }
}
