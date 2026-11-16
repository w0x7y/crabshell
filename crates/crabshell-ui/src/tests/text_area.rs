//! Multi-line text areas: lines, wrapping, moving by line and page, Enter
//! and Ctrl+Enter, scrolling the caret into view and input methods.

use crabshell_reactive::Signal;

use super::{Harness, capture_logs, node_at, rect_at};
use crate::{
  Clipboard, Column, Content, Key, KeyEvent, Modifiers, Point, PointerButton, Rect, Row,
  ScrollView, Style, TextArea, Window,
};

/// The default font's line height.
const LINE: f32 = 16.0;

fn key(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

fn typed(text: &str) -> KeyEvent {
  KeyEvent {
    text: Some(text.to_owned()),
    ..key(Key::Character(text.chars().next().unwrap_or(' ')))
  }
}

fn with(key_: Key, modifiers: Modifiers) -> KeyEvent {
  KeyEvent {
    modifiers,
    ..key(key_)
  }
}

const CTRL: Modifiers = Modifiers {
  ctrl: true,
  alt: false,
  shift: false,
  logo: false,
};

/// A 300 by 200 window with the keyboard and a focused area editing
/// `text`, 100 wide unless `area` says otherwise.
fn area(h: &mut Harness, text: Signal<String>, area: impl FnOnce(TextArea) -> TextArea) -> Window {
  let column = Column::new().child(area(
    TextArea::new(text)
      .focus(true)
      .style(Style::new().width(100.0)),
  ));
  let window = h.window(h.panel().child(column), 300.0, 200.0);
  window.keyboard_enter();
  h.frame(&window);
  window
}

/// The area's rectangle and its caret.
fn shown(window: &Window) -> (Rect, Rect) {
  let view = window.view();
  let node = node_at(view.root(), &[0, 0]);
  match node.content() {
    Content::Input(input) => (node.rect(), input.caret.expect("a caret")),
    _ => panic!("not an area"),
  }
}

/// Presses `key` and returns where the caret is in the text.
fn press(h: &mut Harness, window: &Window, event: KeyEvent) -> usize {
  window.key(&event);
  h.frame(window);
  window.field().expect("the focused area").cursor
}

#[test]
fn enter_breaks_a_line_and_the_area_is_as_tall_as_its_lines() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let window = area(&mut h, text, |area| area);
  assert_eq!(shown(&window).0.height, LINE, "one line when empty");
  window.key(&typed("a"));
  window.key(&key(Key::Enter));
  window.key(&typed("b"));
  h.frame(&window);
  assert_eq!(text.get(), "a\nb");
  let (rect, caret) = shown(&window);
  assert_eq!(rect.height, 2.0 * LINE);
  assert_eq!(caret.y, rect.y + LINE, "the caret is on the second line");
  text.set(String::from("a\r\nb\nc"));
  h.frame(&window);
  assert_eq!(shown(&window).0.height, 3.0 * LINE);
  assert_eq!(window.field().expect("a field").text, "a\nb\nc");
}

#[test]
fn long_lines_wrap_at_the_width_unless_wrap_is_off() {
  let mut h = Harness::new();
  let words = "word ".repeat(20);
  let text = h.cx.signal(words.clone());
  let window = area(&mut h, text, |area| area);
  let (rect, _) = shown(&window);
  assert_eq!(rect.width, 100.0);
  assert!(rect.height > 3.0 * LINE, "{rect:?}");

  // In a row, which doesn't stretch it, it takes the default width.
  let text = h.cx.signal(words.clone());
  let row = Row::new().child(TextArea::new(text));
  let window = h.window(h.panel().child(row), 300.0, 200.0);
  assert_eq!(rect_at(&window, &[0, 0]).width, 200.0);

  let text = h.cx.signal(words);
  let window = area(&mut h, text, |area| area.wrap(false).style(Style::new()));
  let (rect, _) = shown(&window);
  assert_eq!(rect.height, LINE);
  assert!(rect.width > 200.0, "as wide as its line: {rect:?}");
}

#[test]
fn up_and_down_move_by_line_and_keep_the_caret_across() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef\nab\nabcdef"));
  let window = area(&mut h, text, |area| area);
  press(&mut h, &window, with(Key::Home, CTRL));
  assert_eq!(press(&mut h, &window, key(Key::End)), 6);
  assert_eq!(
    press(&mut h, &window, key(Key::Down)),
    9,
    "the short line's end"
  );
  assert_eq!(press(&mut h, &window, key(Key::Down)), 16, "back across");
  assert_eq!(press(&mut h, &window, key(Key::Up)), 9);
  assert_eq!(press(&mut h, &window, key(Key::Up)), 6, "still across");
  assert_eq!(
    press(&mut h, &window, key(Key::Up)),
    0,
    "above the first line"
  );
  // Shift selects as it goes, still across.
  let shift = Modifiers {
    shift: true,
    ..Modifiers::default()
  };
  press(&mut h, &window, with(Key::Down, shift));
  let state = window.field().expect("a field");
  assert_eq!((state.anchor, state.cursor), (0, 9));
}

#[test]
fn home_and_end_reach_the_ends_of_a_wrapped_line_and_ctrl_the_text() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("aaaa bbbb"));
  let window = area(&mut h, text, |area| area.style(Style::new().width(45.0)));
  let (rect, caret) = shown(&window);
  assert_eq!(rect.height, 2.0 * LINE, "one word a line");
  assert_eq!(caret.y, rect.y + LINE);
  assert_eq!(press(&mut h, &window, key(Key::Home)), 5);
  assert_eq!(press(&mut h, &window, key(Key::Up)), 0);
  // The space the line wraps at hangs past its end.
  assert_eq!(press(&mut h, &window, key(Key::End)), 4);
  let (rect, caret) = shown(&window);
  assert_eq!(caret.y, rect.y);
  assert!(caret.x > rect.x + 20.0, "{caret:?}");
  assert_eq!(press(&mut h, &window, with(Key::End, CTRL)), 9);
  assert_eq!(press(&mut h, &window, with(Key::Home, CTRL)), 0);

  // A word too long for a line breaks inside it: the end of the first
  // line is the start of the second, and the caret stays on the first.
  text.set("a".repeat(12));
  h.frame(&window);
  press(&mut h, &window, with(Key::Home, CTRL));
  let end = press(&mut h, &window, key(Key::End));
  assert!(end > 0 && end < 12, "{end}");
  let (rect, caret) = shown(&window);
  assert_eq!(caret.y, rect.y);
  // A click past the end of the first line puts it there too, unlike
  // Home on the second, which reaches the same byte.
  press(&mut h, &window, key(Key::Down));
  assert_eq!(press(&mut h, &window, key(Key::Home)), end);
  assert_eq!(shown(&window).1.y, rect.y + LINE);
  window.pointer_motion_at(Point::new(rect.right() - 1.0, rect.y + 4.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_button_at(PointerButton::Left, false, 10);
  h.frame(&window);
  assert_eq!(window.field().expect("a field").cursor, end);
  assert_eq!(shown(&window).1.y, rect.y);
}

#[test]
fn page_keys_move_ten_lines_or_a_scroll_view_high() {
  let mut h = Harness::new();
  let lines: Vec<String> = (0..30).map(|n| format!("{n:02}")).collect();
  let lines = lines.join("\n");
  let text = h.cx.signal(lines.clone());
  let window = area(&mut h, text, |area| area);
  press(&mut h, &window, with(Key::Home, CTRL));
  // Each line is three bytes with its break.
  assert_eq!(press(&mut h, &window, key(Key::PageDown)), 30);
  assert_eq!(press(&mut h, &window, key(Key::PageUp)), 0);

  let text = h.cx.signal(lines);
  let view = ScrollView::new(
    TextArea::new(text)
      .focus(true)
      .style(Style::new().width(100.0)),
  )
  .style(Style::new().height(3.0 * LINE));
  let window = h.window(h.panel().child(Column::new().child(view)), 300.0, 200.0);
  window.keyboard_enter();
  press(&mut h, &window, with(Key::Home, CTRL));
  assert_eq!(press(&mut h, &window, key(Key::PageDown)), 9);
}

#[test]
fn the_caret_leaving_a_scroll_view_scrolls_it_the_least_that_shows_it() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("1\n2\n3\n4\n5"));
  let offset = h.cx.signal(0.0_f32);
  let view = ScrollView::new(
    TextArea::new(text)
      .focus(true)
      .style(Style::new().width(100.0)),
  )
  .offset(offset)
  .style(Style::new().height(2.0 * LINE));
  let window = h.window(h.panel().child(Column::new().child(view)), 300.0, 200.0);
  window.keyboard_enter();
  window.key(&with(Key::Home, CTRL));
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  window.key(&with(Key::End, CTRL));
  // One layout scrolls it, with no frame showing the caret outside.
  h.frame(&window);
  assert_eq!(offset.get(), 3.0 * LINE);
  let view = node_at(window.view().root(), &[0, 0]).rect();
  let caret = match node_at(window.view().root(), &[0, 0, 0]).content() {
    Content::Input(input) => input.caret.expect("a caret"),
    _ => panic!("not an area"),
  };
  assert_eq!(caret.bottom(), view.bottom());
  window.key(&key(Key::Up));
  h.frame(&window);
  assert_eq!(offset.get(), 3.0 * LINE, "still in view: nothing scrolls");
  // Up to the second line, which starts 16 down.
  window.key(&key(Key::Up));
  window.key(&key(Key::Up));
  h.frame(&window);
  assert_eq!(offset.get(), LINE);
  assert!(!window.needs_layout());
}

#[test]
fn ctrl_enter_runs_on_accept_and_a_panic_there_is_logged_with_its_path() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("a"));
  let window = area(&mut h, text, |area| {
    area.on_accept(|_, text| panic!("accept {text}"))
  });
  let logs = capture_logs(|| {
    window.key(&with(Key::Enter, CTRL));
  });
  assert!(
    logs.contains(
      "handler PanelWindow[DP-1]/Column/TextArea[0].on_accept panicked and is disabled: accept a"
    ),
    "{logs}"
  );
  window.key(&key(Key::Enter));
  assert_eq!(text.get(), "a\n", "the area still edits");
}

#[test]
fn input_methods_and_pastes_keep_line_breaks_and_dragging_selects_lines() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let window = area(&mut h, text, |area| area);
  assert!(window.field().expect("a field").multiline);
  window.commit_text("a\nb");
  window.paste("\r\nc");
  h.frame(&window);
  assert_eq!(text.get(), "a\nb\nc");
  let (rect, _) = shown(&window);
  window.pointer_motion_at(Point::new(rect.x, rect.y + 2.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(rect.right(), rect.y + LINE + 2.0), 10);
  window.pointer_button_at(PointerButton::Left, false, 20);
  h.frame(&window);
  let copied = window.key(&KeyEvent {
    modifiers: CTRL,
    ..typed("c")
  });
  assert_eq!(copied, Some(Clipboard::Copy(String::from("a\nb"))));
  let selection = match node_at(window.view().root(), &[0, 0]).content() {
    Content::Input(input) => input.selection.to_vec(),
    _ => panic!("not an area"),
  };
  assert_eq!(selection.len(), 2, "{selection:?}");
}

#[test]
fn dropping_an_area_frees_what_it_made() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let before = h.runtime.node_count();
  let window = area(&mut h, text, |area| area.on_accept(|_, _| {}));
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn other_moves_forget_the_end_of_a_wrapped_line_and_the_place_across() {
  let mut h = Harness::new();
  let text = h.cx.signal("a".repeat(12));
  let window = area(&mut h, text, |area| area.style(Style::new().width(45.0)));
  press(&mut h, &window, with(Key::Home, CTRL));
  let end = press(&mut h, &window, key(Key::End));
  press(&mut h, &window, key(Key::Right));
  // Back at the same byte by an arrow: the start of the second line.
  assert_eq!(press(&mut h, &window, key(Key::Left)), end);
  let (rect, caret) = shown(&window);
  assert_eq!(caret.y, rect.y + LINE);

  let text = h.cx.signal(String::from("abcdef\nab\nabcdef"));
  let window = area(&mut h, text, |area| area);
  press(&mut h, &window, with(Key::Home, CTRL));
  press(&mut h, &window, key(Key::End));
  assert_eq!(press(&mut h, &window, key(Key::Down)), 9);
  press(&mut h, &window, key(Key::Left));
  press(&mut h, &window, key(Key::Right));
  // From where the arrows left it, not from the first line's end.
  assert_eq!(press(&mut h, &window, key(Key::Down)), 12);

  // A click at the same byte forgets it too.
  press(&mut h, &window, with(Key::Home, CTRL));
  press(&mut h, &window, key(Key::End));
  assert_eq!(press(&mut h, &window, key(Key::Down)), 9);
  let (_, caret) = shown(&window);
  window.pointer_motion_at(Point::new(caret.x + 30.0, caret.y + 4.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_button_at(PointerButton::Left, false, 10);
  h.frame(&window);
  assert_eq!(window.field().expect("a field").cursor, 9);
  assert_eq!(press(&mut h, &window, key(Key::Down)), 12);
}

#[test]
fn focusing_a_field_below_a_scroll_views_end_scrolls_it_into_view() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("x"));
  let offset = h.cx.signal(0.0_f32);
  let content = Column::new()
    .child(crate::Icon::new("x").size(100))
    .child(TextArea::new(text));
  let view = ScrollView::new(content)
    .offset(offset)
    .style(Style::new().height(2.0 * LINE));
  let window = h.window(h.panel().child(Column::new().child(view)), 300.0, 200.0);
  window.keyboard_enter();
  window.key(&key(Key::Tab));
  h.frame(&window);
  // The area's one line ends 100 + 16 down, and the view is 32 high.
  assert_eq!(offset.get(), 100.0 + LINE - 2.0 * LINE);
}

#[test]
fn an_area_removed_after_an_edit_is_skipped_by_the_next_layout() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let shown = h.cx.signal(true);
  let list = Column::new().children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_, _| TextArea::new(text).focus(true),
  );
  let view = ScrollView::new(list).style(Style::new().height(2.0 * LINE));
  let window = h.window(h.panel().child(view), 300.0, 200.0);
  window.keyboard_enter();
  window.key(&typed("a"));
  shown.set(false);
  h.frame(&window);
  assert_eq!(window.focused(), None);
}
