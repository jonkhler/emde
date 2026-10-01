//! Images end to end: sources, sizes and every graphics path in stream mode,
//! through the binary.
//!
//! The fixtures in `tests/fixtures/images` are tiny generated files; run
//! `cargo test --test images -- --ignored write_fixtures` to regenerate
//! them.

#![cfg(feature = "images")]

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use image::codecs::gif::GifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, Frame, ImageEncoder, RgbaImage};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/images")
}

/// A 64×32 opaque gradient: red to blue across, darker downwards.
fn gradient() -> RgbaImage {
    RgbaImage::from_fn(64, 32, |x, y| {
        let r = (255 - x * 4) as u8;
        let b = (x * 4) as u8;
        let g = (y * 8) as u8;
        image::Rgba([r, g, b, 255])
    })
}

/// A 32×32 green disc on a transparent background.
fn disc() -> RgbaImage {
    RgbaImage::from_fn(32, 32, |x, y| {
        let (dx, dy) = (x as i32 - 16, y as i32 - 16);
        let inside = dx * dx + dy * dy <= 14 * 14;
        image::Rgba([40, 200, 80, if inside { 255 } else { 0 }])
    })
}

fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

fn jpeg(img: &RgbaImage) -> Vec<u8> {
    let rgb: Vec<u8> = img.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 90)
        .write_image(&rgb, img.width(), img.height(), ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn gif(frames: &[RgbaImage]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.encode_frames(frames.iter().cloned().map(Frame::new))
            .unwrap();
    }
    out
}

fn webp(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

/// Regenerate the committed fixtures.
#[test]
#[ignore = "writes tests/fixtures/images"]
fn write_fixtures() {
    let dir = fixtures();
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, bytes: &[u8]| std::fs::write(dir.join(name), bytes).unwrap();
    write("gradient.png", &png(&gradient()));
    write("disc.png", &png(&disc()));
    write("photo.jpg", &jpeg(&gradient()));
    let blue = RgbaImage::from_pixel(32, 16, image::Rgba([0, 0, 255, 255]));
    let red = RgbaImage::from_pixel(32, 16, image::Rgba([255, 0, 0, 255]));
    write("anim.gif", &gif(&[red, blue]));
    write("tiny.webp", &webp(&disc()));
    let icon = RgbaImage::from_pixel(8, 8, image::Rgba([250, 180, 40, 255]));
    write("icon.png", &png(&icon));
    // Header intact (64×32), pixel data cut off.
    let full = png(&gradient());
    write("truncated.png", &full[..40]);
    write("empty.png", b"");
    write("not-an-image.png", b"this is text, not a PNG\n");
    write(
        "logo.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\" fill=\"red\"/></svg>\n",
    );
}

#[test]
fn committed_fixtures_are_what_the_generator_makes() {
    let read = |name: &str| std::fs::read(fixtures().join(name)).unwrap();
    assert_eq!(read("gradient.png"), png(&gradient()));
    assert_eq!(read("disc.png"), png(&disc()));
    assert!(read("photo.jpg").starts_with(b"\xff\xd8\xff"));
    assert!(read("anim.gif").starts_with(b"GIF8"));
    assert!(read("tiny.webp").starts_with(b"RIFF"));
    assert_eq!(read("truncated.png").len(), 40);
    assert!(read("empty.png").is_empty());
}

// --- Through the binary ------------------------------------------------------

/// The binary with a controlled environment (no terminal hints, no user
/// configuration), `TERM=xterm-256color` unless `env` says otherwise.
fn emde(env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_emde"));
    for var in emde::term::env::VARS {
        cmd.env_remove(var);
    }
    for var in ["EMDE_CONFIG", "XDG_CONFIG_HOME", "EMDE_TRACE"] {
        cmd.env_remove(var);
    }
    cmd.env("HOME", "/nonexistent/emde-test-home");
    cmd.env("TERM", "xterm-256color");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// A file name no other test of this process uses.
fn scratch_file(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("emde-images-{}-{tag}-{n}.md", std::process::id()))
}

/// Show `md` (images relative to the fixtures) with `args`.
fn show(md: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let doc = scratch_file("show");
    let md = md.replace("](", &format!("]({}/", fixtures().display()));
    std::fs::write(&doc, md).unwrap();
    let o = emde(env).args(args).arg(&doc).output().unwrap();
    let _ = std::fs::remove_file(&doc);
    o
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// `s` without SGR sequences (`ESC [ … m`).
fn unstyled(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Every kitty graphics command in the output.
fn kitty_commands(out: &str) -> Vec<&str> {
    out.split("\x1b_G").skip(1).collect()
}

const GRADIENT: &str = "![Gradient](gradient.png)";

#[test]
fn block_images_in_truecolor() {
    let o = show(
        GRADIENT,
        &["--color=truecolor", "--images", "blocks", "-w", "40"],
        &[],
    );
    assert!(o.status.success());
    let out = text(&o);
    let rows: Vec<&str> = out.lines().filter(|l| l.contains('▀')).collect();
    assert_eq!(rows.len(), 2, "64×32 px at 8×16 px cells: {out}");
    for row in rows {
        assert_eq!(row.matches('▀').count(), 8, "{row:?}");
        assert!(
            row.contains("\x1b[38;2;") && row.contains(";48;2;"),
            "{row:?}"
        );
        assert!(row.ends_with("\x1b[0m"), "{row:?}");
    }
    // The alt text is not shown over the image; the caption is below it.
    assert!(!out.contains('▣'));
    assert!(out.contains("Gradient"));
}

#[test]
fn block_glyph_sets_and_colour_depths() {
    // The disc's round edge needs quadrants that half blocks do not have.
    let disc = "![Disc](disc.png)";
    let o = show(
        disc,
        &[
            "--color=truecolor",
            "--images",
            "blocks",
            "--blocks",
            "quadrant",
        ],
        &[],
    );
    let out = text(&o);
    assert!(
        out.chars().any(|c| ('\u{2596}'..='\u{259f}').contains(&c)),
        "quadrant glyphs: {out}"
    );
    let o = show(GRADIENT, &["--color=256", "--images", "blocks"], &[]);
    let out = text(&o);
    assert!(
        out.contains("\x1b[38;5;") && !out.contains("38;2;"),
        "{out}"
    );
    // Without colours there are no blocks: the alt text instead.
    let o = show(GRADIENT, &["--images", "blocks"], &[("NO_COLOR", "1")]);
    assert!(unstyled(&text(&o)).contains("▣ Gradient"), "{}", text(&o));
}

#[test]
fn piped_output_and_images_off_show_alt_text() {
    for args in [&[][..], &["--color=always", "--images", "none"]] {
        let o = show(GRADIENT, args, &[]);
        let out = text(&o);
        assert!(unstyled(&out).contains("▣ Gradient"), "{args:?}: {out}");
        assert!(!out.contains('▀'));
    }
}

#[test]
fn kitty_placeholders_upload_then_print_placeholder_text() {
    let ghostty = [
        ("TERM_PROGRAM", "ghostty"),
        ("TERM_PROGRAM_VERSION", "1.3.1"),
    ];
    let args = ["--color=always", "--images", "kitty", "-w", "40"];
    let o = show(GRADIENT, &args, &ghostty);
    assert!(o.status.success());
    let out = text(&o);
    let commands = kitty_commands(&out);
    assert_eq!(commands.len(), 1, "one upload of one chunk: {out:?}");
    assert!(commands[0].starts_with("a=T,U=1,i="), "{out:?}");
    assert!(commands[0].contains(",f=100,t=d,c=8,r=2,q=2,m=0;iVBORw0KGgo"));
    let rows: Vec<&str> = out.lines().filter(|l| l.contains('\u{10EEEE}')).collect();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.matches('\u{10EEEE}').count(), 8);
        assert!(
            row.contains("\x1b[38;2;") && row.ends_with("\x1b[39m"),
            "{row:?}"
        );
    }
    // Every run uploads under a fresh id.
    let again = text(&show(GRADIENT, &args, &ghostty));
    let id = |s: &str| {
        s.split(",i=")
            .nth(1)
            .and_then(|r| r.split(',').next())
            .map(str::to_owned)
    };
    assert_ne!(id(&out), id(&again));
}

#[test]
fn kitty_classic_placements_draw_over_reserved_rows() {
    let o = show(
        GRADIENT,
        &["--color=always", "--images", "kitty", "-w", "40"],
        &[("TERM", "xterm-kitty")],
    );
    let out = text(&o);
    let commands = kitty_commands(&out);
    assert_eq!(commands.len(), 2, "{out:?}");
    assert!(commands[0].starts_with("a=t,i="));
    assert!(commands[1].starts_with("a=p,i=") && commands[1].contains(",p=1,c=8,r=2,C=1,q=2"));
    for cmd in commands {
        assert!(cmd.contains("q=2"), "{cmd:?}");
    }
    // Two newlines reserve the second row and the one below the box, the
    // cursor goes back up, over to the box (column 14: centred in the
    // 36-column measure, with no indent when piped) and is saved around
    // the image, then moves over it to the box's right edge.
    assert!(
        out.contains("\n\n\x1b[2A\r\x1b[14C\x1b7\x1b_Ga=t,"),
        "{out:?}"
    );
    assert!(out.contains("q=2\x1b\\\x1b8\x1b[8C\n"), "{out:?}");
}

#[test]
fn iterm_inline_images_send_the_file() {
    let o = show(
        GRADIENT,
        &["--color=always", "--images", "iterm", "-w", "40"],
        &[("TERM_PROGRAM", "iTerm.app")],
    );
    let out = text(&o);
    let file = std::fs::read(fixtures().join("gradient.png")).unwrap();
    let head = format!(
        "\x1b]1337;File=inline=1;size={};width=8;height=2;preserveAspectRatio=1:",
        file.len()
    );
    assert!(out.contains(&head), "{out:?}");
    assert!(out.contains("\x1b7\x1b]1337;") && out.contains("\x07\x1b8"));
}

#[cfg(feature = "sixel")]
#[test]
fn sixel_images() {
    let o = show(
        GRADIENT,
        &["--color=always", "--images", "sixel", "-w", "40"],
        &[],
    );
    let out = text(&o);
    let dcs = out.find("\x1bP").expect("a sixel sequence");
    assert!(out[dcs..].contains("q\"1;1;60;30"), "{out:?}");
    assert!(out.contains("\x1b\\\x1b8"));
}

#[test]
fn inside_tmux_kitty_commands_are_never_unwrapped() {
    // Piped, so there is no tmux query to say passthrough is on: the
    // forced mode is refused, and no APC may reach tmux.
    let o = show(
        GRADIENT,
        &["--color=always", "--images", "kitty"],
        &[
            ("TMUX", "/nonexistent/emde-test/socket,1,0"),
            ("TERM", "tmux-256color"),
            ("TERM_PROGRAM", "ghostty"),
        ],
    );
    let out = text(&o);
    assert!(!out.contains("\x1b_G"), "{out:?}");
    assert!(out.contains('▀'), "blocks instead: {out:?}");
}

#[test]
fn pictures_follow_the_background() {
    let dir = fixtures();
    let md = format!(
        "<picture>\n<source media=\"(prefers-color-scheme: dark)\" srcset=\"{0}/disc.png\">\n\
         <img src=\"{0}/gradient.png\" alt=\"p\">\n</picture>\n",
        dir.display()
    );
    let md = md.as_str();
    // The dark source is the green disc, the light one the gradient.
    let dark = show(
        md,
        &[
            "--color=truecolor",
            "--images",
            "blocks",
            "--background",
            "dark",
        ],
        &[],
    );
    let light = show(
        md,
        &[
            "--color=truecolor",
            "--images",
            "blocks",
            "--background",
            "light",
        ],
        &[],
    );
    assert!(
        text(&dark).contains("38;2;40;200;80"),
        "the green disc: {}",
        text(&dark)
    );
    assert!(!text(&light).contains("38;2;40;200;80"), "{}", text(&light));
    assert!(text(&light).contains('▀'), "the gradient: {}", text(&light));
}

#[test]
fn the_gallery_in_blocks() {
    let gallery = fixtures().join("gallery.md");
    let o = emde(&[])
        .args(["--color=always", "--images", "blocks", "-w", "60", "-v"])
        .arg(&gallery)
        .output()
        .unwrap();
    assert!(o.status.success());
    let shown = text(&o).replace('\x1b', "\\e");
    // With the `svg` feature the logo is drawn; without it, its alt text.
    if cfg!(feature = "svg") {
        insta::assert_snapshot!("the_gallery_in_blocks_with_svg", shown);
    } else {
        insta::assert_snapshot!(shown);
    }
    let err = String::from_utf8_lossy(&o.stderr);
    let problems: Vec<&str> = err.lines().collect();
    let svg = cfg!(feature = "svg");
    assert_eq!(problems.len(), if svg { 5 } else { 6 }, "{err}");
    for what in [
        "missing.png:",
        "empty.png: not a PNG, JPEG, GIF or WebP image",
        "not-an-image.png: not a PNG",
        "remote.png: remote images are off",
        "truncated.png: cannot decode image",
    ] {
        assert!(err.contains(what), "{what}: {err}");
    }
    assert_eq!(
        err.contains("logo.svg: SVG images are not supported"),
        !svg,
        "{err}"
    );
}

/// The red 16×16 logo through every graphics path: drawn, never its alt
/// text.
#[cfg(feature = "svg")]
#[test]
fn svg_figures_through_every_path() {
    let logo = "![An SVG logo](logo.svg)";
    let shows = |args: &[&str], env: &[(&str, &str)]| {
        let mut all = vec!["--color=truecolor", "-w", "40"];
        all.extend_from_slice(args);
        let o = show(logo, &all, env);
        assert!(o.status.success());
        let out = text(&o);
        assert!(!unstyled(&out).contains("▣"), "{args:?}: {out:?}");
        out
    };
    // 16×16 px at 8×16 px cells: 2×1 cells of solid red (spaces on red);
    // sixel draws it at 12×12, the box in whole bands of six rows.
    let blocks = shows(&["--images", "blocks"], &[]);
    assert!(blocks.contains("\x1b[48;2;255;0;0m  \x1b[0m"), "{blocks:?}");
    let ghostty = [
        ("TERM_PROGRAM", "ghostty"),
        ("TERM_PROGRAM_VERSION", "1.3.1"),
    ];
    let kitty = shows(&["--images", "kitty"], &ghostty);
    let commands = kitty_commands(&kitty);
    assert_eq!(commands.len(), 1, "{kitty:?}");
    assert!(commands[0].contains(",f=100,t=d,c=2,r=1,q=2,m=0;iVBORw0KGgo"));
    let iterm = shows(&["--images", "iterm"], &[("TERM_PROGRAM", "iTerm.app")]);
    assert!(
        iterm.contains("\x1b]1337;File=inline=1;size=") && iterm.contains("width=2;height=1;"),
        "{iterm:?}"
    );
    if cfg!(feature = "sixel") {
        let sixel = shows(&["--images", "sixel"], &[]);
        assert!(
            sixel.contains("\x1bP") && sixel.contains("q\"1;1;12;12"),
            "{sixel:?}"
        );
    }
}

/// Serve `body` for `/ok.png` and 404 for anything else, on a local port,
/// for `requests` requests.
fn serve(body: Vec<u8>, requests: usize) -> u16 {
    serve_at("/ok.png", body, requests)
}

/// Serve `body` for `path` and 404 for anything else, on a local port, for
/// `requests` requests.
fn serve_at(path: &'static str, body: Vec<u8>, requests: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(requests) {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 2048];
            let n = stream.read(&mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..n]).into_owned();
            let response = if head.starts_with(&format!("GET {path} ")) {
                let mut r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                r.extend_from_slice(&body);
                r
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            };
            let _ = stream.write_all(&response);
        }
    });
    port
}

#[test]
fn remote_images_are_fetched_with_curl_when_allowed() {
    if Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_err()
    {
        return;
    }
    let port = serve(png(&gradient()), 2);
    let md = format!(
        "![remote](http://127.0.0.1:{port}/ok.png)\n\n![gone](http://127.0.0.1:{port}/gone.png)\n"
    );
    let doc = scratch_file("remote");
    std::fs::write(&doc, md).unwrap();
    let no_proxy = |cmd: &mut Command| {
        for var in [
            "http_proxy",
            "HTTP_PROXY",
            "https_proxy",
            "HTTPS_PROXY",
            "all_proxy",
            "ALL_PROXY",
        ] {
            cmd.env_remove(var);
        }
        cmd.env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1");
    };
    let mut cmd = emde(&[]);
    no_proxy(&mut cmd);
    let o = cmd
        .args([
            "--color=always",
            "--images",
            "blocks",
            "--remote-images",
            "-v",
        ])
        .arg(&doc)
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(out.matches('▀').count(), 16, "{out}");
    assert!(unstyled(&out).contains("▣ gone"), "{out}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("gone.png: could not be fetched with curl"),
        "{err}"
    );
    // Off by default: nothing is fetched.
    let mut cmd = emde(&[]);
    no_proxy(&mut cmd);
    let o = cmd
        .args(["--color=always", "--images", "blocks"])
        .arg(&doc)
        .output()
        .unwrap();
    assert!(unstyled(&text(&o)).contains("▣ remote"));
    let _ = std::fs::remove_file(&doc);
}

/// A badge served like shields.io's: an SVG at a URL without `.svg`, found
/// by its content, and only with `--remote-images`.
#[cfg(feature = "svg")]
#[test]
fn remote_svg_badges_are_recognised_by_their_content() {
    if Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_err()
    {
        return;
    }
    let badge = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"16\">\
                  <rect width=\"32\" height=\"16\" fill=\"#00ff00\"/></svg>"
        .to_vec();
    let port = serve_at("/badge/build-passing-green", badge, 1);
    let doc = scratch_file("badge");
    std::fs::write(
        &doc,
        format!("![build](http://127.0.0.1:{port}/badge/build-passing-green)\n"),
    )
    .unwrap();
    let mut cmd = emde(&[]);
    for var in ["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"] {
        cmd.env_remove(var);
    }
    let o = cmd
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .args([
            "--color=truecolor",
            "--images",
            "blocks",
            "--remote-images",
            "-v",
        ])
        .arg(&doc)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&doc);
    let out = text(&o);
    // 32×16 px: 4×1 cells of solid green.
    assert!(out.contains("\x1b[48;2;0;255;0m    \x1b[0m"), "{out:?}");
    assert!(
        o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The screen after `bytes` as a terminal shows them: `rows` × 40 cells,
/// the tty turning `\n` into `\r\n` (`ONLCR`).
fn screen(bytes: &[u8], rows: u16) -> Vec<String> {
    let mut cooked = Vec::with_capacity(bytes.len() + 64);
    for &b in bytes {
        if b == b'\n' {
            cooked.push(b'\r');
        }
        cooked.push(b);
    }
    let mut parser = vt100::Parser::new(rows, 40, 0);
    parser.process(&cooked);
    parser
        .screen()
        .contents()
        .split('\n')
        .map(|l| l.trim_end().to_owned())
        .collect()
}

#[test]
fn pixel_images_leave_the_text_after_them_in_place() {
    // The image arrives at the bottom of an 8-row screen, so reserving its
    // rows scrolls the screen; the caption and the text after it must still
    // land below the box, which stays blank for the image (vt100 ignores
    // the image itself).
    let md = "a\n\nb\n\nc\n\n![Gradient](gradient.png)\n\nafter";
    let mut cases = vec![
        ("iterm", vec![("TERM_PROGRAM", "iTerm.app")]),
        ("kitty", vec![("TERM", "xterm-kitty")]),
    ];
    if cfg!(feature = "sixel") {
        cases.push(("sixel", vec![]));
    }
    for (mode, env) in cases {
        let o = show(md, &["--color=always", "--images", mode, "-w", "40"], &env);
        let shown = screen(&o.stdout, 8);
        assert_eq!(
            shown,
            ["c", "", "", "", "              Gradient", "", "after"],
            "{mode}: {shown:?}"
        );
    }
    // Block images are text, for comparison.
    let o = show(
        md,
        &["--color=always", "--images", "blocks", "-w", "40"],
        &[],
    );
    let shown = screen(&o.stdout, 8);
    assert_eq!(shown[2].trim(), "▀▀▀▀▀▀▀▀", "{shown:?}");
    assert_eq!(shown[4].trim(), "Gradient");
}

/// `bytes` with every escape sequence that starts with `open` (up to and
/// including `close`) replaced by `with`.
fn replace_sequences(bytes: &[u8], open: &[u8], close: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(start) = rest.windows(open.len()).position(|w| w == open) {
        out.extend_from_slice(&rest[..start]);
        let after = &rest[start + open.len()..];
        let Some(end) = after.windows(close.len()).position(|w| w == close) else {
            break;
        };
        out.extend_from_slice(with);
        rest = &after[end + close.len()..];
    }
    out.extend_from_slice(rest);
    out
}

#[test]
fn pixel_images_survive_a_cursor_left_below_them() {
    // Some terminals leave the cursor on the row below an image. For a box
    // that ends on the screen's last row that scrolls the screen, so the
    // box and the row below it are reserved before the image is drawn: as
    // if each image were two linefeeds (the box is two rows), the text
    // after it must still land right below the box.
    let md = "a\n\nb\n\nc\n\n![Gradient](gradient.png)\n\nafter";
    let mut cases = vec![(
        "iterm",
        vec![("TERM_PROGRAM", "iTerm.app")],
        &b"\x1b]1337;"[..],
        &b"\x07"[..],
    )];
    if cfg!(feature = "sixel") {
        cases.push(("sixel", vec![], b"\x1bP", b"\x1b\\"));
    }
    for (mode, env, open, close) in cases {
        let o = show(md, &["--color=always", "--images", mode, "-w", "40"], &env);
        let moved = replace_sequences(&o.stdout, open, close, b"\n\n");
        assert_ne!(moved, o.stdout, "{mode}: the image was replaced");
        let shown = screen(&moved, 8);
        assert_eq!(
            shown,
            ["c", "", "", "", "              Gradient", "", "after"],
            "{mode}: {shown:?}"
        );
    }
}
