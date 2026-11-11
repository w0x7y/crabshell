//! Calling the platform's listener: it may replace itself, and a panic in
//! it is logged and doesn't lose it. A small struct stands in for the
//! platform, which needs a compositor.

use crate::events::{Event, Listener, notify};
use crate::windows::WindowId;

#[derive(Default)]
struct Fake {
  listener: Option<Listener<Fake>>,
  heard: Vec<String>,
}

fn slot(fake: &mut Fake) -> &mut Option<Listener<Fake>> {
  &mut fake.listener
}

#[test]
fn a_panicking_listener_is_logged_and_kept() {
  let mut fake = Fake::default();
  fake.listener = Some(Box::new(|fake: &mut Fake, event: &Event| {
    if let Event::WindowClosed(_) = event {
      panic!("a bad handler");
    }
    fake.heard.push(format!("{event:?}"));
  }));
  let logs = super::capture_logs(|| {
    notify(&mut fake, slot, &Event::WindowClosed(WindowId(7)));
  });
  assert!(
    logs.contains("the platform's listener panicked on WindowClosed(WindowId(7)): a bad handler"),
    "{logs}"
  );
  assert!(fake.listener.is_some(), "the listener is put back");
  notify(&mut fake, slot, &Event::WindowClosed(WindowId(8)));
  notify(&mut fake, slot, &Event::OutputRemoved(output("DP-1")));
  assert_eq!(
    fake.heard.len(),
    1,
    "it still hears events: {:?}",
    fake.heard
  );
}

#[test]
fn a_listener_may_replace_itself_and_reach_its_owner() {
  let mut fake = Fake::default();
  fake.listener = Some(Box::new(|fake: &mut Fake, _: &Event| {
    fake.heard.push(String::from("first"));
    fake.listener = Some(Box::new(|fake: &mut Fake, _: &Event| {
      fake.heard.push(String::from("second"));
    }));
  }));
  notify(&mut fake, slot, &Event::OutputAdded(output("DP-1")));
  notify(&mut fake, slot, &Event::OutputAdded(output("DP-2")));
  notify(&mut fake, slot, &Event::OutputAdded(output("DP-3")));
  assert_eq!(fake.heard, ["first", "second", "second"]);
}

#[test]
fn with_no_listener_nothing_happens() {
  let mut fake = Fake::default();
  notify(&mut fake, slot, &Event::WindowClosed(WindowId(0)));
  assert!(fake.listener.is_none());
}

fn output(name: &str) -> crate::OutputInfo {
  crate::OutputInfo {
    name: name.to_owned(),
    mode: None,
    scale: 1,
  }
}
