//! `cx.quantize_colors`: jobs on threads of their own, answered through
//! the loop, on a loop of the tests' own with no compositor.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use crabshell_reactive::Runtime;
use crabshell_ui::{Color, Cx, IntoProp};

use super::capture_logs;
use crate::colors::{self, Handle, Work};

const RED: Color = Color::rgb(255, 0, 0);
const GREEN: Color = Color::rgb(0, 255, 0);
const BLUE: Color = Color::rgb(0, 0, 255);

/// A loop and a runtime, turned the way the shell turns them.
struct Turns {
  event_loop: EventLoop<'static, ()>,
  runtime: Runtime,
}

impl Turns {
  fn new() -> Turns {
    Turns {
      event_loop: EventLoop::try_new().expect("a loop"),
      runtime: Runtime::current(),
    }
  }

  /// The loop's handle, held weakly as the shell's is.
  fn handle(&self) -> Handle<()> {
    let weak = self.event_loop.handle().downgrade();
    std::rc::Rc::new(move || weak.upgrade())
  }

  /// One iteration: what is ready, then the flush. Returns how long it
  /// waited.
  fn turn(&mut self, timeout: Duration) -> Duration {
    let started = Instant::now();
    self
      .event_loop
      .dispatch(Some(timeout), &mut ())
      .expect("the loop runs");
    self.runtime.flush();
    started.elapsed()
  }

  fn until(&mut self, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
      assert!(Instant::now() < deadline, "timed out waiting for {what}");
      self.turn(Duration::from_millis(20));
    }
  }
}

/// A PNG of red and blue, three red pixels to one blue, in the temp dir.
fn red_and_blue(name: &str) -> PathBuf {
  let path = std::env::temp_dir().join(format!(
    "crabshell-quantize-{}-{name}.png",
    std::process::id()
  ));
  let file = std::fs::File::create(&path).expect("create the PNG");
  let mut encoder = png::Encoder::new(file, 2, 2);
  encoder.set_color(png::ColorType::Rgb);
  let mut writer = encoder.write_header().expect("the header");
  writer
    .write_image_data(&[255, 0, 0, 0, 0, 255, 255, 0, 0, 255, 0, 0])
    .expect("the pixels");
  path
}

/// Work that answers `slow` with green once `release` sends, `fast` with
/// blue, and anything else with red. `done` hears each path as its job
/// ends.
struct Gated {
  work: Work,
  release: mpsc::Sender<()>,
  done: mpsc::Receiver<PathBuf>,
}

fn gated() -> Gated {
  let (release, gate) = mpsc::channel();
  let gate = Mutex::new(gate);
  let (finished, done) = mpsc::channel();
  let work: Work = Arc::new(move |path: &Path| {
    if path == Path::new("slow") {
      gate.lock().expect("the gate").recv().expect("released");
    }
    let colors = match path.to_str() {
      Some("slow") => vec![GREEN],
      Some("fast") => vec![BLUE],
      _ => vec![RED],
    };
    let _ = finished.send(path.to_owned());
    Ok(colors)
  });
  Gated {
    work,
    release,
    done,
  }
}

/// Waits for the job for `path` to end.
fn ended(done: &mpsc::Receiver<PathBuf>, path: &str) {
  while done
    .recv_timeout(Duration::from_secs(10))
    .expect("a job ends")
    != Path::new(path)
  {}
}

#[test]
fn the_colors_fill_once_the_image_is_quantized() {
  let mut turns = Turns::new();
  let cx = Cx::new(turns.runtime.new_owner());
  let path = red_and_blue("fill");
  let found = colors::follow(&cx, turns.handle(), path.into_prop(), colors::quantizer(1));
  assert!(found.get_untracked().is_empty());
  turns.until("the colors", || !found.get_untracked().is_empty());
  assert_eq!(found.get_untracked(), [RED, BLUE]);
  cx.owner().dispose();
}

#[test]
fn nothing_wakes_once_the_colors_are_in() {
  let mut turns = Turns::new();
  let cx = Cx::new(turns.runtime.new_owner());
  let path = red_and_blue("idle");
  let found = colors::follow(&cx, turns.handle(), path.into_prop(), colors::quantizer(1));
  turns.until("the colors", || !found.get_untracked().is_empty());
  let waited = turns.turn(Duration::from_millis(500));
  assert!(
    waited >= Duration::from_millis(490),
    "something woke the loop after {waited:?}"
  );
  cx.owner().dispose();
}

#[test]
fn a_failure_is_logged_with_the_path_and_empties_the_list() {
  let mut turns = Turns::new();
  let cx = Cx::new(turns.runtime.new_owner());
  let path = cx.signal(red_and_blue("failure"));
  let found = colors::follow(
    &cx,
    turns.handle(),
    (move || path.get()).into_prop(),
    colors::quantizer(1),
  );
  turns.until("the colors", || !found.get_untracked().is_empty());
  let missing = std::env::temp_dir().join("crabshell-quantize-missing.png");
  let logs = capture_logs(|| {
    path.set(missing.clone());
    turns.until("the list to empty", || found.get_untracked().is_empty());
  });
  assert!(
    logs.contains(&format!("path={}", missing.display())),
    "{logs}"
  );
  assert!(logs.contains("couldn't find an image's colors"), "{logs}");
  cx.owner().dispose();
}

#[test]
fn a_new_path_replaces_the_colors_and_an_older_result_never_lands_after_it() {
  let mut turns = Turns::new();
  let cx = Cx::new(turns.runtime.new_owner());
  let path = cx.signal(PathBuf::from("first"));
  let Gated {
    work,
    release,
    done,
  } = gated();
  let found = colors::follow(&cx, turns.handle(), (move || path.get()).into_prop(), work);
  turns.until("the first colors", || found.get_untracked() == [RED]);
  // The slow job is still at work when the fast one starts and ends.
  path.set(PathBuf::from("slow"));
  turns.runtime.flush();
  path.set(PathBuf::from("fast"));
  turns.until("the fast colors", || found.get_untracked() == [BLUE]);
  release.send(()).expect("the slow job waits");
  ended(&done, "slow");
  turns.turn(Duration::from_millis(200));
  assert_eq!(found.get_untracked(), [BLUE]);
  cx.owner().dispose();
}

#[test]
fn when_the_owner_goes_nothing_is_set_and_the_source_is_removed() {
  let mut turns = Turns::new();
  let root = Cx::new(turns.runtime.new_owner());
  let before = turns.runtime.node_count();
  let child = root.child();
  let Gated {
    work,
    release,
    done,
  } = gated();
  let found = colors::follow(
    &child,
    turns.handle(),
    PathBuf::from("slow").into_prop(),
    work,
  );
  turns.runtime.flush();
  child.owner().dispose();
  turns.runtime.flush();
  assert_eq!(
    turns.runtime.node_count(),
    before,
    "everything it made is freed"
  );
  assert_eq!(found.try_get(), None);
  // The job ends and answers, and no source is there to hear it.
  release.send(()).expect("the slow job waits");
  ended(&done, "slow");
  let waited = turns.turn(Duration::from_millis(300));
  assert!(
    waited >= Duration::from_millis(290),
    "the answer woke the loop after {waited:?}"
  );
  root.owner().dispose();
}

#[test]
fn a_path_closure_that_panics_is_logged_with_its_call_site() {
  let mut turns = Turns::new();
  let cx = Cx::new(turns.runtime.new_owner());
  let Gated { work, .. } = gated();
  let logs = capture_logs(|| {
    let found = colors::follow(
      &cx,
      turns.handle(),
      (|| -> PathBuf { panic!("no wallpaper") }).into_prop(),
      work,
    );
    turns.turn(Duration::from_millis(50));
    assert!(found.get_untracked().is_empty());
  });
  assert!(
    logs.contains("effect cx.quantize_colors (called at crates/crabshell/src/tests/colors.rs:"),
    "{logs}"
  );
  assert!(
    logs.contains("panicked and is disabled: no wallpaper"),
    "{logs}"
  );
  // The rest keeps running.
  let count = cx.signal(0);
  let seen = cx.signal(0);
  cx.effect(move || seen.set(count.get()));
  count.set(2);
  turns.turn(Duration::from_millis(1));
  assert_eq!(seen.get_untracked(), 2);
  cx.owner().dispose();
}

#[test]
fn a_job_that_panics_is_logged_and_leaves_the_list_empty() {
  let mut turns = Turns::new();
  let cx = Cx::new(turns.runtime.new_owner());
  let work: Work = Arc::new(|_: &Path| panic!("a decoder bug"));
  let logs = capture_logs(|| {
    let found = colors::follow(&cx, turns.handle(), PathBuf::from("bug").into_prop(), work);
    turns.runtime.flush();
    // The loop wakes as the thread ends with no answer.
    turns.turn(Duration::from_secs(10));
    assert!(found.get_untracked().is_empty());
  });
  assert!(logs.contains("path=bug"), "{logs}");
  assert!(logs.contains("couldn't find an image's colors"), "{logs}");
  cx.owner().dispose();
}
