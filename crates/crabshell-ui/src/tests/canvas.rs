//! Canvases: when the paint closure runs, what it records, what that
//! repaints, and what it refuses.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Counter, Harness, capture_logs, node_at};
use crate::{
  Canvas, CanvasOp, Color, Content, Paint, Path, PathCommand, Point, Rect, Row, Size, Stroke,
  Style, Text, Window,
};

const RED: Color = Color::rgb(255, 0, 0);

/// What the canvas at `path` drew at its last good run.
fn ops(window: &Window, path: &[usize]) -> Vec<CanvasOp> {
  match node_at(window.view().root(), path).content() {
    Content::Canvas(canvas) => canvas.ops.to_vec(),
    _ => panic!("no canvas at {path:?}"),
  }
}

/// A square path `side` wide at the corner.
fn square(side: f32) -> Path {
  Path::rect(Rect::new(0.0, 0.0, side, side))
}

#[test]
fn the_paint_runs_once_laid_out_and_again_only_when_its_size_changes() {
  let mut h = Harness::new();
  let runs = Counter::default();
  let sizes = Rc::new(RefCell::new(Vec::new()));
  let width = h.cx.signal(40.0_f32);
  let sibling = h.cx.signal(RED);
  let (counted, seen) = (runs.clone(), Rc::clone(&sizes));
  let row = Row::new()
    .child(Row::new().style(move || Style::new().width(10.0).background(sibling.get())))
    .child(
      Canvas::new(move |pen| {
        counted.hit();
        seen.borrow_mut().push(pen.size());
        pen.fill(&square(4.0), RED);
      })
      .style(move || Style::new().width(width.get()).height(20.0)),
    );
  let window = Window::new(&h.cx, h.panel().child(row));
  window.set_size(Size::new(200.0, 30.0));
  h.runtime.flush();
  assert_eq!(runs.get(), 0, "it waits for its size");
  window.layout(&mut h.text);
  assert_eq!(*sizes.borrow(), [Size::new(40.0, 20.0)]);
  assert_eq!(ops(&window, &[0, 1]).len(), 1, "drawn by the first layout");

  // A sibling's paint change lays nothing out and leaves the canvas be.
  sibling.set(Color::rgb(0, 0, 255));
  h.frame(&window);
  assert_eq!(runs.get(), 1);
  // Nor does a layout that keeps its size.
  window.set_size(Size::new(300.0, 30.0));
  h.frame(&window);
  assert_eq!(runs.get(), 1);

  width.set(60.0);
  h.frame(&window);
  assert_eq!(
    *sizes.borrow(),
    [Size::new(40.0, 20.0), Size::new(60.0, 20.0)]
  );
}

#[test]
fn a_signal_the_paint_read_repaints_only_the_canvas_box() {
  let mut h = Harness::new();
  let side = h.cx.signal(4.0_f32);
  let row = Row::new().child(Text::new("label")).child(
    Canvas::new(move |pen| pen.fill(&square(side.get()), RED))
      .style(Style::new().width(30.0).height(20.0)),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();

  side.set(8.0);
  h.runtime.flush();
  assert!(!window.needs_layout(), "a drawing is a paint change");
  let canvas = node_at(window.view().root(), &[0, 1]).bounds();
  assert_eq!(window.take_damage(), [canvas]);
  assert_eq!(
    ops(&window, &[0, 1]),
    [CanvasOp::Fill(square(8.0), Paint::Color(RED))]
  );

  // The same drawing again repaints nothing.
  side.update(|_| {});
  h.frame(&window);
  assert!(window.take_damage().is_empty());
}

#[test]
fn an_idle_canvas_records_nothing_again() {
  let mut h = Harness::new();
  let runs = Counter::default();
  let counted = runs.clone();
  let sibling = h.cx.signal(RED);
  let row = Row::new()
    .child(Row::new().style(move || Style::new().width(10.0).background(sibling.get())))
    .child(
      Canvas::new(move |pen| {
        counted.hit();
        pen.fill(&Path::circle(Point::new(10.0, 10.0), 8.0), RED);
      })
      .style(Style::new().width(20.0).height(20.0)),
    );
  let window = h.window(h.panel().child(row), 40.0, 20.0);
  window.take_damage();
  let beside = node_at(window.view().root(), &[0, 0]).bounds();
  for n in 1..20 {
    // A sibling's paint changes, and the window is laid out again at the
    // same size.
    sibling.set(Color::rgb(0, n, 0));
    h.runtime.flush();
    window.layout(&mut h.text);
    assert_eq!(window.take_damage(), [beside], "only the sibling repaints");
  }
  assert_eq!(runs.get(), 1);
}

#[test]
fn a_path_started_without_move_to_counts_its_closes_from_its_start() {
  let mut h = Harness::new();
  // As drawn, each piece starts at (1000, 0) and runs out to near the
  // corner and back: 20,000 pixels in all, so 200,000 dashes.
  let canvas = Canvas::new(|pen| {
    let path = (0..10).fold(Path::new().line_to(Point::new(1000.0, 0.0)), |path, k| {
      path.line_to(Point::new(0.0, k as f32)).close()
    });
    pen.stroke(&path, &Stroke::new(1.0, RED).dash(&[0.05, 0.05], 0.0));
  });
  let mut window = None;
  let logs = capture_logs(|| window = Some(h.window(h.panel().child(canvas), 20.0, 20.0)));
  assert!(ops(&window.expect("built"), &[0]).is_empty(), "{logs}");
}

#[test]
fn a_panicking_paint_is_logged_with_its_path_and_the_last_drawing_stays() {
  let mut h = Harness::new();
  let n = h.cx.signal(0);
  let row = Row::new()
    .child(Text::new(move || format!("n is {}", n.get())))
    .child(
      Canvas::new(move |pen| {
        pen.fill(&square(4.0), RED);
        assert!(n.get() != 1, "the canvas fails");
      })
      .style(Style::new().width(10.0).height(10.0)),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let drawn = ops(&window, &[0, 1]);
  let logs = capture_logs(|| {
    n.set(1);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Canvas[1].paint panicked and is disabled")
      && logs.contains("the canvas fails"),
    "{logs}"
  );
  assert_eq!(ops(&window, &[0, 1]), drawn, "the last good drawing stays");
  n.set(2);
  h.frame(&window);
  let view = window.view();
  let Content::Text(text) = node_at(view.root(), &[0, 0]).content() else {
    panic!("no text");
  };
  assert_eq!(text.content(), "n is 2", "the rest keeps running");
}

#[test]
fn numbers_it_cannot_draw_are_skipped_and_logged_once() {
  let mut h = Harness::new();
  let n = h.cx.signal(0);
  let canvas = Canvas::new(move |pen| {
    n.get();
    pen.fill(&Path::new().line_to(Point::new(f32::NAN, 0.0)), RED);
    pen.fill(&Path::new().line_to(Point::new(1.0e9, 0.0)), RED);
    pen.fill(&Path::new().arc(Point::new(0.0, 0.0), -1.0, 0.0, 90.0), RED);
    pen.stroke(&square(4.0), &Stroke::new(f32::INFINITY, RED));
    pen.stroke(&square(4.0), &Stroke::new(-1.0, RED));
    pen.stroke(&square(4.0), &Stroke::new(1.0, RED).dash(&[0.0, 0.0], 0.0));
    pen.stroke(&square(4.0), &Stroke::new(1.0, RED).dash(&[2.0, -1.0], 0.0));
    pen.stroke(&square(4.0), &Stroke::new(1.0, RED).dash(&[2.0], f32::NAN));
    // Ten million dashes along a long line.
    let long = Path::new()
      .move_to(Point::new(0.0, 0.0))
      .line_to(Point::new(1.0e6, 0.0));
    pen.stroke(&long, &Stroke::new(1.0, RED).dash(&[0.05, 0.05], 0.0));
    // A width of 0 draws nothing, quietly.
    pen.stroke(&square(4.0), &Stroke::new(0.0, RED));
    pen.fill(&square(2.0), RED);
  });
  let mut window = None;
  let logs = capture_logs(|| window = Some(h.window(h.panel().child(canvas), 20.0, 20.0)));
  let window = window.expect("built");
  assert_eq!(
    ops(&window, &[0]),
    [CanvasOp::Fill(square(2.0), Paint::Color(RED))]
  );
  assert_eq!(
    logs.matches("PanelWindow[DP-1]/Canvas.paint drew").count(),
    1,
    "{logs}"
  );
  assert!(logs.contains("NaN"), "the first is named: {logs}");
  let again = capture_logs(|| {
    n.set(1);
    h.frame(&window);
  });
  assert!(!again.contains("drew"), "once per canvas: {again}");
}

#[test]
fn a_runaway_paint_keeps_at_most_16384_commands() {
  let mut h = Harness::new();
  let canvas = Canvas::new(|pen| {
    for _ in 0..10 {
      let zigzag = (0..2_000).fold(Path::new(), |path, x| {
        path.line_to(Point::new(x as f32, (x % 2) as f32))
      });
      pen.stroke(&zigzag, &Stroke::new(1.0, RED));
    }
  });
  let mut window = None;
  let logs = capture_logs(|| window = Some(h.window(h.panel().child(canvas), 20.0, 20.0)));
  assert_eq!(ops(&window.expect("built"), &[0]).len(), 8);
  assert!(logs.contains("more than 16384 path commands"), "{logs}");
}

#[test]
fn an_odd_dash_pattern_is_repeated() {
  let mut h = Harness::new();
  let canvas = Canvas::new(|pen| {
    pen.stroke(
      &square(4.0),
      &Stroke::new(1.0, RED).dash(&[3.0, 1.0, 2.0], 1.0),
    );
  });
  let window = h.window(h.panel().child(canvas), 20.0, 20.0);
  let [CanvasOp::Stroke(_, stroke)] = &ops(&window, &[0])[..] else {
    panic!("one stroke");
  };
  assert_eq!(stroke.dash, [3.0, 1.0, 2.0, 3.0, 1.0, 2.0]);
}

#[test]
fn shapes_are_built_from_plain_commands() {
  assert_eq!(
    Path::rect(Rect::new(1.0, 2.0, 3.0, 4.0)).commands(),
    [
      PathCommand::MoveTo(Point::new(1.0, 2.0)),
      PathCommand::LineTo(Point::new(4.0, 2.0)),
      PathCommand::LineTo(Point::new(4.0, 6.0)),
      PathCommand::LineTo(Point::new(1.0, 6.0)),
      PathCommand::Close,
    ]
  );
  // The radius is held to half the shorter side.
  let pill = Path::rounded_rect(Rect::new(0.0, 0.0, 40.0, 10.0), 50.0);
  assert!(pill.commands().contains(&PathCommand::Arc {
    center: Point::new(35.0, 5.0),
    radius: 5.0,
    start: -90.0,
    sweep: 90.0,
  }));
  assert_eq!(
    Path::rounded_rect(Rect::new(0.0, 0.0, 4.0, 4.0), 0.0),
    Path::rect(Rect::new(0.0, 0.0, 4.0, 4.0))
  );
}

#[test]
fn a_dropped_canvas_frees_everything_it_made() {
  let mut h = Harness::new();
  let empty = h.runtime.node_count();
  let n = h.cx.signal(0);
  let before = h.runtime.node_count();
  let canvas = Canvas::new(move |pen| pen.fill(&square(n.get() as f32), RED));
  let window = h.window(h.panel().child(canvas), 20.0, 20.0);
  assert!(h.runtime.node_count() > before);
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
  assert!(before > empty);
}
