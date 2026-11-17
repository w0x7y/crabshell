//! Canvases: their goldens, repaints after a change, and drawing through
//! opacity, clip and transform layers.

use crabshell_reactive::Signal;
use crabshell_ui::{
  Align, Canvas, Cap, Color, Gradient, Join, PanelWindow, Path, Point, Rect, Row, Stack, Stroke,
  Style,
};

use super::{Buffer, Scene, assert_golden};
use crate::DamageHistory;

const BACKGROUND: Style = Style::new().background(Color::hex("#1e1e2e"));
const TRACK: Color = Color::hex("#45475a");
const GREEN: Color = Color::hex("#a6e3a1");
const BLUE: Color = Color::hex("#89b4fa");
const PEACH: Color = Color::hex("#fab387");

/// A progress ring at `progress` of a turn: a track and an arc from 12
/// o'clock with round caps, inset so the caps stay inside the box.
fn ring(progress: Signal<f32>) -> Canvas {
  Canvas::new(move |pen| {
    let size = pen.size();
    let middle = Point::new(size.width / 2.0, size.height / 2.0);
    let radius = size.width.min(size.height) / 2.0 - 4.0;
    pen.stroke(&Path::circle(middle, radius), &Stroke::new(4.0, TRACK));
    let arc = Path::new().arc(middle, radius, -90.0, 360.0 * progress.get());
    pen.stroke(&arc, &Stroke::new(4.0, GREEN).cap(Cap::Round));
  })
  .style(Style::new().width(36.0).height(36.0))
}

/// The points of a small graph across `width`, for `samples`.
fn points(samples: &[f32], width: f32, height: f32) -> Vec<Point> {
  let step = width / (samples.len() - 1) as f32;
  samples
    .iter()
    .enumerate()
    .map(|(i, sample)| Point::new(i as f32 * step, height * (1.0 - sample)))
    .collect()
}

/// A filled graph with a gradient under a line through `samples`.
fn graph(samples: Signal<Vec<f32>>) -> Canvas {
  Canvas::new(move |pen| {
    let size = pen.size();
    let points = points(&samples.get(), size.width, size.height);
    let line = points[1..]
      .iter()
      .fold(Path::new().move_to(points[0]), |path, &p| path.line_to(p));
    let area = line
      .clone()
      .line_to(Point::new(size.width, size.height))
      .line_to(Point::new(0.0, size.height))
      .close();
    pen.fill(
      &area,
      Gradient::linear(
        180.0,
        Color::rgba(0x89, 0xb4, 0xfa, 0xc0),
        Color::rgba(0x89, 0xb4, 0xfa, 0),
      ),
    );
    pen.stroke(&line, &Stroke::new(1.5, BLUE).join(Join::Round));
  })
  .style(Style::new().width(80.0).height(36.0))
}

/// A dashed polyline with square caps and bevelled corners, and a cubic
/// and a quadratic curve dashed with round caps.
fn dashes() -> Canvas {
  Canvas::new(|pen| {
    let zigzag = Path::new()
      .move_to(Point::new(4.0, 30.0))
      .line_to(Point::new(16.0, 6.0))
      .line_to(Point::new(28.0, 30.0))
      .line_to(Point::new(40.0, 6.0));
    let dashed = Stroke::new(2.0, PEACH)
      .cap(Cap::Square)
      .join(Join::Bevel)
      .dash(&[5.0, 3.0], 1.0);
    pen.stroke(&zigzag, &dashed);
    let curves = Path::new()
      .move_to(Point::new(46.0, 30.0))
      .cubic_to(
        Point::new(50.0, 0.0),
        Point::new(60.0, 40.0),
        Point::new(64.0, 8.0),
      )
      .quad_to(Point::new(70.0, 0.0), Point::new(76.0, 30.0));
    pen.stroke(
      &curves,
      &Stroke::new(2.0, GREEN)
        .cap(Cap::Round)
        .dash(&[0.0, 4.0], 0.0),
    );
  })
  .style(Style::new().width(80.0).height(36.0))
}

/// A rounded canvas whose ellipse reaches past its box, cut to its
/// corners, inside a translucent, turned stack.
fn layered() -> Stack {
  Stack::new()
    .style(Style::new().opacity(0.6).rotate(12.0))
    .child(
      Canvas::new(|pen| {
        pen.fill(&Path::ellipse(Point::new(18.0, 18.0), 26.0, 12.0), PEACH);
        pen.fill(
          &Path::rounded_rect(Rect::new(8.0, 8.0, 20.0, 20.0), 4.0),
          BLUE,
        );
      })
      .style(
        Style::new()
          .width(36.0)
          .height(36.0)
          .radius(10.0)
          .border(1.0, GREEN),
      ),
    )
}

fn panel(scene: &Scene) -> PanelWindow {
  let progress = scene.cx.signal(0.7_f32);
  let samples = scene
    .cx
    .signal(vec![0.2, 0.5, 0.35, 0.8, 0.6, 0.9, 0.4, 0.55]);
  scene.panel().style(BACKGROUND).child(
    Row::new()
      .padding(4, 6)
      .gap(8)
      .align(Align::Center)
      .child(ring(progress))
      .child(graph(samples))
      .child(dashes())
      .child(layered()),
  )
}

/// A progress ring with round caps, a filled graph with a gradient, a
/// dashed polyline and curves, and a canvas cut to its rounded box inside
/// an opacity and transform layer.
#[test]
fn canvases() {
  for (scale, suffix) in [(1.0, "1x"), (1.5, "1.5x"), (2.0, "2x")] {
    let mut scene = Scene::new();
    let panel = panel(&scene);
    let window = scene.window(panel, 270.0, 44.0);
    let buffer = scene.full(&window, scale);
    assert_golden(&format!("canvas@{suffix}"), &buffer);
  }
}

#[test]
fn a_canvas_redrawn_after_a_change_matches_a_fresh_draw() {
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let mut scene = Scene::new();
    let progress = scene.cx.signal(0.3_f32);
    let samples = scene.cx.signal(vec![0.1, 0.9, 0.3]);
    // The graph spans the tile edge at 64 buffer pixels.
    let panel = scene.panel().style(BACKGROUND).child(
      Row::new()
        .padding(4, 6)
        .gap(8)
        .align(Align::Center)
        .child(ring(progress))
        .child(graph(samples))
        .child(layered()),
    );
    let window = scene.window(panel, 200.0, 44.0);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);

    progress.set(0.85);
    samples.set(vec![0.7, 0.2, 0.6, 0.4]);
    scene.frame(&window);
    let damage = window.take_damage();
    let canvases = [
      super::node_at(window.view().root(), &[0, 0]).bounds(),
      super::node_at(window.view().root(), &[0, 1]).bounds(),
    ];
    assert_eq!(damage, canvases, "only the two canvases repaint");
    super::repaint::repaint(&mut scene, &window, &damage, &mut history, &mut buffer);
    let full = scene.full(&window, scale);
    assert!(buffer.pixels == full.pixels, "at {scale}");
  }
}

#[test]
fn a_canvas_draws_only_inside_its_box() {
  let mut scene = Scene::new();
  let canvas = Canvas::new(|pen| {
    pen.fill(&Path::rect(Rect::new(-50.0, -50.0, 200.0, 200.0)), BLUE);
  })
  .style(Style::new().width(20.0).height(10.0));
  let panel = scene.panel().child(Row::new().padding(5, 5).child(canvas));
  let window = scene.window(panel, 40.0, 30.0);
  for scale in [1.0, 1.5, 2.0] {
    let buffer = scene.full(&window, scale);
    let px = |x: f32, y: f32| buffer.pixel((x * scale) as u32, (y * scale) as u32);
    assert_eq!(px(10.0, 8.0), [0x89, 0xb4, 0xfa, 255], "inside at {scale}");
    for (x, y) in [(2.0, 8.0), (27.0, 8.0), (10.0, 2.0), (10.0, 17.0)] {
      assert_eq!(px(x, y), [0, 0, 0, 0], "outside at {x}, {y} at {scale}");
    }
  }
}

/// A 400 by 140 canvas of 300 small circles, each filled with a sweep
/// gradient, and stroked with one every other.
fn dots() -> Canvas {
  Canvas::new(|pen| {
    for index in 0..300 {
      let middle = Point::new(
        8.0 + (index % 30) as f32 * 13.0,
        8.0 + (index / 30) as f32 * 13.0,
      );
      let sweep = Gradient::sweep(index as f32, BLUE, PEACH);
      let circle = Path::circle(middle, 5.0);
      pen.fill(&circle, sweep);
      if index % 2 == 0 {
        pen.stroke(&circle, &Stroke::new(1.5, sweep));
      }
    }
  })
  .style(Style::new().width(400.0).height(140.0))
}

#[test]
fn a_canvas_gradient_is_worked_out_only_around_its_path() {
  let mut scene = Scene::new();
  let panel = scene.panel().child(Row::new().padding(4, 4).child(dots()));
  let window = scene.window(panel, 408.0, 148.0);
  let before = scene.renderer.gradient_pixels();
  scene.full(&window, 2.0);
  let worked = scene.renderer.gradient_pixels() - before;
  // Each circle is 20 pixels across at scale 2, its stroke a little
  // more: 450 paths of at most 32 by 32 pixels with the margins, where
  // the whole canvas's box in each tile would be thousands of times more.
  assert!(worked > 0 && worked <= 450 * 32 * 32, "{worked} pixels");
}

/// How long a full draw of [`dots`] takes at scale 2, at best. Run it in
/// a release build:
/// `cargo test --release -p crabshell-render --lib time_a_canvas_of_gradient_dots -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_a_canvas_of_gradient_dots() {
  let mut scene = Scene::new();
  let panel = scene.panel().child(Row::new().padding(4, 4).child(dots()));
  let window = scene.window(panel, 408.0, 148.0);
  let fastest = (0..10)
    .map(|_| {
      let start = std::time::Instant::now();
      scene.full(&window, 2.0);
      start.elapsed()
    })
    .min()
    .expect("a draw");
  println!(
    "{:.1} ms a full draw at scale 2",
    fastest.as_secs_f64() * 1000.0
  );
}
