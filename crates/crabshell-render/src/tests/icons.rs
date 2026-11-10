//! The icon cache, and icons that can't be drawn.

use std::rc::Rc;

use crabshell_ui::{Icon, IconPixels, IconSource, Row};

use super::{Scene, capture_logs, fixture};
use crate::icons::Icons;

#[test]
fn an_icon_is_rasterized_once_per_size() {
    let mut scene = Scene::new();
    let row = Row::new()
        .child(Icon::new(fixture("shapes.svg").as_path()).size(16))
        .child(Icon::new(fixture("shapes.svg").as_path()).size(16))
        .child(Icon::new(fixture("shapes.svg").as_path()).size(20));
    let window = scene.window(scene.panel().child(row), 100.0, 24.0);
    scene.full(&window, 1.0);
    assert_eq!(scene.renderer.cached_icons(), 2);
    scene.full(&window, 1.0);
    assert_eq!(scene.renderer.cached_icons(), 2);
    assert_eq!(scene.renderer.rasterized_icons(), 2);
    // At another scale the icons are other sizes in pixels.
    scene.full(&window, 2.0);
    assert_eq!(scene.renderer.cached_icons(), 4);
    assert_eq!(scene.renderer.rasterized_icons(), 4);
}

/// A one-pixel icon; each call makes new pixel data.
fn pixels(seed: u8) -> IconSource {
    IconSource::Pixels(IconPixels {
        width: 1,
        height: 1,
        data: Rc::from(&[255, seed, 0, 0][..]),
    })
}

#[test]
fn past_its_capacity_the_cache_drops_icons_unused_for_16_draws() {
    let mut icons = Icons::new();
    let sources: Vec<IconSource> = (0..=200).map(pixels).collect();
    for source in &sources {
        icons.get(source, 8);
    }
    icons.end_frame();
    // Fifteen more draws use only one icon: everything was used recently.
    for _ in 1..16 {
        icons.get(&sources[7], 8);
        icons.end_frame();
    }
    assert_eq!(icons.cached(), 201);
    // The sixteenth drops the rest.
    icons.get(&sources[7], 8);
    icons.end_frame();
    assert_eq!(icons.cached(), 1);
}

#[test]
fn an_icon_that_fails_is_logged_once_and_draws_nothing() {
    let mut scene = Scene::new();
    let row = Row::new()
        .child(Icon::new(fixture("missing.svg").as_path()))
        // A file that exists but is neither SVG nor PNG.
        .child(Icon::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        ))
        .child(Icon::new(IconSource::Pixels(IconPixels {
            width: 4,
            height: 4,
            data: Rc::from(&[0; 5][..]),
        })));
    let window = scene.window(scene.panel().child(row), 100.0, 24.0);
    let logs = capture_logs(|| {
        let first = scene.full(&window, 1.0);
        scene.full(&window, 1.0);
        assert!(first.pixels.iter().all(|&byte| byte == 0), "nothing drawn");
    });
    assert_eq!(logs.matches("couldn't draw an icon").count(), 3, "{logs}");
    assert!(logs.contains("missing.svg"), "{logs}");
    assert!(logs.contains("only .svg and .png files"), "{logs}");
    assert!(logs.contains("4x4 pixels need 64 bytes, got 5"), "{logs}");
}

#[test]
fn a_failed_icon_dropped_from_the_cache_is_not_logged_again() {
    let bad = IconSource::Pixels(IconPixels {
        width: 2,
        height: 2,
        data: Rc::from(&[0; 3][..]),
    });
    let others: Vec<IconSource> = (0..=200).map(pixels).collect();
    let mut icons = Icons::new();
    let logs = capture_logs(|| {
        assert!(icons.get(&bad, 8).is_none());
        icons.end_frame();
        // Fill the cache past its capacity with other icons and draw them
        // until the failed one is dropped, then ask for it again.
        for _ in 0..17 {
            for other in &others {
                icons.get(other, 8);
            }
            icons.end_frame();
        }
        assert!(icons.get(&bad, 8).is_none());
    });
    assert_eq!(icons.rasterized(), 203);
    assert_eq!(logs.matches("couldn't draw an icon").count(), 1, "{logs}");
}

#[test]
fn drawing_drops_icons_no_recent_draw_used() {
    let mut scene = Scene::new();
    let shown = scene.cx.signal((0..=200).collect::<Vec<u8>>());
    let row = Row::new().children_keyed(
        move || shown.get(),
        |seed| *seed,
        |_cx, seed| Icon::new(pixels(seed.get_untracked())).size(1),
    );
    let window = scene.window(scene.panel().child(row), 300.0, 4.0);
    scene.full(&window, 1.0);
    assert_eq!(scene.renderer.cached_icons(), 201);
    shown.set(vec![3]);
    scene.frame(&window);
    for _ in 0..16 {
        scene.full(&window, 1.0);
    }
    assert_eq!(scene.renderer.cached_icons(), 1);
}

/// A fresh scratch directory for one test's files.
fn scratch(test: &str) -> std::path::PathBuf {
    let directory =
        std::env::temp_dir().join(format!("crabshell-render-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create a scratch directory");
    directory
}

/// Rasterizes the icon `source` makes, on another thread, at 16 pixels.
/// `None` if it hasn't finished in 3 seconds; otherwise whether it drew and
/// what it logged.
fn rasterize_briefly(
    source: impl FnOnce() -> IconSource + Send + 'static,
) -> Option<(bool, String)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let source = source();
        let mut drawn = false;
        let logs = capture_logs(|| {
            let mut icons = Icons::new();
            drawn = icons.get(&source, 16).is_some();
        });
        let _ = sender.send((drawn, logs));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(3))
        .ok()
}

fn file(path: &std::path::Path) -> IconSource {
    IconSource::File(path.to_owned())
}

#[test]
fn only_regular_files_under_the_size_cap_are_read() {
    let directory = scratch("hostile");
    let fifo = directory.join("pipe.png");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    assert!(
        made.is_ok_and(|status| status.success()),
        "mkfifo works here"
    );
    let zero = directory.join("zero.svg");
    std::os::unix::fs::symlink("/dev/zero", &zero).expect("make a symlink");
    let big = directory.join("big.png");
    std::fs::write(&big, vec![0; 4 * 1024 * 1024 + 1]).expect("write a big file");
    let big_svg = directory.join("big.svg");
    std::fs::write(&big_svg, vec![b' '; 1024 * 1024 + 1]).expect("write a big file");

    for (path, reason) in [
        (&fifo, "not a regular file"),
        (&zero, "not a regular file"),
        // Refused by size before anything is read.
        (&big, "is 4194305 bytes, more than 4194304"),
        (&big_svg, "is 1048577 bytes, more than 1048576"),
    ] {
        let moved = path.clone();
        let (drawn, logs) = rasterize_briefly(move || file(&moved)).expect("it returned quickly");
        assert!(!drawn, "{}", path.display());
        assert!(logs.contains(reason), "{}: {logs}", path.display());
    }
}

#[test]
fn reading_stops_one_byte_past_the_cap() {
    use std::cell::Cell;
    use std::io::{self, Read};

    /// Endless zeros, counting how many were read. It fails far past the
    /// cap, so a read that ignored the cap ends.
    struct Endless(Rc<Cell<u64>>);

    impl Read for Endless {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0.get() > 1024 * 1024 {
                return Err(io::Error::other("read on past the cap"));
            }
            buffer.fill(0);
            self.0.set(self.0.get() + buffer.len() as u64);
            Ok(buffer.len())
        }
    }

    let given = Rc::new(Cell::new(0));
    let read = crate::icons::read_limited(Endless(Rc::clone(&given)), 100);
    assert_eq!(read, Err(String::from("more than 100 bytes")));
    assert_eq!(given.get(), 101);
    let read = crate::icons::read_limited(&[7_u8; 100][..], 100);
    assert_eq!(read, Ok(vec![7; 100]));
}

#[test]
fn an_svg_image_link_loads_nothing() {
    let directory = scratch("href");
    let fifo = directory.join("pipe.png");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    assert!(
        made.is_ok_and(|status| status.success()),
        "mkfifo works here"
    );
    let svg = |href: &str| {
        format!(
            "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>\
             <rect width='4' height='4' fill='red'/>\
             <image href='{href}' width='16' height='16'/></svg>"
        )
    };
    for (name, href) in [
        ("fifo.svg", fifo.display().to_string()),
        ("png.svg", fixture("disc.png").display().to_string()),
        ("svg.svg", fixture("shapes.svg").display().to_string()),
    ] {
        let path = directory.join(name);
        std::fs::write(&path, svg(&href)).expect("write the SVG");
        let mut icons = Icons::new();
        let moved = path.clone();
        let returned = rasterize_briefly(move || file(&moved));
        assert!(
            returned.is_some_and(|(drawn, _)| drawn),
            "{name} returned and drew"
        );
        // Only the red square is drawn; the linked file isn't read.
        let pixmap = icons.get(&file(&path), 16).expect("drawn").clone();
        let ink = pixmap
            .pixels()
            .iter()
            .filter(|pixel| pixel.alpha() > 0)
            .count();
        assert_eq!(ink, 16, "{name}");
    }
}

#[test]
fn an_svg_with_a_filter_is_refused() {
    let directory = scratch("filters");
    let spin = "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>\
        <filter id='f'><feTurbulence baseFrequency='0.5' numOctaves='2000000000'/></filter>\
        <rect width='16' height='16' filter='url(#f)'/></svg>";
    // The same filter inside an SVG image embedded as a data URL.
    let nested = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>\
         <image width='16' height='16' href='data:image/svg+xml;utf8,{}'/></svg>",
        spin.replace('<', "%3C")
            .replace('>', "%3E")
            .replace('#', "%23")
            .replace('\'', "%27")
            .replace(' ', "%20")
    );
    for (name, text) in [("spin.svg", spin.to_owned()), ("nested.svg", nested)] {
        let path = directory.join(name);
        std::fs::write(&path, text).expect("write the SVG");
        let moved = path.clone();
        let (drawn, logs) = rasterize_briefly(move || file(&moved)).expect("it returned quickly");
        assert!(!drawn, "{name}");
        assert!(logs.contains("uses a filter"), "{name}: {logs}");
    }
}

/// Writes an all-transparent RGBA PNG of `width` by `height`.
fn write_png(path: &std::path::Path, width: u32, height: u32) {
    write_rgba(
        path,
        width,
        height,
        &vec![0; width as usize * height as usize * 4],
    );
}

/// Writes straight-alpha `rgba` as a PNG of `width` by `height`.
fn write_rgba(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("create the PNG");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("write the PNG header");
    writer.write_image_data(rgba).expect("write the PNG pixels");
    writer.finish().expect("finish the PNG");
}

#[test]
fn a_png_wider_or_taller_than_1024_is_refused() {
    let directory = scratch("dimensions");
    for (width, height, allowed) in [(1024, 1, true), (1025, 1, false), (1, 1025, false)] {
        let path = directory.join(format!("{width}x{height}.png"));
        write_png(&path, width, height);
        let moved = path.clone();
        let (drawn, logs) = rasterize_briefly(move || file(&moved)).expect("it returned quickly");
        assert_eq!(drawn, allowed, "{width}x{height}");
        if !allowed {
            assert!(logs.contains(&format!("{width}x{height}")), "{logs}");
        }
    }
}

#[test]
fn a_large_png_is_averaged_down_not_point_sampled() {
    let directory = scratch("alias");
    let mut totals = Vec::new();
    for bar in [100_u32, 104, 108, 112] {
        // A 4-pixel white bar on a 256-pixel icon covers a quarter of one
        // column of the 16-pixel result, wherever it falls.
        let side = 256;
        let mut rgba = vec![0_u8; side * side * 4];
        for y in 0..side {
            for x in bar as usize..bar as usize + 4 {
                rgba[(y * side + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let path = directory.join(format!("bar{bar}.png"));
        write_rgba(&path, side as u32, side as u32, &rgba);
        let mut icons = Icons::new();
        let pixmap = icons.get(&file(&path), 16).expect("drawn").clone();
        let row: u32 = (0..16)
            .map(|x| {
                pixmap
                    .pixel(x, 8)
                    .map_or(0, |pixel| u32::from(pixel.alpha()))
            })
            .sum();
        totals.push(row);
    }
    let (low, high) = (totals.iter().min(), totals.iter().max());
    assert!(
        high.zip(low)
            .is_some_and(|(high, low)| high - low <= 16 && *low > 32),
        "ink per row: {totals:?}"
    );
}

#[test]
fn halving_rounds_to_the_nearest_value() {
    // Opaque and almost opaque white in a checkerboard average to 254.5,
    // which rounds up to fully opaque. Rounding down would fade the icon a
    // little more with every halving.
    let side = 64;
    let rgba: Vec<u8> = (0..side * side)
        .flat_map(|index| {
            let alpha = if (index % side + index / side) % 2 == 0 {
                255
            } else {
                254
            };
            [255, 255, 255, alpha]
        })
        .collect();
    let path = scratch("rounding").join("checkerboard.png");
    write_rgba(&path, side, side, &rgba);
    let mut icons = Icons::new();
    let pixmap = icons.get(&file(&path), 16).expect("drawn").clone();
    let center = pixmap.pixel(8, 8).map(|pixel| pixel.alpha());
    assert_eq!(center, Some(255));
}

#[test]
fn tray_pixels_over_512_are_refused() {
    for (width, allowed) in [(512, true), (513, false)] {
        let source = move || {
            IconSource::Pixels(IconPixels {
                width,
                height: 1,
                data: Rc::from(vec![255; width as usize * 4]),
            })
        };
        let (drawn, logs) = rasterize_briefly(source).expect("it returned quickly");
        assert_eq!(drawn, allowed, "{width}x1");
        if !allowed {
            assert!(logs.contains("513x1"), "{logs}");
        }
    }
}

#[test]
fn tray_pixels_are_cached_by_their_shared_data() {
    let data: Rc<[u8]> = Rc::from(&[255, 1, 2, 3][..]);
    let source = |data: Rc<[u8]>| {
        IconSource::Pixels(IconPixels {
            width: 1,
            height: 1,
            data,
        })
    };
    let mut icons = Icons::new();
    icons.get(&source(Rc::clone(&data)), 8);
    icons.get(&source(Rc::clone(&data)), 8);
    assert_eq!(icons.rasterized(), 1);
    // Equal bytes in another allocation are another icon: the cache doesn't
    // read the bytes to tell.
    icons.get(&source(Rc::from(&[255, 1, 2, 3][..])), 8);
    assert_eq!(icons.rasterized(), 2);
}

#[test]
fn icons_used_by_any_recent_draw_stay_cached() {
    // Two windows with 70 icons each: 140, past the cache's 128, but each
    // is drawn every other frame.
    let mut scene = Scene::new();
    let row = |seeds: std::ops::Range<u16>| {
        seeds.fold(Row::new(), |row, seed| {
            row.child(
                Icon::new(IconSource::Pixels(IconPixels {
                    width: 1,
                    height: 1,
                    data: Rc::from(&[255, (seed % 256) as u8, (seed / 256) as u8, 7][..]),
                }))
                .size(2),
            )
        })
    };
    let a = scene.window(scene.panel().child(row(0..70)), 300.0, 4.0);
    let b = scene.window(scene.panel().child(row(1000..1070)), 300.0, 4.0);
    for _ in 0..3 {
        scene.full(&a, 1.0);
        scene.full(&b, 1.0);
    }
    assert_eq!(scene.renderer.rasterized_icons(), 140);
}
