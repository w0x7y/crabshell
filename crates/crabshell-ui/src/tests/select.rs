//! Selecting text with the pointer: dragging, double clicks and
//! Shift+click.

use crabshell_reactive::Signal;

use super::{Counter, rect_at};
use super::{Harness, node_at};
use crate::{
  Button, Clipboard, Column, Content, CursorShape, Icon, Key, KeyEvent, Modifiers, Point,
  PointerButton, Rect, Row, ScrollView, Span, Style, Text, TextInput, Window,
};

/// Ctrl+C, and what it copied.
fn copied(window: &Window) -> Option<String> {
  let ctrl_c = KeyEvent {
    key: Key::Character('c'),
    text: None,
    modifiers: Modifiers {
      ctrl: true,
      ..Modifiers::default()
    },
    repeat: false,
  };
  match window.key(&ctrl_c) {
    Some(Clipboard::Copy(text)) => Some(text),
    _ => None,
  }
}

/// A 200 by 30 window with the keyboard and one 100-pixel field.
fn field(h: &mut Harness, text: Signal<String>) -> Window {
  let row = Row::new().child(TextInput::new(text).style(Style::new().width(100.0)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  window
}

/// A left click at `x`, 10 down, going down at `time` and up 10 ms later.
fn click(window: &Window, x: f32, time: u32) {
  window.pointer_motion_at(Point::new(x, 10.0), time);
  window.pointer_button_at(PointerButton::Left, true, time);
  window.pointer_button_at(PointerButton::Left, false, time + 10);
}

#[test]
fn dragging_in_a_field_selects_to_the_pointer_even_outside_it() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = field(&mut h, text);
  window.pointer_motion_at(Point::new(1.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  assert_eq!(copied(&window), None, "a press only places the caret");
  // Past the field's right end, outside it.
  window.pointer_motion_at(Point::new(190.0, 25.0), 20);
  h.frame(&window);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
  // Back to the start: the selection follows the pointer.
  window.pointer_motion_at(Point::new(1.0, 10.0), 40);
  assert_eq!(copied(&window), None);
  window.pointer_motion_at(Point::new(190.0, 10.0), 60);
  window.pointer_button_at(PointerButton::Left, false, 80);
  // The release keeps the selection, and moving after it changes nothing.
  window.pointer_motion_at(Point::new(1.0, 10.0), 100);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
}

#[test]
fn dragging_a_long_field_scrolls_its_text() {
  let mut h = Harness::new();
  let text = h.cx.signal("w".repeat(60));
  let window = field(&mut h, text);
  // The caret starts at the end, so the field shows the end first.
  click(&window, 1.0, 0);
  window.key(&KeyEvent {
    key: Key::Home,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  });
  h.frame(&window);
  window.pointer_motion_at(Point::new(1.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 1000);
  // Each move past the end selects further and the layout scrolls the
  // text after it, as the caret goes.
  for step in 1..40 {
    window.pointer_motion_at(Point::new(150.0, 10.0), 1000 + step * 10);
    h.frame(&window);
  }
  window.pointer_button_at(PointerButton::Left, false, 1500);
  assert_eq!(copied(&window), Some("w".repeat(60)));
}

#[test]
fn a_double_click_selects_the_word_under_the_pointer() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("one two"));
  let window = field(&mut h, text);
  // Find a place in "two".
  let x = (1..100)
    .map(|x| x as f32)
    .find(|&x| {
      click(&window, x, 10_000 * x as u32);
      window.field().is_some_and(|field| field.cursor == 5)
    })
    .expect("a place inside the second word");
  click(&window, x, 0);
  click(&window, x, 200);
  assert_eq!(copied(&window).as_deref(), Some("two"));
  // Too slow for a double click: the second click places the caret.
  click(&window, x, 1000);
  click(&window, x, 1600);
  assert_eq!(copied(&window), None);
}

#[test]
fn shift_click_extends_the_selection() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = field(&mut h, text);
  click(&window, 1.0, 0);
  window.set_modifiers(Modifiers {
    shift: true,
    ..Modifiers::default()
  });
  click(&window, 99.0, 1000);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
  window.set_modifiers(Modifiers::default());
  click(&window, 99.0, 2000);
  assert_eq!(copied(&window), None, "a plain click drops it");
}

#[test]
fn dragging_a_field_in_a_scroll_view_selects_instead_of_scrolling() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let offset = h.cx.signal(0.0_f32);
  let content = Column::new()
    .child(TextInput::new(text).style(Style::new().width(100.0)))
    .child(Icon::new("x").size(200));
  let view = ScrollView::new(content).offset(offset);
  let window = h.window(h.panel().child(view), 200.0, 30.0);
  window.keyboard_enter();
  let field = node_at(window.view().root(), &[0, 0, 0]).rect();
  window.pointer_motion_at(Point::new(field.x + 1.0, field.y + 5.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(field.x + 1.0, field.y + 25.0), 50);
  window.pointer_motion_at(Point::new(150.0, 29.0), 100);
  window.pointer_button_at(PointerButton::Left, false, 150);
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
}

/// A 200 by 30 window with the keyboard and `child` in a row.
fn label(h: &mut Harness, child: impl crate::Widget) -> Window {
  let window = h.window(h.panel().child(Row::new().child(child)), 200.0, 30.0);
  window.keyboard_enter();
  window
}

/// Drags the left button from `from` to `to`.
fn drag(window: &Window, from: Point, to: Point, time: u32) {
  window.pointer_motion_at(from, time);
  window.pointer_button_at(PointerButton::Left, true, time);
  window.pointer_motion_at(to, time + 10);
  window.pointer_button_at(PointerButton::Left, false, time + 20);
}

/// The selection the text at `path` draws.
fn selection(window: &Window, path: &[usize]) -> Vec<Rect> {
  match node_at(window.view().root(), path).content() {
    Content::Text(text) => text.selection(),
    _ => panic!("not a text"),
  }
}

#[test]
fn a_selectable_text_selects_by_dragging_and_copies_while_focused() {
  let mut h = Harness::new();
  let window = label(&mut h, Text::new("hello world").selectable(true));
  window.pointer_motion(Point::new(5.0, 10.0));
  assert_eq!(window.cursor(), CursorShape::Text);
  drag(&window, Point::new(1.0, 10.0), Point::new(190.0, 10.0), 0);
  h.frame(&window);
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Text[0]")
  );
  assert_eq!(copied(&window).as_deref(), Some("hello world"));
  let text = rect_at(&window, &[0, 0]);
  let shown = selection(&window, &[0, 0]);
  assert_eq!(shown.len(), 1);
  assert!(shown[0].width > text.width / 2.0, "{shown:?} in {text:?}");
}

#[test]
fn a_text_selects_nothing_unless_it_is_selectable() {
  let mut h = Harness::new();
  let on = h.cx.signal(false);
  let clicks = Counter::default();
  let c = clicks.clone();
  let button =
    Button::new(Text::new("hello").selectable(move || on.get())).on_click(move |_| c.hit());
  let window = label(&mut h, button);
  drag(&window, Point::new(1.0, 10.0), Point::new(3.0, 10.0), 0);
  assert_eq!(clicks.get(), 1, "the press goes to the button");
  assert_eq!(window.focused(), None);
  // Selectable, the text is the deepest target and takes the press.
  on.set(true);
  h.frame(&window);
  drag(
    &window,
    Point::new(1.0, 10.0),
    Point::new(190.0, 10.0),
    1000,
  );
  assert_eq!(clicks.get(), 1);
  assert_eq!(copied(&window).as_deref(), Some("hello"));
  // Turned off, it drops its selection and the focus.
  on.set(false);
  h.frame(&window);
  assert_eq!(window.focused(), None);
  assert!(selection(&window, &[0, 0, 0]).is_empty());
}

#[test]
fn a_press_elsewhere_or_a_new_text_drops_the_selection() {
  let mut h = Harness::new();
  let words = h.cx.signal(String::from("hello world"));
  let row = Row::new()
    .child(Text::new(move || words.get()).selectable(true))
    .child(Icon::new("x").size(20));
  let window = label(&mut h, row);
  let icon = rect_at(&window, &[0, 0, 1]);
  drag(&window, Point::new(1.0, 10.0), Point::new(190.0, 10.0), 0);
  assert_eq!(copied(&window).as_deref(), Some("hello world"));
  window.pointer_motion_at(Point::new(icon.x + 5.0, 10.0), 1000);
  window.pointer_button_at(PointerButton::Left, true, 1000);
  window.pointer_button_at(PointerButton::Left, false, 1010);
  h.frame(&window);
  assert_eq!(window.focused(), None);
  assert!(selection(&window, &[0, 0, 0]).is_empty());

  drag(
    &window,
    Point::new(1.0, 10.0),
    Point::new(190.0, 10.0),
    2000,
  );
  // Longer, so the old selection's bytes would still be there.
  words.set(String::from("bye bye bye bye"));
  h.frame(&window);
  assert_eq!(copied(&window), None);
}

#[test]
fn tab_skips_a_selectable_text() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let row = Row::new()
    .child(Text::new("label").selectable(true))
    .child(TextInput::new(text));
  let window = label(&mut h, row);
  window.key(&KeyEvent {
    key: Key::Tab,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  });
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Row[0]/TextInput[1]")
  );
}

#[test]
fn a_selection_runs_across_the_lines_of_a_text() {
  let mut h = Harness::new();
  let window = label(&mut h, Text::new("one\ntwo").max_lines(2));
  // Not selectable: nothing to show.
  assert!(selection(&window, &[0, 0]).is_empty());
  let window = label(&mut h, Text::new("one\ntwo").max_lines(2).selectable(true));
  let text = rect_at(&window, &[0, 0]);
  drag(
    &window,
    Point::new(text.x + 1.0, text.y + 2.0),
    Point::new(190.0, text.bottom() - 2.0),
    0,
  );
  h.frame(&window);
  assert_eq!(copied(&window).as_deref(), Some("one\ntwo"));
  let shown = selection(&window, &[0, 0]);
  assert_eq!(shown.len(), 2, "{shown:?}");
  assert!(shown[1].y > shown[0].y);
  // A double click on the second line selects its word.
  let second = Point::new(text.x + 5.0, text.bottom() - 4.0);
  for time in [5000, 5100] {
    window.pointer_motion_at(second, time);
    window.pointer_button_at(PointerButton::Left, true, time);
    window.pointer_button_at(PointerButton::Left, false, time + 10);
  }
  assert_eq!(copied(&window).as_deref(), Some("two"));
  assert_eq!(selection(&window, &[0, 0]).len(), 1, "the first line isn't");
}

#[test]
fn shift_click_after_new_text_extends_from_its_start() {
  let mut h = Harness::new();
  let words = h.cx.signal(String::from("hello world"));
  let window = label(&mut h, Text::new(move || words.get()).selectable(true));
  drag(&window, Point::new(1.0, 10.0), Point::new(190.0, 10.0), 0);
  assert_eq!(copied(&window).as_deref(), Some("hello world"));
  words.set(String::from("bye bye bye bye"));
  h.frame(&window);
  // Shift and a press at the start: nothing from the old caret on.
  window.set_modifiers(Modifiers {
    shift: true,
    ..Modifiers::default()
  });
  click(&window, 1.0, 1000);
  assert_eq!(copied(&window), None);
}

#[test]
fn a_double_click_in_a_password_selects_all_of_it() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("ab cd"));
  let row = Row::new().child(
    TextInput::new(text)
      .password(true)
      .style(Style::new().width(100.0)),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  // Past the dots: the last word, were it shown.
  click(&window, 99.0, 0);
  click(&window, 99.0, 100);
  window.key(&KeyEvent {
    key: Key::Character('x'),
    text: Some(String::from("x")),
    modifiers: Modifiers::default(),
    repeat: false,
  });
  assert_eq!(text.get(), "x", "where its spaces are stays hidden");
}

#[test]
fn a_span_change_that_keeps_the_text_keeps_the_selection() {
  let mut h = Harness::new();
  let bold = h.cx.signal(false);
  let words = h.cx.signal(String::from("world"));
  let spans = move || {
    let word = Span::new(words.get());
    vec![
      Span::new("hello "),
      if bold.get() { word.bold() } else { word },
    ]
  };
  let window = label(&mut h, Text::rich(spans).selectable(true));
  drag(&window, Point::new(1.0, 10.0), Point::new(190.0, 10.0), 0);
  assert_eq!(copied(&window).as_deref(), Some("hello world"));
  bold.set(true);
  h.frame(&window);
  assert_eq!(copied(&window).as_deref(), Some("hello world"));
  assert_eq!(selection(&window, &[0, 0]).len(), 1);
  // New text, longer, so the old bytes would still be there.
  words.set(String::from("there, everyone"));
  h.frame(&window);
  assert_eq!(copied(&window), None);
}
