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
  assert_eq!(lines.finish(&"test"), ["three\rx"]);
}

#[test]
fn an_empty_marker_passes_on_each_read_keeping_a_cut_character() {
  let mut lines = Lines::new("");
  assert_eq!(push(&mut lines, b"caf\xc3"), ["caf"]);
  assert_eq!(push(&mut lines, b"\xa9\n!"), ["\u{e9}\n!"]);
  assert!(push(&mut lines, b"").is_empty());
  assert!(push(&mut lines, b"\xe2\x82").is_empty());
  assert_eq!(lines.finish(&"test"), ["\u{fffd}"]);
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
fn a_line_of_exactly_the_cap_is_kept_when_a_read_cuts_its_marker() {
  let mut lines = Lines::new("\r\n");
  let mut bytes = vec![b'y'; MAX_LINE];
  bytes.push(b'\r');
  let logs = capture_logs(|| {
    assert!(push(&mut lines, &bytes).is_empty());
    let got = push(&mut lines, b"\nok\r\n");
    let lengths: Vec<usize> = got.iter().map(String::len).collect();
    assert_eq!(lengths, [MAX_LINE, 2]);
  });
  assert!(!logs.contains("a line over 1 MiB"), "{logs}");
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

#[test]
fn a_new_marker_splits_what_waits_when_the_stream_ends_right_after() {
  let mut lines = Lines::default();
  assert!(push(&mut lines, b"a;b").is_empty());
  lines.set_marker(";");
  assert_eq!(lines.finish(&"test"), ["a", "b"]);
}

#[test]
fn lines_come_one_at_a_time_so_a_new_marker_splits_the_rest() {
  let mut lines = Lines::default();
  lines.append(b"head\nr1;r2\nr3;");
  assert_eq!(lines.next(&"test").as_deref(), Some("head"));
  lines.set_marker(";");
  assert_eq!(lines.next(&"test").as_deref(), Some("r1"));
  assert_eq!(lines.next(&"test").as_deref(), Some("r2\nr3"));
  assert_eq!(lines.next(&"test"), None);
  assert_eq!(lines.rest(&"test"), None);
}

/// The whole stream split at each marker in turn, leftmost first, with
/// what is after the last one as a last line.
fn split_whole(stream: &[u8], marker: &[u8]) -> Vec<String> {
  let mut out = Vec::new();
  let (mut start, mut at) = (0, 0);
  while at + marker.len() <= stream.len() {
    if &stream[at..at + marker.len()] == marker {
      out.push(String::from_utf8_lossy(&stream[start..at]).into_owned());
      at += marker.len();
      start = at;
    } else {
      at += 1;
    }
  }
  if start < stream.len() {
    out.push(String::from_utf8_lossy(&stream[start..]).into_owned());
  }
  out
}

#[test]
fn reads_of_any_size_split_as_the_whole_stream_would() {
  // xorshift, so every run checks the same streams.
  let mut seed: u64 = 0x1234_5678_9abc_def0;
  let mut next = move || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    seed
  };
  let markers = ["\r\n\r\n", "aa", "aba", "\n", "ab", "abab"];
  for round in 0..20_000 {
    let marker = markers[round % markers.len()];
    let len = (next() % 40) as usize;
    let stream: Vec<u8> = (0..len).map(|_| b"ab\r\n"[(next() % 4) as usize]).collect();
    let mut lines = Lines::new(marker);
    let mut got = Vec::new();
    let mut at = 0;
    while at < stream.len() {
      let end = (at + 1 + (next() % 6) as usize).min(stream.len());
      got.extend(push(&mut lines, &stream[at..end]));
      at = end;
    }
    got.extend(lines.finish(&"test"));
    assert_eq!(
      got,
      split_whole(&stream, marker.as_bytes()),
      "{marker:?} {stream:?}"
    );
  }
}

#[test]
fn a_long_lines_room_goes_once_it_is_passed_on_not_at_the_next_read() {
  let mut lines = Lines::new("\n");
  for chunk in vec![b'z'; 900 * 1024].chunks(64 * 1024) {
    assert!(push(&mut lines, chunk).is_empty());
  }
  assert_eq!(push(&mut lines, b"\n").len(), 1);
  // Nothing more comes, as when the stream goes quiet.
  assert!(
    lines.inbox.capacity() <= 64 * 1024,
    "{}",
    lines.inbox.capacity()
  );
}

#[test]
fn a_last_line_over_the_cap_is_dropped_too() {
  let mut lines = Lines::new("\r\n\r\n");
  let logs = capture_logs(|| {
    assert!(push(&mut lines, &vec![b'y'; MAX_LINE + 3]).is_empty());
    assert!(lines.finish(&"test").is_empty());
  });
  assert_eq!(logs.matches("a line over 1 MiB").count(), 1, "{logs}");
}
