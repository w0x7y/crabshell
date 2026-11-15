use super::{Lines, MAX_LINE};
use crate::testing::capture_logs;

fn push(lines: &mut Lines, bytes: &[u8]) -> Vec<String> {
  lines.push(bytes, &"test")
}

#[test]
fn a_marker_of_several_bytes_is_found_where_a_read_cut_it() {
  let mut lines = Lines::new("\r\n");
  assert!(push(&mut lines, b"one\r").is_empty());
  assert_eq!(push(&mut lines, b"\ntwo\r\n\r\nthr"), ["one", "two", ""]);
  assert_eq!(push(&mut lines, b"ee\rx"), [] as [&str; 0]);
  assert_eq!(lines.finish().as_deref(), Some("three\rx"));
}

#[test]
fn an_empty_marker_passes_on_each_read_keeping_a_cut_character() {
  let mut lines = Lines::new("");
  assert_eq!(push(&mut lines, b"caf\xc3"), ["caf"]);
  assert_eq!(push(&mut lines, b"\xa9\n!"), ["\u{e9}\n!"]);
  assert!(push(&mut lines, b"").is_empty());
  assert!(push(&mut lines, b"\xe2\x82").is_empty());
  assert_eq!(lines.finish().as_deref(), Some("\u{fffd}"));
}

#[test]
fn a_line_over_the_cap_is_dropped_up_to_a_marker_a_read_cut() {
  let mut lines = Lines::new("--");
  let mut long = vec![b'y'; MAX_LINE + 10];
  long.push(b'-');
  let logs = capture_logs(|| {
    assert!(push(&mut lines, &long).is_empty());
    assert_eq!(push(&mut lines, b"-next--"), ["next"]);
  });
  assert_eq!(logs.matches("a line over 1 MiB").count(), 1, "{logs}");
  assert!(lines.inbox.capacity() <= 64 * 1024, "its room isn't kept");
}

#[test]
fn a_line_that_ends_past_the_cap_in_one_read_is_dropped() {
  let mut lines = Lines::default();
  let mut bytes = vec![b'y'; MAX_LINE + 1];
  bytes.extend_from_slice(b"\nok\n");
  let logs = capture_logs(|| assert_eq!(push(&mut lines, &bytes), ["ok"]));
  assert!(logs.contains("a line over 1 MiB"), "{logs}");
}

#[test]
fn a_new_marker_splits_what_waits_too() {
  let mut lines = Lines::default();
  assert!(push(&mut lines, b"a;b").is_empty());
  lines.set_marker(";");
  assert_eq!(push(&mut lines, b";c"), ["a", "b"]);
  lines.reset();
  assert_eq!(push(&mut lines, b"d;"), ["d"], "the marker stays");
}
