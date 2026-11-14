//! Tests for the renderer, by area. Each test runs on its own thread, so
//! each starts with an empty reactive runtime.
//!
//! Golden images live in `tests/golden`. A test compares what it draws with
//! its image, allowing 2/255 per channel. To write the images again, on
//! purpose, run the tests with `CRABSHELL_BLESS=1` and look at every changed
//! image before committing it. They depend on this machine's fonts:
//! `sans-serif` is Noto Sans, and DejaVu Sans and DejaVu Sans Bold supply
//! the snowman.

mod colors;
mod damage;
mod glyphs;
mod golden;
mod icons;
mod images;
mod repaint;
mod shapes;
mod theme;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crabshell_reactive::Runtime;
use crabshell_ui::{Cx, Output, PanelWindow, Size, TextSystem, Window};

use crate::{BufferAge, Canvas, DamageHistory, PixelFormat, PixelRect, Renderer};

/// A runtime, a root context, a text system and a renderer, with helpers to
/// build windows and draw them the way the platform does.
struct Scene {
  runtime: Runtime,
  cx: Cx,
  text: TextSystem,
  renderer: Renderer,
}

/// A buffer, with the frame it last held.
#[derive(Clone)]
struct Buffer {
  pixels: Vec<u8>,
  width: u32,
  height: u32,
  scale: f32,
  age: BufferAge,
}

impl Buffer {
  /// A new buffer for `window` at `scale`, all transparent.
  fn new(window: &Window, scale: f32) -> Buffer {
    let size = window.size();
    let width = (size.width * scale).ceil() as u32;
    let height = (size.height * scale).ceil() as u32;
    Buffer {
      pixels: vec![0; width as usize * height as usize * 4],
      width,
      height,
      scale,
      age: BufferAge::default(),
    }
  }

  /// The RGBA bytes of the pixel at `x`, `y`.
  fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
    let index = (y * self.width + x) as usize * 4;
    let mut pixel = [0; 4];
    pixel.copy_from_slice(&self.pixels[index..index + 4]);
    pixel
  }
}

impl Scene {
  fn new() -> Scene {
    let runtime = Runtime::current();
    Scene {
      runtime,
      cx: Cx::new(runtime.new_owner()),
      text: TextSystem::new(),
      renderer: Renderer::new(),
    }
  }

  /// A panel on output "DP-1".
  fn panel(&self) -> PanelWindow {
    PanelWindow::new(&Output::new("DP-1"))
  }

  /// Builds `panel` at `width` by `height`, runs the bindings and lays it
  /// out.
  fn window(&mut self, panel: PanelWindow, width: f32, height: f32) -> Window {
    let window = Window::new(&self.cx, panel);
    window.set_size(Size::new(width, height));
    self.frame(&window);
    window
  }

  /// One event-loop iteration: flush, then layout if needed.
  fn frame(&mut self, window: &Window) {
    self.runtime.flush();
    if window.needs_layout() {
      window.layout(&mut self.text);
    }
  }

  /// Draws the window's damage into `buffer`, which belongs to `history`,
  /// in RGBA order.
  fn draw(
    &mut self,
    window: &Window,
    history: &mut DamageHistory,
    buffer: &mut Buffer,
  ) -> Vec<PixelRect> {
    let damage = window.take_damage();
    self
      .renderer
      .draw(
        &window.view(),
        &damage,
        &mut self.text,
        history,
        Canvas {
          pixels: &mut buffer.pixels,
          width: buffer.width,
          height: buffer.height,
          scale: buffer.scale,
          format: PixelFormat::Abgr8888,
          age: &mut buffer.age,
        },
      )
      .expect("the test's canvas fits")
  }

  /// The whole window drawn into a new buffer at `scale`, leaving its
  /// damage for other draws.
  fn full(&mut self, window: &Window, scale: f32) -> Buffer {
    let mut buffer = Buffer::new(window, scale);
    self
      .renderer
      .draw(
        &window.view(),
        &[],
        &mut self.text,
        &mut DamageHistory::new(),
        Canvas {
          pixels: &mut buffer.pixels,
          width: buffer.width,
          height: buffer.height,
          scale,
          format: PixelFormat::Abgr8888,
          age: &mut buffer.age,
        },
      )
      .expect("the test's canvas fits");
    buffer
  }
}

/// The node at `path` under `root`, as a list of child indexes.
fn node_at<'a>(root: crabshell_ui::NodeView<'a>, path: &[usize]) -> crabshell_ui::NodeView<'a> {
  path.iter().fold(root, |node, &index| {
    node
      .children()
      .nth(index)
      .unwrap_or_else(|| panic!("{} has no child {index}", node.path()))
  })
}

/// A file in `tests/fixtures`.
fn fixture(name: &str) -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures")
    .join(name)
}

/// Checks `buffer` against `tests/golden/<name>.png`, allowing 2/255 per
/// premultiplied channel, or writes the image when `CRABSHELL_BLESS` is
/// set. On a mismatch the drawn image is saved under
/// `target/golden-failures`. Images are stored with straight alpha, as PNG
/// expects, and premultiplied again to compare, so faint pixels aren't
/// held to a tighter tolerance than the renderer's own rounding.
fn assert_golden(name: &str, buffer: &Buffer) {
  let straight = demultiplied(&buffer.pixels);
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/golden")
    .join(format!("{name}.png"));
  if std::env::var_os("CRABSHELL_BLESS").is_some() {
    write_png(&path, &straight, buffer.width, buffer.height);
    return;
  }
  let Some((expected, width, height)) = read_png(&path) else {
    panic!(
      "no golden image {}; run with CRABSHELL_BLESS=1 to write it",
      path.display()
    );
  };
  let failure = if (width, height) != (buffer.width, buffer.height) {
    Some(format!(
      "size {}x{}, golden {width}x{height}",
      buffer.width, buffer.height
    ))
  } else {
    let mut differing = 0;
    let mut worst = 0;
    let expected = premultiplied(&expected);
    for (drawn, golden) in buffer
      .pixels
      .as_chunks::<4>()
      .0
      .iter()
      .zip(expected.as_chunks::<4>().0)
    {
      let difference = drawn
        .iter()
        .zip(golden)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
      if difference > 2 {
        differing += 1;
      }
      worst = worst.max(difference);
    }
    (differing > 0)
      .then(|| format!("{differing} pixels differ by more than 2/255, the most by {worst}"))
  };
  if let Some(failure) = failure {
    let saved = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../../target/golden-failures")
      .join(format!("{name}.png"));
    write_png(&saved, &straight, buffer.width, buffer.height);
    panic!(
      "{name} doesn't match its golden image: {failure}; drawn image saved to {}",
      saved.display()
    );
  }
}

/// Straight RGBA, as PNG stores it, premultiplied.
fn premultiplied(straight: &[u8]) -> Vec<u8> {
  let mut pixels = straight.to_vec();
  for pixel in pixels.as_chunks_mut::<4>().0 {
    let alpha = u16::from(pixel[3]);
    for channel in &mut pixel[..3] {
      *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
    }
  }
  pixels
}

/// Premultiplied RGBA as straight RGBA, as PNG stores it.
fn demultiplied(premultiplied: &[u8]) -> Vec<u8> {
  let mut straight = premultiplied.to_vec();
  for pixel in straight.as_chunks_mut::<4>().0 {
    let alpha = u16::from(pixel[3]);
    for channel in &mut pixel[..3] {
      if let Some(value) = (u16::from(*channel) * 255 + alpha / 2).checked_div(alpha) {
        *channel = value.min(255) as u8;
      }
    }
  }
  straight
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).expect("create the image's directory");
  }
  let file = std::fs::File::create(path).expect("create the image file");
  let mut encoder = png::Encoder::new(io::BufWriter::new(file), width, height);
  encoder.set_color(png::ColorType::Rgba);
  encoder.set_depth(png::BitDepth::Eight);
  let mut writer = encoder.write_header().expect("write the PNG header");
  writer.write_image_data(rgba).expect("write the PNG pixels");
}

/// An RGBA PNG's pixels and size, or `None` if there is no such file.
fn read_png(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
  let file = std::fs::File::open(path).ok()?;
  let decoder = png::Decoder::new(io::BufReader::new(file));
  let mut reader = decoder.read_info().expect("a golden image is a PNG");
  let mut pixels = vec![0; reader.output_buffer_size().expect("a small image")];
  let info = reader
    .next_frame(&mut pixels)
    .expect("a golden image decodes");
  assert_eq!(
    info.color_type,
    png::ColorType::Rgba,
    "golden images are RGBA"
  );
  pixels.truncate(info.width as usize * info.height as usize * 4);
  Some((pixels, info.width, info.height))
}

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
  #[derive(Clone, Default)]
  struct Logs(Arc<Mutex<Vec<u8>>>);

  impl io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self
        .0
        .lock()
        .expect("log buffer lock")
        .extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = logs.0.lock().expect("log buffer lock").clone();
  String::from_utf8(bytes).expect("log output is UTF-8")
}
