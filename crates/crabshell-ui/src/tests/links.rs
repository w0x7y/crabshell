//! Links in rich text: hit by their glyphs, with the pointing hand, and a
//! click that runs `on_link` and reaches nothing else.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Counter, Harness, capture_logs, node_at};
use crate::{
  Align, Button, Content, CursorShape, Point, PointerButton, Row, Span, Style, Text, Window,
};

/// "go to " and then a link to "https://e.org".
fn spans() -> Vec<Span> {
  vec![Span::new("go to "), Span::new("site").link("https://e.org")]
}

/// A text whose `on_link` records each URL it gets.
fn linked(spans: Vec<Span>) -> (Text, Rc<RefCell<Vec<String>>>) {
  let opened = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&opened);
  let text = Text::rich(spans).on_link(move |_cx, url| log.borrow_mut().push(url.to_owned()));
  (text, opened)
}

/// A 300 by 30 window holding `child` in a row whose children keep their
/// own height.
fn window(h: &mut Harness, child: impl crate::Widget) -> Window {
  let row = Row::new().align(Align::Start).child(child);
  let window = h.window(h.panel().child(row), 300.0, 60.0);
  window.keyboard_enter();
  window
}

/// The middle of the glyphs of span `span` on line `line` of the text at
/// `path`, where layout put them.
fn span_point(window: &Window, path: &[usize], span: usize, line: usize) -> Point {
  let view = window.view();
  let Content::Text(text) = node_at(view.root(), path).content() else {
    panic!("not a text");
  };
  let origin = text.origin();
  let run = text.runs().nth(line).expect("that line");
  let glyphs: Vec<_> = run
    .glyphs
    .iter()
    // The ellipsis carries the first span's metadata but no bytes.
    .filter(|glyph| glyph.metadata == span + 1 && glyph.start < glyph.end)
    .collect();
  let start = glyphs.first().expect("the span is on the line").x;
  let last = glyphs.last().expect("the span is on the line");
  Point::new(
    origin.x + (start + last.x + last.w) / 2.0,
    origin.y + run.line_top + run.line_height / 2.0,
  )
}

/// A left press and release at `at`.
fn click(window: &Window, at: Point, time: u32) {
  window.pointer_motion_at(at, time);
  window.pointer_button_at(PointerButton::Left, true, time);
  window.pointer_button_at(PointerButton::Left, false, time + 10);
}

#[test]
fn a_click_on_a_link_runs_on_link_and_nothing_around_it() {
  let mut h = Harness::new();
  let (text, opened) = linked(spans());
  let clicks = Counter::default();
  let count = clicks.clone();
  let button = Button::new(text)
    .cursor(CursorShape::Crosshair)
    .on_click(move |_| count.hit());
  let window = window(&mut h, button);
  let link = span_point(&window, &[0, 0, 0], 1, 0);
  let plain = span_point(&window, &[0, 0, 0], 0, 0);

  window.pointer_motion(link);
  assert_eq!(window.cursor(), CursorShape::Pointer);
  click(&window, link, 0);
  assert_eq!(*opened.borrow(), ["https://e.org"]);
  assert_eq!(clicks.get(), 0, "the button got the link's click");

  // Off the link, the text lets the pointer through to the button.
  window.pointer_motion(plain);
  assert_eq!(window.cursor(), CursorShape::Crosshair);
  click(&window, plain, 1000);
  assert_eq!(clicks.get(), 1);
  assert_eq!(opened.borrow().len(), 1);
}

#[test]
fn a_text_without_on_link_lets_its_links_through() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let count = clicks.clone();
  let button = Button::new(Text::rich(spans())).on_click(move |_| count.hit());
  let window = window(&mut h, button);
  click(&window, span_point(&window, &[0, 0, 0], 1, 0), 0);
  assert_eq!(clicks.get(), 1);
}

#[test]
fn a_press_and_release_on_different_places_opens_nothing() {
  let mut h = Harness::new();
  let (text, opened) = linked(vec![
    Span::new("one").link("https://one.org"),
    Span::new(" and "),
    Span::new("two").link("https://two.org"),
  ]);
  let window = window(&mut h, text);
  let one = span_point(&window, &[0, 0], 0, 0);
  let two = span_point(&window, &[0, 0], 2, 0);
  window.pointer_motion_at(one, 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(two, 10);
  window.pointer_button_at(PointerButton::Left, false, 20);
  assert!(opened.borrow().is_empty(), "{:?}", opened.borrow());
  click(&window, two, 1000);
  assert_eq!(*opened.borrow(), ["https://two.org"]);
}

#[test]
fn a_wrapped_link_opens_from_either_line() {
  let mut h = Harness::new();
  let (text, opened) = linked(vec![
    Span::new("see "),
    Span::new("the whole long link").link("https://e.org"),
  ]);
  let text = text.max_lines(2).style(Style::new().width(90.0));
  let window = window(&mut h, text);
  click(&window, span_point(&window, &[0, 0], 1, 1), 0);
  click(&window, span_point(&window, &[0, 0], 1, 0), 1000);
  assert_eq!(opened.borrow().len(), 2);
}

#[test]
fn a_link_is_hit_where_a_transform_draws_it() {
  for style in [
    Style::new().translate(40.0, 10.0),
    Style::new().scale(2.0).origin(0.0, 0.0),
  ] {
    let mut h = Harness::new();
    let (text, opened) = linked(spans());
    let window = window(&mut h, text.style(style));
    let laid_out = span_point(&window, &[0, 0], 1, 0);
    let drawn = match style.translate {
      Some(_) => Point::new(laid_out.x + 40.0, laid_out.y + 10.0),
      None => Point::new(laid_out.x * 2.0, laid_out.y * 2.0),
    };
    window.pointer_motion(laid_out);
    assert_eq!(window.cursor(), CursorShape::Default, "{style:?}");
    click(&window, laid_out, 0);
    assert!(opened.borrow().is_empty(), "{style:?}");
    window.pointer_motion(drawn);
    assert_eq!(window.cursor(), CursorShape::Pointer, "{style:?}");
    click(&window, drawn, 1000);
    assert_eq!(*opened.borrow(), ["https://e.org"], "{style:?}");
  }
}

#[test]
fn a_selectable_text_opens_a_link_on_a_click_and_selects_on_a_drag() {
  let mut h = Harness::new();
  let (text, opened) = linked(spans());
  let window = window(&mut h, text.selectable(true));
  let link = span_point(&window, &[0, 0], 1, 0);
  let plain = span_point(&window, &[0, 0], 0, 0);
  window.pointer_motion(plain);
  assert_eq!(window.cursor(), CursorShape::Text);
  window.pointer_motion(link);
  assert_eq!(window.cursor(), CursorShape::Pointer);

  click(&window, link, 0);
  assert_eq!(*opened.borrow(), ["https://e.org"]);

  // A drag across the link selects instead, ending on the same link.
  window.pointer_motion_at(Point::new(link.x - 8.0, link.y), 1000);
  window.pointer_button_at(PointerButton::Left, true, 1000);
  window.pointer_motion_at(Point::new(link.x + 8.0, link.y), 1010);
  window.pointer_button_at(PointerButton::Left, false, 1020);
  h.frame(&window);
  assert_eq!(opened.borrow().len(), 1);
  let view = window.view();
  let Content::Text(text) = node_at(view.root(), &[0, 0]).content() else {
    panic!("not a text");
  };
  assert!(!text.selection().is_empty());
}

#[test]
fn a_panicking_on_link_is_logged_and_the_window_runs_on() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let count = clicks.clone();
  let text = Text::rich(spans()).on_link(|_, _| panic!("no browser"));
  let row = Row::new()
    .align(Align::Start)
    .child(text)
    .child(Button::new(Text::new("other")).on_click(move |_| count.hit()));
  let window = h.window(h.panel().child(row), 300.0, 30.0);
  let link = span_point(&window, &[0, 0], 1, 0);
  let logs = capture_logs(|| click(&window, link, 0));
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Text[0].on_link panicked and is disabled"),
    "{logs}"
  );
  let other = node_at(window.view().root(), &[0, 1]).rect();
  click(&window, Point::new(other.x + 2.0, other.y + 5.0), 1000);
  assert_eq!(clicks.get(), 1);
}

#[test]
fn the_ellipsis_after_a_leading_link_is_not_part_of_it() {
  let mut h = Harness::new();
  // cosmic-text shapes the "…" with the first span's attributes.
  let (text, opened) = linked(vec![
    Span::new("link").link("https://e.org"),
    Span::new(" and a great deal more plain text than fits"),
  ]);
  let text = text
    .elide(crate::Elide::End)
    .style(Style::new().width(90.0));
  let window = window(&mut h, text);
  let ellipsis = {
    let view = window.view();
    let Content::Text(text) = node_at(view.root(), &[0, 0]).content() else {
      panic!("not a text");
    };
    let run = text.runs().next().expect("one line");
    let glyph = run.glyphs.last().expect("the ellipsis ends the line");
    // The ellipsis holds no bytes of the text.
    assert_eq!(glyph.start, glyph.end, "the last glyph is the ellipsis");
    assert_eq!(glyph.metadata, 1, "cosmic-text gave it the link's");
    assert!(text.span(run.line_i, glyph).is_none());
    let origin = text.origin();
    Point::new(
      origin.x + glyph.x + glyph.w / 2.0,
      origin.y + run.line_top + run.line_height / 2.0,
    )
  };
  window.pointer_motion(ellipsis);
  assert_eq!(window.cursor(), CursorShape::Default);
  click(&window, ellipsis, 0);
  assert!(opened.borrow().is_empty(), "{:?}", opened.borrow());
  click(&window, span_point(&window, &[0, 0], 0, 0), 1000);
  assert_eq!(*opened.borrow(), ["https://e.org"]);
}

#[test]
fn a_link_that_changes_while_held_opens_nothing() {
  let mut h = Harness::new();
  let url = h.cx.signal(String::from("https://one.org"));
  let opened = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&opened);
  let text = Text::rich(move || vec![Span::new("go to "), Span::new("site").link(url.get())])
    .on_link(move |_cx, url| log.borrow_mut().push(url.to_owned()));
  let window = window(&mut h, text);
  let link = span_point(&window, &[0, 0], 1, 0);
  window.pointer_motion_at(link, 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  // Another link is under the press now.
  url.set(String::from("https://two.org"));
  h.frame(&window);
  window.pointer_button_at(PointerButton::Left, false, 10);
  assert!(opened.borrow().is_empty(), "{:?}", opened.borrow());
  click(&window, link, 1000);
  assert_eq!(*opened.borrow(), ["https://two.org"]);
}

#[test]
fn a_link_whose_on_link_panicked_lets_the_button_around_it_click() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let count = clicks.clone();
  let text = Text::rich(spans()).on_link(|_, _| panic!("no browser"));
  let button = Button::new(text)
    .cursor(CursorShape::Crosshair)
    .on_click(move |_| count.hit());
  let window = window(&mut h, button);
  let link = span_point(&window, &[0, 0, 0], 1, 0);
  capture_logs(|| click(&window, link, 0));
  assert_eq!(clicks.get(), 0);
  // Disabled, the link is plain text: the button takes the pointer.
  window.pointer_motion(link);
  assert_eq!(window.cursor(), CursorShape::Crosshair);
  click(&window, link, 1000);
  assert_eq!(clicks.get(), 1);
}
