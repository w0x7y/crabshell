//! `cx.quantize_colors`: an image's main colors, worked out on a thread of
//! its own, since decoding a wallpaper takes tens of milliseconds.

use std::cell::RefCell;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use calloop::channel::{self, Event};
use calloop::{LoopHandle, RegistrationToken};
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::{Color, Cx, Prop};

use crate::shell::OnDispose;

/// The longest side an image is quantized at: a wallpaper is halved down
/// to at most this first, which keeps its colors and saves the time.
const SIZE: u32 = 128;

/// The loop's handle while the loop is there. It is held weakly, so a
/// context kept somewhere doesn't keep the loop.
pub(crate) type Handle<D> = Rc<dyn Fn() -> Option<LoopHandle<'static, D>>>;

/// What a job works out on its thread from the path.
pub(crate) type Work = Arc<dyn Fn(&Path) -> Result<Vec<Color>, String> + Send + Sync>;

/// Where the jobs of one `cx.quantize_colors` stand.
#[derive(Default)]
struct Jobs {
  /// The running job's loop source.
  running: Option<RegistrationToken>,
  /// The file the running job, or else the last answer, is for.
  current: Option<Key>,
  /// The newest file asked for while a job runs, started when it ends.
  queued: Option<Key>,
}

/// A file as it was asked for: its path and when it was last modified,
/// so a file rewritten in place is worked out again.
type Key = (PathBuf, Option<SystemTime>);

type Shared = Rc<RefCell<Jobs>>;

/// Quantizing an image file into up to 2^`depth` colors.
pub(crate) fn quantizer(depth: u8) -> Work {
  Arc::new(move |path| crabshell_render::quantize_colors(path, depth, SIZE))
}

/// Colors that `work` finds for `path`, empty until the first job
/// answers. One job runs at a time: a path asked for meanwhile waits,
/// replacing any that waited before it, and the running job's answer is
/// dropped for it, so an older path's colors never land after a newer
/// one's. A path and modification time already worked out start
/// nothing. When `cx`'s owner goes, so does the job's source.
#[track_caller]
pub(crate) fn follow<D: 'static>(
  cx: &Cx,
  handle: Handle<D>,
  path: Prop<PathBuf>,
  work: Work,
) -> ReadSignal<Vec<Color>> {
  let at = Location::caller();
  let colors = cx.signal(Vec::new());
  let jobs = Shared::default();
  let stop = {
    let (handle, jobs) = (Rc::clone(&handle), Rc::clone(&jobs));
    move || {
      let running = jobs.borrow_mut().running.take();
      if let (Some(token), Some(handle)) = (running, handle()) {
        handle.remove(token);
      }
    }
  };
  // The signal above was made, so the owner is there.
  cx.owner().signal(OnDispose(Some(Box::new(stop))));
  let path = path.shared();
  cx.effect(move || {
    // The config's closure, if the path is one.
    let path = path();
    let modified = std::fs::metadata(&path)
      .and_then(|meta| meta.modified())
      .ok();
    let key = (path, modified);
    let mut state = jobs.borrow_mut();
    if state.running.is_some() {
      state.queued = (state.current.as_ref() != Some(&key)).then_some(key);
      return;
    }
    if state.current.as_ref() == Some(&key) {
      return;
    }
    drop(state);
    start(&handle, &jobs, key, Arc::clone(&work), colors);
  })
  .named(format!("cx.quantize_colors (called at {at})"));
  colors.read_only()
}

/// Runs `work` on `key`'s path on a thread of its own, and sets `colors`
/// from its answer through a loop source that goes as the answer
/// arrives, unless another path waits, which starts instead.
fn start<D: 'static>(
  handle: &Handle<D>,
  jobs: &Shared,
  key: Key,
  work: Work,
  colors: Signal<Vec<Color>>,
) {
  let path = key.0.clone();
  jobs.borrow_mut().current = Some(key);
  let Some(loop_handle) = handle() else {
    return;
  };
  let (sender, answers) = channel::channel();
  let (worked, job_work) = (path.clone(), Arc::clone(&work));
  let spawned = std::thread::Builder::new()
    .name(String::from("quantize"))
    .spawn(move || {
      let _ = sender.send(job_work(&worked));
    });
  if let Err(error) = spawned {
    return failed(&path, &error.to_string(), colors);
  }
  let (handle, held) = (Rc::clone(handle), Rc::clone(jobs));
  let mut asked = Some(path.clone());
  let inserted = loop_handle.insert_source(answers, move |event, (), _| {
    // The thread's end follows its answer, sometimes in the same call.
    let Some(path) = asked.take() else {
      return;
    };
    let (running, queued) = {
      let mut state = held.borrow_mut();
      (state.running.take(), state.queued.take())
    };
    if let (Some(token), Some(handle)) = (running, handle()) {
      handle.remove(token);
    }
    if let Some(next) = queued {
      return start(&handle, &held, next, Arc::clone(&work), colors);
    }
    match event {
      Event::Msg(Ok(found)) => {
        if colors.try_with(|old| *old != found) == Some(true) {
          colors.try_set(found);
        }
      }
      Event::Msg(Err(error)) => failed(&path, &error, colors),
      Event::Closed => failed(&path, "the thread ended without an answer", colors),
    }
  });
  match inserted {
    Ok(token) => jobs.borrow_mut().running = Some(token),
    Err(error) => failed(&path, &error.error.to_string(), colors),
  }
}

fn failed(path: &Path, error: &str, colors: Signal<Vec<Color>>) {
  tracing::warn!(path = %path.display(), error, "couldn't find an image's colors");
  colors.try_set(Vec::new());
}
