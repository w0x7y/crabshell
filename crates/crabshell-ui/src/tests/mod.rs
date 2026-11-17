//! Tests for the widget tree, by area. Each test runs on its own thread, so
//! each starts with an empty reactive runtime.
//!
//! Layout tests that need exact numbers use icons, whose size is exact.
//! Text tests compare sizes with each other instead, since the font
//! fontconfig picks depends on the machine.

mod animation;
mod canvas;
mod displaced;
mod drop;
mod edit;
mod elided;
mod floating;
mod flow;
mod geometry;
mod grid;
mod grid_view;
mod image;
mod image_transition;
mod input;
mod invalidation;
mod keyboard;
mod keyed;
mod kinetic;
mod layout;
mod links;
mod lists;
mod lock;
mod markup;
mod measure;
mod menu;
mod placement;
mod pointer;
mod popup;
mod primary;
mod region;
mod rich;
mod screencopy;
mod scroll;
mod select;
mod services;
mod slider;
mod style;
mod text;
mod text_area;
mod text_input;
mod text_styles;
mod timer;
mod transform;
mod two_axes;
mod visible;
mod window;

use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crabshell_reactive::Runtime;

use crate::{
  Cx, NodeView, Output, PanelWindow, Rect, ScrollSource, Size, TextSystem, Wheel, WheelAxis, Window,
};

/// A runtime, a root context and a text system, with helpers to build and
/// lay out windows the way the platform does.
struct Harness {
  runtime: Runtime,
  cx: Cx,
  text: TextSystem,
}

impl Harness {
  fn new() -> Harness {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    // As the platform does, so `cx.measure_text` uses the same fonts.
    let text = cx.provide(TextSystem::new()).as_ref().clone();
    Harness { runtime, cx, text }
  }

  /// A panel on output "DP-1", for `panel` to finish.
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
}

/// A mouse wheel turned by `value120` 120ths of a notch, down when
/// positive.
fn wheel(value120: i32) -> Wheel {
  Wheel {
    source: ScrollSource::Wheel,
    x: WheelAxis::default(),
    y: WheelAxis {
      pixels: value120 as f32 / 8.0,
      notches: value120 as f32 / 120.0,
      stop: false,
    },
  }
}

/// Fingers on a touchpad scrolling `pixels` down.
fn pixels(pixels: f32) -> Wheel {
  Wheel {
    source: ScrollSource::Finger,
    x: WheelAxis::default(),
    y: WheelAxis {
      pixels,
      ..WheelAxis::default()
    },
  }
}

/// The fingers lifting off a touchpad.
fn lift() -> Wheel {
  Wheel {
    source: ScrollSource::Finger,
    x: WheelAxis {
      stop: true,
      ..WheelAxis::default()
    },
    y: WheelAxis {
      stop: true,
      ..WheelAxis::default()
    },
  }
}

/// The node at `path`, relative to the window's root, as a list of child
/// indexes.
fn node_at<'a>(root: NodeView<'a>, path: &[usize]) -> NodeView<'a> {
  path.iter().fold(root, |node, &index| {
    node
      .children()
      .nth(index)
      .unwrap_or_else(|| panic!("{} has no child {index}", node.path()))
  })
}

/// The rectangle of the node at `path`.
fn rect_at(window: &Window, path: &[usize]) -> Rect {
  node_at(window.view().root(), path).rect()
}

/// The widget path of every node, depth first.
fn paths(window: &Window) -> Vec<String> {
  fn walk(node: NodeView<'_>, out: &mut Vec<String>) {
    out.push(node.path().to_owned());
    for child in node.children() {
      walk(child, out);
    }
  }
  let mut out = Vec::new();
  walk(window.view().root(), &mut out);
  out
}

/// Counts how many times a closure ran.
#[derive(Clone, Default)]
struct Counter(Rc<Cell<u32>>);

impl Counter {
  fn hit(&self) {
    self.0.set(self.0.get() + 1);
  }

  fn get(&self) -> u32 {
    self.0.get()
  }
}

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
  #[derive(Clone, Default)]
  struct Buffer(Arc<Mutex<Vec<u8>>>);

  impl io::Write for Buffer {
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

  let buffer = Buffer::default();
  let writer = buffer.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = buffer.0.lock().expect("log buffer lock").clone();
  String::from_utf8(bytes).expect("log output is UTF-8")
}
