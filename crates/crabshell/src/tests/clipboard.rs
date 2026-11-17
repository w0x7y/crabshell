//! The clipboard history: its order, caps and image files, without a
//! compositor.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use crabshell_wayland::Selection;

use super::capture_logs;
use crate::clipboard::{History, MAX_BYTES, MAX_ENTRIES};
use crate::{Clipboard, ClipboardContent, ClipboardEntry, ShellExt};

fn text(text: &str) -> Selection {
  Selection {
    text: Some(text.to_owned()),
    ..Selection::default()
  }
}

fn image(bytes: &[u8]) -> Selection {
  Selection {
    image: Some((String::from("image/png"), bytes.to_vec())),
    ..Selection::default()
  }
}

/// A recording history with the entries it keeps, as the clipboard holds
/// them.
struct Kept {
  history: History,
  entries: Vec<ClipboardEntry>,
}

impl Kept {
  fn new(dir: Option<PathBuf>) -> Kept {
    let mut history = History::new(dir);
    history.recording = true;
    Kept {
      history,
      entries: Vec::new(),
    }
  }

  fn record(&mut self, selection: &Selection) {
    self.history.record(&mut self.entries, selection);
  }

  fn texts(&self) -> Vec<String> {
    let text = |entry: &ClipboardEntry| match &entry.content {
      ClipboardContent::Text(text) => text.clone(),
      ClipboardContent::Image(_) => String::from("<image>"),
    };
    self.entries.iter().map(text).collect()
  }

  fn image(&self, index: usize) -> PathBuf {
    match &self.entries[index].content {
      ClipboardContent::Image(path) => path.clone(),
      ClipboardContent::Text(_) => panic!("an image"),
    }
  }
}

/// A new, empty directory for this test's images.
fn scratch(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("crabshell-clip-{}-{name}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  dir
}

/// A clipboard with no shell, as a test drives it.
fn clipboard(cx: &Cx) -> Clipboard {
  let mut clipboard = None;
  capture_logs(|| clipboard = Some(cx.clipboard()));
  clipboard.expect("a handle")
}

#[test]
fn nothing_is_kept_until_the_history_records() {
  let mut entries = Vec::new();
  History::new(None).record(&mut entries, &text("early"));
  assert!(entries.is_empty());
}

#[test]
fn the_history_is_newest_first_and_a_text_again_moves_to_the_top() {
  let mut kept = Kept::new(None);
  for copied in ["one", "two", "three"] {
    kept.record(&text(copied));
  }
  assert_eq!(kept.texts(), ["three", "two", "one"]);
  let id = kept.entries[2].id;
  kept.record(&text("one"));
  assert_eq!(kept.texts(), ["one", "three", "two"]);
  assert_eq!(kept.entries[0].id, id, "the entry keeps its id");
  kept.record(&Selection::default());
  kept.record(&text(""));
  assert_eq!(kept.texts().len(), 3, "nothing to keep");
}

#[test]
fn a_secret_is_never_kept() {
  let mut kept = Kept::new(None);
  let secret = Selection {
    secret: true,
    ..text("hunter2")
  };
  kept.record(&secret);
  assert!(kept.entries.is_empty());
}

#[test]
fn the_oldest_entries_go_past_the_caps() {
  let mut kept = Kept::new(None);
  for n in 0..=MAX_ENTRIES {
    kept.record(&text(&n.to_string()));
  }
  let texts = kept.texts();
  assert_eq!(texts.len(), MAX_ENTRIES);
  assert_eq!(texts.last().map(String::as_str), Some("1"), "0 went");

  let mut kept = Kept::new(None);
  let big = MAX_BYTES / 3;
  for letter in ["a", "b", "c", "d"] {
    kept.record(&text(&letter.repeat(big)));
  }
  let firsts: Vec<_> = kept
    .texts()
    .iter()
    .map(|text| text[..1].to_owned())
    .collect();
  assert_eq!(firsts, ["d", "c", "b"], "a went for the bytes");
}

#[test]
fn an_image_is_kept_in_a_private_file_removed_with_its_entry() {
  let dir = scratch("image");
  let mut kept = Kept::new(Some(dir.clone()));
  kept.record(&image(b"first"));
  kept.record(&text("between"));
  kept.record(&image(b"first"));
  assert_eq!(kept.texts(), ["<image>", "between"], "the same image moved");
  let (first, id) = (kept.image(0), kept.entries[0].id);
  assert_eq!(std::fs::read(&first).expect("its file"), b"first");
  let mode = |path: &PathBuf| std::fs::metadata(path).expect("there").permissions().mode() & 0o777;
  assert_eq!(mode(&first), 0o600);
  assert_eq!(mode(&dir), 0o700);

  kept.record(&image(b"second"));
  let second = kept.image(0);
  kept.history.remove(&mut kept.entries, id);
  assert!(!first.exists(), "removed with its entry");
  assert_eq!(kept.texts(), ["<image>", "between"]);
  assert!(second.exists());
  drop(kept);
  assert!(!second.exists(), "removed with the history");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clearing_removes_every_image() {
  let dir = scratch("clear");
  let mut kept = Kept::new(Some(dir.clone()));
  kept.record(&image(b"one"));
  kept.record(&image(b"two"));
  kept.history.clear(&mut kept.entries);
  assert!(kept.entries.is_empty());
  assert_eq!(std::fs::read_dir(&dir).expect("the dir").count(), 0);
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_directory_images_are_skipped() {
  let mut kept = Kept::new(None);
  capture_logs(|| kept.record(&image(b"lost")));
  assert!(kept.entries.is_empty());
}

#[test]
fn the_first_history_call_removes_images_a_crashed_shell_left() {
  let dir = scratch("sweep");
  std::fs::create_dir_all(&dir).expect("the dir");
  // Past the kernel's largest pid, so never alive.
  let dead = dir.join("99999999-0.png");
  let ours = dir.join(format!("{}-1.png", std::process::id()));
  let init = dir.join("1-2.png");
  let other = dir.join("notes.txt");
  for path in [&dead, &ours, &init, &other] {
    std::fs::write(path, b"x").expect("a file");
  }
  let history = History::new(Some(dir.clone()));
  history.sweep();
  assert!(!dead.exists(), "a dead shell's image goes");
  assert!(ours.exists() && init.exists() && other.exists());
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_secret_gives_no_text() {
  let cx = Cx::new(Runtime::current().new_owner());
  let clipboard = clipboard(&cx);
  clipboard.heard(&text("plain"));
  assert_eq!(clipboard.text().get(), Some(String::from("plain")));
  let secret = Selection {
    secret: true,
    ..text("hunter2")
  };
  clipboard.heard(&secret);
  assert_eq!(clipboard.text().get(), None);
  cx.owner().dispose();
}

#[test]
fn a_handle_without_a_shell_stays_empty() {
  let cx = Cx::new(Runtime::current().new_owner());
  let clipboard = clipboard(&cx);
  assert_eq!(clipboard.text().get(), None);
  assert!(clipboard.history().get().is_empty());
  clipboard.set_text("nowhere");
  clipboard.copy(0);
  cx.owner().dispose();
  clipboard.clear_history();
  assert_eq!(clipboard.history().try_get(), None, "gone, not a panic");
}

#[test]
fn a_binding_that_asks_for_the_history_doesnt_rerun_at_every_copy() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let clipboard = clipboard(&cx);
  let runs = cx.signal(0);
  cx.effect(move || {
    clipboard.history();
    runs.update(|runs| *runs += 1);
  });
  runtime.flush();
  clipboard.heard(&text("copied"));
  runtime.flush();
  assert_eq!(runs.get_untracked(), 1);
  assert_eq!(clipboard.history().get_untracked().len(), 1, "it records");
  cx.owner().dispose();
}

/// Set when the test binary runs again to measure a full history alone.
const MEASURE: &str = "CRABSHELL_CLIPBOARD_RSS";

/// This process's resident memory, in KiB.
fn rss_kib() -> u64 {
  let status = std::fs::read_to_string("/proc/self/status").expect("its status");
  let line = status.lines().find(|line| line.starts_with("VmRSS:"));
  let kib = line.and_then(|line| line.split_whitespace().nth(1));
  kib.and_then(|kib| kib.parse().ok()).expect("its RSS")
}

/// Run alone in a child by the test below: fills the history with 1 MiB
/// texts past its 32 MiB and prints how much it grew.
#[test]
#[ignore = "run in a child by a_full_history_holds_each_text_once"]
fn fill_a_history_and_print_its_growth() {
  if std::env::var_os(MEASURE).is_none() {
    return;
  }
  let cx = Cx::new(Runtime::current().new_owner());
  let clipboard = clipboard(&cx);
  clipboard.history();
  let before = rss_kib();
  for n in 0..40_u8 {
    clipboard.heard(&text(&char::from(b'0' + n).to_string().repeat(1024 * 1024)));
  }
  assert_eq!(clipboard.history().get_untracked().len(), 32);
  println!("grew {} KiB", rss_kib() - before);
}

#[test]
fn a_full_history_holds_each_text_once() {
  let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
    .args([
      "--exact",
      "tests::clipboard::fill_a_history_and_print_its_growth",
      "--ignored",
      "--nocapture",
      "--test-threads=1",
    ])
    .env(MEASURE, "1")
    .output()
    .expect("the child runs");
  let out = String::from_utf8_lossy(&output.stdout);
  let grew: u64 = out
    .split("grew ")
    .nth(1)
    .and_then(|rest| rest.split(' ').next())
    .and_then(|kib| kib.parse().ok())
    .unwrap_or_else(|| panic!("no measurement in {out}"));
  // 32 MiB of entries, the last text in `text()` and a little more; a
  // second copy of every text would make it about 64 MiB.
  assert!(grew < 44 * 1024, "grew {grew} KiB");
  assert!(grew > 30 * 1024, "grew only {grew} KiB");
}
