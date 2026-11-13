//! Which windows go when an output goes, with fake outputs: numbers stand
//! in for `wl_output` objects, and `()` for surfaces.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Runtime;
use crabshell_ui::{Cx, Popup, Rect, Text, Window};

use crate::windows::{WindowId, Windows};

/// A window for output `name` under `cx`.
fn window(cx: &Cx, name: &str) -> Window {
  Window::new(cx, super::panel(name).child(Text::new(name.to_owned())))
}

/// Gives the window at `index` a surface on fake output `output`.
fn show(windows: &mut Windows<(), u32>, index: usize, output: u32) {
  let entry = &mut windows.entries[index];
  entry.surface = Some(());
  entry.output = Some(output);
}

fn outputs_left(windows: &Windows<(), u32>) -> Vec<String> {
  windows
    .entries
    .iter()
    .map(|entry| entry.window.output().name())
    .collect()
}

#[test]
fn a_removed_output_drops_its_windows_and_their_nodes() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut windows: Windows<(), u32> = Windows::new();
  windows.add(window(&cx, "DP-1"));
  show(&mut windows, 0, 1);
  runtime.flush();
  let nodes = runtime.node_count();

  // Two windows on DP-2, output 2: one shown, one not shown yet.
  windows.add(window(&cx, "DP-2"));
  show(&mut windows, 1, 2);
  windows.add(window(&cx, "DP-2"));
  runtime.flush();
  assert!(runtime.node_count() > nodes);

  assert_eq!(windows.drop_output(&2, "DP-2"), 2);
  runtime.flush();
  assert_eq!(outputs_left(&windows), ["DP-1"]);
  assert_eq!(runtime.node_count(), nodes, "their owners were disposed");
}

#[test]
fn windows_are_dropped_by_the_output_they_were_shown_on_not_its_name() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut windows: Windows<(), u32> = Windows::new();
  // DP-2 was unplugged and plugged back in as output 3 before output 2's
  // removal arrived: the new window stays.
  windows.add(window(&cx, "DP-2"));
  show(&mut windows, 0, 2);
  windows.add(window(&cx, "DP-2"));
  show(&mut windows, 1, 3);
  windows.add(window(&cx, "HDMI-A-1"));
  assert_eq!(windows.drop_output(&2, "DP-2"), 1);
  assert_eq!(windows.entries[0].output, Some(3));
  assert_eq!(outputs_left(&windows), ["DP-2", "HDMI-A-1"]);
  // A window waiting for an output of another name stays too.
  assert_eq!(windows.drop_output(&3, "DP-2"), 1);
  assert_eq!(outputs_left(&windows), ["HDMI-A-1"]);
}

#[test]
fn a_popup_stays_with_its_panel_on_an_output_plugged_back_in() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut windows: Windows<(), u32> = Windows::new();
  // DP-2's new panel, on output 3, opened a popup before output 2's
  // removal arrived.
  let panel = windows.add(window(&cx, "DP-2"));
  show(&mut windows, 0, 3);
  windows.add_popup(panel, popup(&cx, "DP-2")).expect("held");
  assert_eq!(windows.drop_output(&2, "DP-2"), 0);
  assert_eq!(windows.entries.len(), 2);
}

#[test]
fn windows_are_added_and_removed_by_id() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut windows: Windows<(), u32> = Windows::new();
  let first = windows.add(window(&cx, "DP-1"));
  let second = windows.add(window(&cx, "DP-1"));
  assert_ne!(first, second);
  assert!(windows.remove(first));
  assert!(!windows.remove(first), "already gone");
  assert!(windows.get(first).is_none());
  assert_eq!(windows.get(second).map(|entry| entry.id), Some(second));
}

/// The numbers of the fake surfaces dropped, in order.
type DropLog = Rc<RefCell<Vec<u32>>>;

/// A fake surface that records, by number, when it is dropped.
struct Logged(u32, DropLog);

impl Drop for Logged {
  fn drop(&mut self) {
    self.1.borrow_mut().push(self.0);
  }
}

/// Panel 1 on output 1, popup 2 opened from it and popup 3 from popup 2,
/// each with a surface that logs its drop into the returned list.
fn chain(cx: &Cx) -> (Windows<Logged, u32>, [WindowId; 3], DropLog) {
  let dropped = Rc::new(RefCell::new(Vec::new()));
  let mut windows: Windows<Logged, u32> = Windows::new();
  let panel = windows.add(window(cx, "DP-1"));
  let first = windows
    .add_popup(panel, popup(cx, "DP-1"))
    .expect("the panel is held");
  let second = windows
    .add_popup(first, popup(cx, "DP-1"))
    .expect("the popup is held");
  for (number, entry) in (1..).zip(windows.entries.iter_mut()) {
    entry.surface = Some(Logged(number, Rc::clone(&dropped)));
    entry.output = Some(1);
  }
  (windows, [panel, first, second], dropped)
}

/// A popup for output `name`, as one opened from a panel there is.
fn popup(cx: &Cx, name: &str) -> Popup {
  Popup {
    window: window(cx, name),
    anchor: Rect::default(),
  }
}

#[test]
fn removing_a_window_drops_the_popups_it_opened_topmost_first() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let (mut windows, [panel, ..], dropped) = chain(&cx);
  assert!(windows.remove(panel));
  assert_eq!(
    *dropped.borrow(),
    [3, 2, 1],
    "a popup goes before its opener"
  );
  assert!(windows.entries.is_empty());

  let (mut windows, [_, first, _], dropped) = chain(&cx);
  assert!(windows.remove(first));
  assert_eq!(*dropped.borrow(), [3, 2]);
  assert_eq!(windows.entries.len(), 1, "the panel stays");
}

#[test]
fn a_removed_output_drops_its_popups_topmost_first() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let (mut windows, _, dropped) = chain(&cx);
  assert_eq!(windows.drop_output(&1, "DP-1"), 3);
  assert_eq!(*dropped.borrow(), [3, 2, 1]);
}

#[test]
fn opening_a_popup_closes_every_popup_outside_its_openers_chain() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  // A sibling of the open submenu: the submenu goes, its opener stays.
  let (mut windows, [panel, first, _], dropped) = chain(&cx);
  let sibling = windows.add_popup(first, popup(&cx, "DP-1")).expect("held");
  assert_eq!(*dropped.borrow(), [3]);
  let openers: Vec<_> = windows
    .entries
    .iter()
    .map(|entry| entry.opener.map(|(opener, _)| opener))
    .collect();
  assert_eq!(openers, [None, Some(panel), Some(first)]);
  assert_eq!(windows.entries[2].id, sibling);

  // Another popup from the panel: the whole open chain goes, topmost
  // first.
  windows.add_popup(panel, popup(&cx, "DP-1")).expect("held");
  assert_eq!(*dropped.borrow(), [3, 2]);
  assert_eq!(windows.entries.len(), 2);
}

#[test]
fn a_popup_from_a_window_not_held_is_dropped_with_its_nodes() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut windows: Windows<(), u32> = Windows::new();
  let panel = windows.add(window(&cx, "DP-1"));
  windows.remove(panel);
  runtime.flush();
  let nodes = runtime.node_count();
  let popup = popup(&cx, "DP-1");
  runtime.flush();
  assert!(runtime.node_count() > nodes);
  assert_eq!(windows.add_popup(panel, popup), None);
  runtime.flush();
  assert_eq!(runtime.node_count(), nodes);
  assert!(windows.entries.is_empty());
}
