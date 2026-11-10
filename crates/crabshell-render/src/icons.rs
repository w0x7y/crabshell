//! Icons: SVG and PNG files and raw tray pixels, rasterized at the size
//! they are drawn at and cached.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crabshell_ui::{IconPixels, IconSource};
use resvg::usvg;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

/// How many rasterized icons stay cached. Past that, icons no draw used
/// within the last [`RECENT`] draws are dropped.
const CAPACITY: usize = 128;

/// How many draws an icon stays cached after its last use once the cache is
/// past [`CAPACITY`]. Several windows draw in turn, so this spans a few
/// frames of each.
const RECENT: u64 = 16;

/// The largest SVG file read, in bytes.
const MAX_SVG_BYTES: u64 = 1024 * 1024;

/// The largest PNG file read, in bytes.
const MAX_PNG_BYTES: u64 = 4 * 1024 * 1024;

/// The widest and tallest PNG decoded, in pixels.
const MAX_PNG_SIDE: u32 = 1024;

/// The widest and tallest raw tray pixmap drawn.
const MAX_PIXELS_SIDE: u32 = 512;

/// Rasterized icons, by source and size in physical pixels.
pub(crate) struct Icons {
    cache: HashMap<Key, Entry>,
    /// Counts draws, to tell which icons the latest one used.
    frame: u64,
    /// Sources that failed, so each is logged once.
    failed: HashSet<Key>,
    /// How many icons were rasterized, for tests.
    rasterized: usize,
}

/// What an icon was rasterized from, and at what size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    /// A name. Icon themes, where names are looked up, aren't read yet.
    Name {
        name: String,
        side: u32,
    },
    File {
        path: PathBuf,
        side: u32,
    },
    /// Raw pixels, by their shared allocation: reading the bytes on every
    /// draw would cost as much as rasterizing them.
    Pixels {
        data: Shared,
        width: u32,
        height: u32,
        side: u32,
    },
}

/// Tray pixel data, compared and hashed by its address. The key holds a
/// reference, so the address can't be reused by other data while cached.
#[derive(Clone)]
struct Shared(Rc<[u8]>);

impl PartialEq for Shared {
    fn eq(&self, other: &Shared) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Shared {}

impl Hash for Shared {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Rc::as_ptr(&self.0) as *const u8 as usize, self.0.len()).hash(state);
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} bytes", self.0.len())
    }
}

struct Entry {
    /// `None` if the icon couldn't be loaded; drawn as nothing.
    pixmap: Option<Pixmap>,
    used: u64,
}

impl Icons {
    pub(crate) fn new() -> Icons {
        Icons {
            cache: HashMap::new(),
            frame: 0,
            failed: HashSet::new(),
            rasterized: 0,
        }
    }

    /// The number of rasterized icons held, for tests.
    #[cfg(test)]
    pub(crate) fn cached(&self) -> usize {
        self.cache.len()
    }

    /// The number of times an icon was rasterized, for tests.
    #[cfg(test)]
    pub(crate) fn rasterized(&self) -> usize {
        self.rasterized
    }

    /// The icon `source` as a `side` by `side` pixmap, rasterized on first
    /// use. `None` if it can't be loaded; that is logged once.
    pub(crate) fn get(&mut self, source: &IconSource, side: u32) -> Option<&Pixmap> {
        let key = match source {
            IconSource::Name(name) => Key::Name {
                name: name.clone(),
                side,
            },
            IconSource::File(path) => Key::File {
                path: path.clone(),
                side,
            },
            IconSource::Pixels(pixels) => Key::Pixels {
                data: Shared(Rc::clone(&pixels.data)),
                width: pixels.width,
                height: pixels.height,
                side,
            },
        };
        let frame = self.frame;
        if !self.cache.contains_key(&key) {
            self.rasterized += 1;
            let rasterized = match source {
                IconSource::Name(name) => Err(format!("no icon theme to look {name:?} up in")),
                IconSource::File(path) => rasterize_file(path, side),
                IconSource::Pixels(pixels) => from_argb(pixels).and_then(|image| fit(&image, side)),
            };
            let pixmap = match rasterized {
                Ok(pixmap) => Some(pixmap),
                Err(error) => {
                    if self.failed.len() > CAPACITY {
                        self.failed.clear();
                    }
                    if self.failed.insert(key.clone()) {
                        tracing::warn!(icon = %describe(source), %error, "couldn't draw an icon");
                    }
                    None
                }
            };
            self.cache.insert(
                key.clone(),
                Entry {
                    pixmap,
                    used: frame,
                },
            );
        }
        let entry = self.cache.get_mut(&key)?;
        entry.used = frame;
        entry.pixmap.as_ref()
    }

    /// Ends a draw: past [`CAPACITY`], icons no draw used within the last
    /// [`RECENT`] are dropped.
    pub(crate) fn end_frame(&mut self) {
        if self.cache.len() > CAPACITY {
            let oldest = self.frame.saturating_sub(RECENT - 1);
            self.cache.retain(|_, entry| entry.used >= oldest);
        }
        self.frame += 1;
    }
}

/// A short description of `source` for log lines; raw pixels aren't
/// printed.
fn describe(source: &IconSource) -> String {
    match source {
        IconSource::Name(name) => format!("name {name:?}"),
        IconSource::File(path) => format!("file {}", path.display()),
        IconSource::Pixels(pixels) => format!("{}x{} pixels", pixels.width, pixels.height),
    }
}

/// Rasterizes an SVG or PNG file, told apart by its extension. Icon files
/// can come from other programs, so only regular files under a size cap are
/// read.
fn rasterize_file(path: &Path, side: u32) -> Result<Pixmap, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("svg") => rasterize_svg(&read_capped(path, MAX_SVG_BYTES)?, side),
        Some("png") => fit(&decode_png(&read_capped(path, MAX_PNG_BYTES)?)?, side),
        _ => Err(String::from("only .svg and .png files can be icons")),
    }
}

/// Reads a regular file of at most `cap` bytes. A pipe or device would
/// block or never end, so it is refused before it is opened.
pub(crate) fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(String::from("not a regular file"));
    }
    if metadata.len() > cap {
        return Err(format!(
            "the file is {} bytes, more than {cap}",
            metadata.len()
        ));
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    read_limited(file, cap)
}

/// Reads at most `cap` bytes from `reader`, and fails if there is more. A
/// file can grow after its size was checked, and files in `/proc` report
/// no size at all.
pub(crate) fn read_limited(reader: impl Read, cap: u64) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(cap + 1)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    if data.len() as u64 > cap {
        return Err(format!("more than {cap} bytes"));
    }
    Ok(data)
}

fn rasterize_svg(data: &[u8], side: u32) -> Result<Pixmap, String> {
    // An `<image>` naming a file would read anything on the machine, pipes
    // included, so only `data:` URLs are loaded.
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(data, &options).map_err(|error| error.to_string())?;
    // A filter can take hours to render at a tiny size, and icons don't
    // need them. This covers masks, patterns and embedded SVG images too.
    if !tree.filters().is_empty() {
        return Err(String::from("the SVG uses a filter, which icons may not"));
    }
    let size = tree.size();
    let mut pixmap = empty(side)?;
    let scale = side as f32 / size.width().max(size.height());
    let transform = Transform::from_scale(scale, scale).post_translate(
        (side as f32 - size.width() * scale) / 2.0,
        (side as f32 - size.height() * scale) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Decodes a PNG into a premultiplied pixmap of its own size.
fn decode_png(data: &[u8]) -> Result<Pixmap, String> {
    // Every row is checked against these too, so a lying header can't make
    // the decoder allocate more.
    let limits = png::Limits {
        bytes: (MAX_PNG_SIDE * MAX_PNG_SIDE * 8) as usize,
    };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(data), limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width > MAX_PNG_SIDE || height > MAX_PNG_SIDE {
        return Err(format!(
            "a {width}x{height} PNG is larger than {MAX_PNG_SIDE} pixels a side"
        ));
    }
    let size = reader
        .output_buffer_size()
        .ok_or("the PNG is too large to decode")?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| error.to_string())?;
    let rows = buffer
        .chunks_exact(info.line_size)
        .take(info.height as usize);
    let mut rgba = Vec::with_capacity(info.width as usize * info.height as usize * 4);
    for row in rows {
        match info.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(&row[..info.width as usize * 4]),
            png::ColorType::Rgb => {
                for pixel in row.as_chunks::<3>().0.iter().take(info.width as usize) {
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
            png::ColorType::GrayscaleAlpha => {
                for pixel in row.as_chunks::<2>().0.iter().take(info.width as usize) {
                    rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
                }
            }
            png::ColorType::Grayscale => {
                for &gray in row.iter().take(info.width as usize) {
                    rgba.extend_from_slice(&[gray, gray, gray, 255]);
                }
            }
            png::ColorType::Indexed => return Err(String::from("unexpanded palette PNG")),
        }
    }
    premultiplied(rgba, info.width, info.height)
}

/// Raw tray pixels, ARGB in network byte order, as a pixmap.
fn from_argb(pixels: &IconPixels) -> Result<Pixmap, String> {
    if pixels.width > MAX_PIXELS_SIDE || pixels.height > MAX_PIXELS_SIDE {
        return Err(format!(
            "{}x{} pixels are larger than {MAX_PIXELS_SIDE} a side",
            pixels.width, pixels.height
        ));
    }
    let expected = u64::from(pixels.width) * u64::from(pixels.height) * 4;
    if pixels.data.len() as u64 != expected {
        return Err(format!(
            "{}x{} pixels need {expected} bytes, got {}",
            pixels.width,
            pixels.height,
            pixels.data.len()
        ));
    }
    let rgba = pixels
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|argb| [argb[1], argb[2], argb[3], argb[0]])
        .collect();
    premultiplied(rgba, pixels.width, pixels.height)
}

/// A pixmap from straight-alpha RGBA bytes.
fn premultiplied(mut rgba: Vec<u8>, width: u32, height: u32) -> Result<Pixmap, String> {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    let size = tiny_skia::IntSize::from_wh(width, height).ok_or("an icon with no pixels")?;
    Pixmap::from_vec(rgba, size).ok_or_else(|| String::from("icon pixels of the wrong length"))
}

/// Scales `image` to fit a `side` by `side` pixmap, centered. An image more
/// than twice the size is first halved, averaging each 2 by 2 block, so
/// thin details shrink to faint lines instead of vanishing or flickering.
fn fit(image: &Pixmap, side: u32) -> Result<Pixmap, String> {
    let mut halved;
    let mut image = image;
    while image.width().max(image.height()) > side * 2 {
        halved = halve(image)?;
        image = &halved;
    }
    let mut pixmap = empty(side)?;
    let scale = side as f32 / image.width().max(image.height()) as f32;
    let transform = Transform::from_scale(scale, scale).post_translate(
        (side as f32 - image.width() as f32 * scale) / 2.0,
        (side as f32 - image.height() as f32 * scale) / 2.0,
    );
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, transform, None);
    Ok(pixmap)
}

/// `image` at half its size, rounded up, each pixel the average of the 2 by
/// 2 block it covers. Premultiplied pixels average correctly.
fn halve(image: &Pixmap) -> Result<Pixmap, String> {
    let (width, height) = (image.width(), image.height());
    let (half_width, half_height) = (width.div_ceil(2), height.div_ceil(2));
    let mut half = Pixmap::new(half_width, half_height).ok_or("an icon with no pixels")?;
    let source = image.data();
    let target = half.data_mut();
    for y in 0..half_height {
        for x in 0..half_width {
            let mut sum = [0_u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                // At an odd edge, the last pixel stands in for the missing one.
                let sx = (x * 2 + dx).min(width - 1);
                let sy = (y * 2 + dy).min(height - 1);
                let index = (sy * width + sx) as usize * 4;
                for (total, &value) in sum.iter_mut().zip(&source[index..index + 4]) {
                    *total += u32::from(value);
                }
            }
            let index = (y * half_width + x) as usize * 4;
            for (value, total) in target[index..index + 4].iter_mut().zip(sum) {
                *value = ((total + 2) / 4) as u8;
            }
        }
    }
    Ok(half)
}

fn empty(side: u32) -> Result<Pixmap, String> {
    Pixmap::new(side, side).ok_or_else(|| String::from("an icon with no pixels"))
}
