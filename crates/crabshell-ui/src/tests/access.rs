//! What a screen reader is shown: roles, names, states, children, clicks
//! and the events heard while one listens.

use std::rc::Rc;
use std::task::Waker;

use super::{Counter, Harness, capture_logs};
use crate::{
  AccessEvent, AccessRole, AccessText, AccessValue, Accessibility, Button, Column, FloatingWindow,
  Icon, Key, KeyEvent, Lock, Modifiers, Output, Row, SessionLock, Size, Slider, Spacer, Style,
  Text, TextArea, TextInput, Window,
};

fn key(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

fn tab() -> KeyEvent {
  key(Key::Tab)
}

fn typed(text: &str) -> KeyEvent {
  KeyEvent {
    text: Some(text.to_owned()),
    ..key(Key::Character(text.chars().next().unwrap_or(' ')))
  }
}

/// A harness whose root context has the registry, as the shell's does.
fn harness() -> (Harness, Rc<Accessibility>) {
  let harness = Harness::new();
  let access = harness.cx.provide(Accessibility::default());
  (harness, access)
}

/// The counter bar's row: a button with a label, a spacer and a text.
fn counter(harness: &mut Harness, clicks: &Counter) -> Window {
  let count = harness.cx.signal(0);
  let clicks = clicks.clone();
  let panel = harness.panel().child(
    Row::new()
      .child(
        Button::new(Text::new(move || format!("clicked {} times", count.get()))).on_click(
          move |_| {
            clicks.hit();
            count.update(|n| *n += 1);
          },
        ),
      )
      .child(Spacer::new())
      .child(Text::new("hello")),
  );
  harness.window(panel, 300.0, 30.0)
}

fn take(access: &Accessibility) -> Vec<AccessEvent> {
  access.take_events(Waker::noop())
}

#[test]
fn a_counter_bar_shows_its_button_by_role_and_name() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let _window = counter(&mut harness, &clicks);
  let windows = access.windows();
  assert_eq!(windows.len(), 1);
  let frame = access.node(windows[0]).expect("the window");
  assert_eq!(frame.role, AccessRole::Frame);
  assert_eq!(frame.name, "crabshell");
  assert_eq!(frame.parent, None);
  let row = access.node(frame.children[0]).expect("the row");
  assert_eq!(row.role, AccessRole::Filler);
  assert_eq!(row.parent, Some(windows[0]));
  let roles: Vec<_> = row
    .children
    .iter()
    .map(|&id| access.node(id).expect("a child").role)
    .collect();
  assert_eq!(
    roles,
    [AccessRole::Button, AccessRole::Filler, AccessRole::Label]
  );
  let button = access.node(row.children[0]).expect("the button");
  assert_eq!(button.name, "clicked 0 times");
  assert_eq!(button.index, 0);
  assert!(button.focusable && button.enabled && button.shown && button.clickable);
  assert!(!button.focused);
  assert!(button.rect.width > 0.0);
  let label = access.node(row.children[2]).expect("the label");
  assert_eq!((label.name.as_str(), label.index), ("hello", 2));
  assert!(!label.clickable && !label.focusable);
}

#[test]
fn accessible_name_wins_over_the_text_and_follows_its_closure() {
  let (mut harness, access) = harness();
  let name = harness.cx.signal(String::from("volume"));
  let panel = harness.panel().child(
    Row::new()
      .child(Button::new(Icon::new("audio")).accessible_name(move || name.get()))
      .child(Button::new(Text::new("text")).accessible_name("named")),
  );
  let window = harness.window(panel, 300.0, 30.0);
  let frame = access.node(access.windows()[0]).expect("the window");
  let row = access.node(frame.children[0]).expect("the row");
  let names = |access: &Accessibility| -> Vec<String> {
    let row = access.node(row.children[0]).expect("a row").parent;
    let row = access.node(row.expect("a parent")).expect("the row");
    row
      .children
      .iter()
      .map(|&id| access.node(id).expect("a button").name)
      .collect()
  };
  assert_eq!(names(&access), ["volume", "named"]);
  // Heard only while a screen reader listens.
  assert!(take(&access).is_empty());
  access.listen(true);
  name.set(String::from("muted"));
  harness.frame(&window);
  assert_eq!(names(&access), ["muted", "named"]);
  assert_eq!(take(&access), [AccessEvent::NameChanged(row.children[0])]);
}

#[test]
fn a_label_change_renames_the_button_around_it() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  access.listen(true);
  let frame = access.node(access.windows()[0]).expect("the window");
  let row = access.node(frame.children[0]).expect("the row");
  let button = row.children[0];
  let label = access.node(button).expect("the button").children[0];
  assert!(access.click(button));
  harness.frame(&window);
  assert_eq!(clicks.get(), 1);
  assert_eq!(
    access.node(button).expect("the button").name,
    "clicked 1 times"
  );
  assert_eq!(
    take(&access),
    [
      AccessEvent::NameChanged(label),
      AccessEvent::NameChanged(button)
    ]
  );
}

#[test]
fn a_click_needs_an_enabled_shown_button() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let (on, shown) = (harness.cx.signal(true), harness.cx.signal(true));
  let hit = clicks.clone();
  let panel = harness.panel().child(
    Row::new()
      .child(
        Button::new(Text::new("a"))
          .enabled(move || on.get())
          .visible(move || shown.get())
          .on_click(move |_| hit.hit()),
      )
      .child(Text::new("b")),
  );
  let window = harness.window(panel, 300.0, 30.0);
  let frame = access.node(access.windows()[0]).expect("the window");
  let row = access.node(frame.children[0]).expect("the row");
  let (button, text) = (row.children[0], row.children[1]);
  assert!(!access.click(text), "a text has no action");
  on.set(false);
  harness.frame(&window);
  let state = access.node(button).expect("the button");
  assert!(!state.enabled && !state.focusable);
  assert!(!access.click(button));
  on.set(true);
  shown.set(false);
  harness.frame(&window);
  assert!(!access.click(button));
  // A hidden widget stays among its parent's children, and doesn't show.
  let parent = access.node(text).expect("the text").parent;
  let row = access.node(parent.expect("a parent")).expect("the row");
  assert_eq!(row.children, [button, text]);
  assert!(!access.node(button).expect("the button").shown);
  assert_eq!(clicks.get(), 0);
}

#[test]
fn showing_and_turning_a_button_on_are_heard() {
  let (mut harness, access) = harness();
  let (on, shown, label) = (
    harness.cx.signal(true),
    harness.cx.signal(true),
    harness.cx.signal(true),
  );
  let panel = harness.panel().child(
    Row::new().child(
      Button::new(
        Row::new()
          .child(Text::new("mute"))
          .child(Text::new("now").visible(move || label.get())),
      )
      .enabled(move || on.get())
      .visible(move || shown.get()),
    ),
  );
  let window = harness.window(panel, 300.0, 30.0);
  let row = access
    .node(access.windows()[0])
    .expect("the window")
    .children[0];
  let button = access.node(row).expect("the row").children[0];
  assert_eq!(access.node(button).expect("the button").name, "mute now");
  access.listen(true);
  on.set(false);
  harness.frame(&window);
  shown.set(false);
  harness.frame(&window);
  shown.set(true);
  harness.frame(&window);
  label.set(false);
  harness.frame(&window);
  let inner = access.node(button).expect("the button").children[0];
  let now = access.node(inner).expect("the labels").children[1];
  assert_eq!(
    take(&access),
    [
      AccessEvent::Enabled(button, false),
      AccessEvent::Shown(button, false),
      AccessEvent::Shown(button, true),
      AccessEvent::Shown(now, false),
      AccessEvent::NameChanged(button),
    ]
  );
  assert_eq!(access.node(button).expect("the button").name, "mute");
}

#[test]
fn keyed_children_coming_and_going_are_heard_once_laid_out() {
  let (mut harness, access) = harness();
  let items = harness.cx.signal(vec![1, 2]);
  let panel = harness.panel().child(Row::new().children_keyed(
    move || items.get(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  ));
  // Built while a screen reader listens: only the window is news.
  access.listen(true);
  let window = harness.window(panel, 300.0, 30.0);
  assert_eq!(take(&access), [AccessEvent::WindowsChanged]);
  let row = access
    .node(access.windows()[0])
    .expect("the window")
    .children[0];
  let before = access.node(row).expect("the row").children;
  items.set(vec![2, 3]);
  harness.frame(&window);
  let after = access.node(row).expect("the row").children;
  assert_eq!(after[0], before[1]);
  assert_eq!(
    take(&access),
    [
      AccessEvent::Children {
        parent: row,
        child: before[0],
        index: 0,
        added: false
      },
      AccessEvent::Children {
        parent: row,
        child: after[1],
        index: 1,
        added: true
      },
    ]
  );
  drop(window);
  assert_eq!(take(&access), [AccessEvent::WindowsChanged]);
}

#[test]
fn a_floating_window_title_and_a_placeholder_rename_them() {
  let (mut harness, access) = harness();
  let (title, shown) = (
    harness.cx.signal(String::from("About")),
    harness.cx.signal(true),
  );
  let placeholder = harness.cx.signal(String::from("search"));
  let window = Window::floating(
    &harness.cx,
    FloatingWindow::new()
      .title(move || title.get())
      .visible(move || shown.get())
      .child(
        TextInput::new(harness.cx.signal(String::new())).placeholder(move || placeholder.get()),
      ),
  );
  window.set_size(Size::new(200.0, 100.0));
  harness.frame(&window);
  let root = access.windows()[0];
  let field = access.node(root).expect("the window").children[0];
  assert_eq!(access.node(root).expect("the window").name, "About");
  access.listen(true);
  title.set(String::from("Settings"));
  placeholder.set(String::from("find"));
  harness.frame(&window);
  assert_eq!(
    take(&access),
    [
      AccessEvent::NameChanged(root),
      AccessEvent::NameChanged(field)
    ]
  );
  assert_eq!(access.node(field).expect("the field").name, "find");
  // Hidden, the window is unknown, and comes back with its parts.
  shown.set(false);
  harness.frame(&window);
  assert_eq!(take(&access), [AccessEvent::WindowsChanged]);
  assert!(access.windows().is_empty());
  assert!(access.node(root).is_none() && access.node(field).is_none());
  shown.set(true);
  harness.frame(&window);
  assert_eq!(access.windows(), [root]);
}

#[test]
fn while_locked_only_the_lock_is_read_or_clicked() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  let panel = access.windows()[0];
  let button = access
    .node(access.node(panel).expect("the panel").children[0])
    .expect("the row")
    .children[0];
  access.listen(true);
  let lock = Lock::new(
    &harness.cx,
    SessionLock::new().surface(|_, _| Text::new("locked")),
  );
  let surface = lock.surface(&Output::new("DP-1")).expect("the lock lives");
  surface.set_size(Size::new(300.0, 30.0));
  harness.frame(&surface);
  assert_eq!(take(&access), [AccessEvent::WindowsChanged]);
  let listed = access.windows();
  assert_eq!(listed.len(), 1);
  assert_eq!(
    access.node(listed[0]).expect("the lock").role,
    AccessRole::Frame
  );
  assert!(access.node(panel).is_none() && access.node(button).is_none());
  assert!(!access.click(button), "clicked behind the lock");
  harness.frame(&window);
  assert_eq!(clicks.get(), 0);
  drop(surface);
  assert_eq!(take(&access), [AccessEvent::WindowsChanged]);
  assert_eq!(access.windows(), [panel]);
  assert!(access.click(button));
  assert_eq!(clicks.get(), 1);
}

#[test]
fn tab_and_the_keyboard_coming_and_going_are_heard() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  let root = access.windows()[0];
  let button = access
    .node(access.node(root).expect("the window").children[0])
    .expect("the row")
    .children[0];
  // Nothing is kept while nobody listens.
  window.keyboard_enter();
  window.key(&tab());
  assert!(take(&access).is_empty());
  window.keyboard_leave();
  access.listen(true);
  window.keyboard_enter();
  assert_eq!(
    take(&access),
    [
      AccessEvent::Active(root, true),
      AccessEvent::Focused(button, true)
    ]
  );
  assert!(access.node(button).expect("the button").focused);
  assert!(access.node(root).expect("the window").active);
  window.key(&tab());
  assert!(take(&access).is_empty(), "Tab stays on the only button");
  window.keyboard_leave();
  assert_eq!(
    take(&access),
    [
      AccessEvent::Focused(button, false),
      AccessEvent::Active(root, false)
    ]
  );
  // What nobody took goes when the screen reader does.
  window.keyboard_enter();
  access.listen(false);
  assert!(take(&access).is_empty());
}

#[test]
fn a_slider_is_one_object_that_takes_the_focus() {
  let (mut harness, access) = harness();
  let value = harness.cx.signal(0.5);
  let panel = harness.panel().child(
    Row::new()
      .child(Slider::new(value).accessible_name("brightness"))
      .child(TextInput::new(harness.cx.signal(String::new())).placeholder("search")),
  );
  let window = harness.window(panel, 300.0, 30.0);
  access.listen(true);
  let row = access
    .node(
      access
        .node(access.windows()[0])
        .expect("the window")
        .children[0],
    )
    .expect("the row");
  let slider = access.node(row.children[0]).expect("the slider");
  assert_eq!(slider.role, AccessRole::Slider);
  assert_eq!(slider.name, "brightness");
  assert!(slider.children.is_empty() && slider.focusable);
  let field = access.node(row.children[1]).expect("the field");
  assert_eq!(
    (field.role, field.name.as_str()),
    (AccessRole::Entry, "search")
  );
  assert!(field.editable && !field.multi_line);
  window.keyboard_enter();
  take(&access);
  window.key(&tab());
  assert_eq!(take(&access), [AccessEvent::Focused(row.children[0], true)]);
}

#[test]
fn a_window_leaves_the_list_when_dropped() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  let root = access.windows()[0];
  drop(window);
  assert!(access.windows().is_empty());
  assert!(access.node(root).is_none());
  assert!(!access.click(root));
}

#[test]
fn a_panicking_accessible_name_is_logged_with_its_path() {
  let (mut harness, _access) = harness();
  let logs = capture_logs(|| {
    let panel = harness.panel().child(
      Row::new().child(
        Button::new(Text::new("ok"))
          .style(Style::new())
          .accessible_name(|| -> String { panic!("no name") }),
      ),
    );
    let _window = harness.window(panel, 300.0, 30.0);
  });
  assert!(
    logs.contains(
      "PanelWindow[DP-1]/Row/Button[0].accessible_name panicked and is disabled: no name"
    ),
    "{logs}"
  );
}

/// A window's widgets in a column, as a screen reader finds them.
fn widgets(access: &Accessibility) -> Vec<crate::AccessId> {
  let root = access.node(access.windows()[0]).expect("the window");
  access.node(root.children[0]).expect("the column").children
}

#[test]
fn a_field_reads_its_text_and_caret_and_a_password_only_its_length() {
  let (mut harness, access) = harness();
  let panel = harness.panel().child(
    Column::new()
      .child(TextInput::new(harness.cx.signal("héllo wörld".to_owned())))
      .child(TextInput::new(harness.cx.signal("s3cret".to_owned())).password(true))
      .child(TextArea::new(harness.cx.signal("one\ntwo".to_owned()))),
  );
  let _window = harness.window(panel, 300.0, 200.0);
  let [field, password, area] = widgets(&access)[..] else {
    panic!("three widgets");
  };
  let field = access
    .node(field)
    .expect("the field")
    .text
    .expect("its text");
  assert_eq!(
    field,
    AccessText {
      text: "héllo wörld".to_owned(),
      caret: 11,
      selection: None,
    }
  );
  assert_eq!(field.segment(2, 1), Some((0, 5)), "a word");
  assert_eq!(
    field.segment(11, 0),
    Some((11, 11)),
    "no character past the end"
  );
  assert_eq!(field.segment(11, 3), Some((0, 11)), "the line at the end");
  assert_eq!(field.segment(0, 9), None);
  let password = access.node(password).expect("the password");
  assert_eq!(password.role, AccessRole::PasswordText);
  let dots = password.text.expect("its length");
  assert_eq!((dots.text.as_str(), dots.caret), ("••••••", 6));
  let area = access.node(area).expect("the area").text.expect("its text");
  assert_eq!(area.segment(5, 3), Some((4, 7)), "the second line");
  assert_eq!(
    area.segment(1, 3),
    Some((0, 4)),
    "the first, with its break"
  );
}

#[test]
fn typing_moving_and_setting_a_field_are_heard_and_a_password_as_dots() {
  let (mut harness, access) = harness();
  let text = harness.cx.signal("hello".to_owned());
  let panel = harness.panel().child(
    Column::new()
      .child(TextInput::new(text))
      .child(TextInput::new(harness.cx.signal(String::new())).password(true)),
  );
  let window = harness.window(panel, 300.0, 100.0);
  let [field, password] = widgets(&access)[..] else {
    panic!("two widgets");
  };
  window.keyboard_enter();
  window.key(&tab());
  window.key(&typed("x"));
  assert!(take(&access).is_empty(), "kept while nobody listens");
  access.listen(true);
  window.key(&typed("y"));
  let inserted = |start, text: &str| AccessEvent::TextChanged {
    id: field,
    start,
    text: text.to_owned(),
    inserted: true,
  };
  assert_eq!(
    take(&access),
    [inserted(6, "y"), AccessEvent::CaretMoved(field, 7)]
  );
  window.key(&key(Key::Backspace));
  assert_eq!(
    take(&access),
    [
      AccessEvent::TextChanged {
        id: field,
        start: 6,
        text: "y".to_owned(),
        inserted: false,
      },
      AccessEvent::CaretMoved(field, 6)
    ]
  );
  window.key(&key(Key::Left));
  assert_eq!(take(&access), [AccessEvent::CaretMoved(field, 5)]);
  harness.frame(&window);
  take(&access);
  // From outside, the text is replaced; the caret goes to its end.
  text.set("help".to_owned());
  harness.frame(&window);
  assert_eq!(
    take(&access),
    [
      AccessEvent::TextChanged {
        id: field,
        start: 3,
        text: "lox".to_owned(),
        inserted: false,
      },
      inserted(3, "p"),
      AccessEvent::CaretMoved(field, 4)
    ]
  );
  window.key(&tab());
  take(&access);
  window.key(&typed("z"));
  assert_eq!(
    take(&access),
    [
      AccessEvent::TextChanged {
        id: password,
        start: 0,
        text: "•".to_owned(),
        inserted: true,
      },
      AccessEvent::CaretMoved(password, 1)
    ]
  );
  // A field without the focus has no caret to follow.
  text.set("hi".to_owned());
  harness.frame(&window);
  assert_eq!(
    take(&access),
    [
      AccessEvent::TextChanged {
        id: field,
        start: 1,
        text: "elp".to_owned(),
        inserted: false,
      },
      inserted(1, "i")
    ]
  );
}

#[test]
fn a_slider_reads_and_writes_its_value_and_refuses_when_off() {
  let (mut harness, access) = harness();
  let value = harness.cx.signal(4.0);
  let on = harness.cx.signal(true);
  let panel = harness.panel().child(
    Column::new().child(
      Slider::new(value)
        .range(0.0, 10.0)
        .step(2.0)
        .enabled(move || on.get()),
    ),
  );
  let window = harness.window(panel, 300.0, 100.0);
  let slider = widgets(&access)[0];
  let read = |access: &Accessibility| access.node(slider).expect("the slider").value;
  assert_eq!(
    read(&access),
    Some(AccessValue {
      min: 0.0,
      max: 10.0,
      current: 4.0,
      increment: 2.0,
    })
  );
  access.listen(true);
  assert!(access.set_value(slider, 7.3), "on a step");
  harness.frame(&window);
  assert_eq!(value.get_untracked(), 8.0);
  assert_eq!(take(&access), [AccessEvent::ValueChanged(slider)]);
  assert!(access.set_value(slider, 50.0), "within the range");
  assert!(!access.set_value(slider, f64::NAN));
  harness.frame(&window);
  assert_eq!(value.get_untracked(), 10.0);
  assert!(!access.set_value(access.windows()[0], 1.0), "not a slider");
  on.set(false);
  harness.frame(&window);
  take(&access);
  let node = access.node(slider).expect("the slider");
  assert!(!node.enabled && !node.focusable);
  assert!(!access.set_value(slider, 2.0), "a slider that is off");
  harness.frame(&window);
  assert_eq!(value.get_untracked(), 10.0);
  assert!(take(&access).is_empty());
}

#[test]
fn a_panicking_slider_range_read_by_a_screen_reader_is_logged() {
  let (mut harness, access) = harness();
  let boom = harness.cx.signal(false);
  let panel = harness.panel().child(
    Column::new()
      .child(Slider::new(harness.cx.signal(4.0)).range(0.0, move || {
        if boom.get() {
          panic!("no end");
        }
        10.0
      }))
      .child(Slider::new(harness.cx.signal(4.0)).range(2.0, || -> f32 { panic!("never") })),
  );
  let window = harness.window(panel, 300.0, 100.0);
  let [slider, never] = widgets(&access)[..] else {
    panic!("two sliders");
  };
  // One whose end never had a value holds only its start.
  let value = access
    .node(never)
    .expect("the slider")
    .value
    .expect("its value");
  assert_eq!((value.min, value.max, value.current), (2.0, 2.0, 2.0));
  boom.set(true);
  let logs = capture_logs(|| {
    let value = access.node(slider).expect("the slider").value;
    assert_eq!(value.map(|value| value.max), Some(10.0), "the last end");
  });
  assert!(
    logs.contains("in PanelWindow[DP-1]/Column/Slider[0] panicked and is disabled: no end"),
    "{logs}"
  );
  harness.frame(&window);
  assert!(access.set_value(slider, 3.0));
}
