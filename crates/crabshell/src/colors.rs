//! `cx.quantize_colors`: an image's main colors, worked out on a thread of
//! its own, since decoding a wallpaper takes tens of milliseconds.

use std::cell::Cell;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

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

/// The running job's loop source, if any.
type Job = Rc<Cell<Option<RegistrationToken>>>;

/// Quantizing an image file into up to 2^`depth` colors.
pub(crate) fn quantizer(depth: u8) -> Work {
  Arc::new(move |path| crabshell_render::quantize_colors(path, depth, SIZE))
}

/// Colors that `work` finds for `path`, empty until the first job
/// answers. Each new path starts a job and drops the one before, whose
/// answer can't land after it. When `cx`'s owner goes, so does the job's
/// source.
#[track_caller]
pub(crate) fn follow<D: 'static>(
  cx: &Cx,
  handle: Handle<D>,
  path: Prop<PathBuf>,
  work: Work,
) -> ReadSignal<Vec<Color>> {
  let at = Location::caller();
  let colors = cx.signal(Vec::new());
  let job = Job::default();
  let stop = {
    let (handle, job) = (Rc::clone(&handle), Rc::clone(&job));
    Rc::new(move || {
      if let (Some(token), Some(handle)) = (job.take(), handle()) {
        handle.remove(token);
      }
    })
  };
  let gone = Rc::clone(&stop);
  // The signal above was made, so the owner is there.
  cx.owner().signal(OnDispose(Some(Box::new(move || gone()))));
  let path = path.shared();
  cx.effect(move || {
    // The config's closure, if the path is one.
    let path = path();
    stop();
    job.set(start(&handle, &job, path, Arc::clone(&work), colors));
  })
  .named(format!("cx.quantize_colors (called at {at})"));
  colors.read_only()
}

/// Runs `work` on `path` on a thread of its own, and sets `colors` from
/// its answer through a loop source that goes as the answer arrives.
fn start<D: 'static>(
  handle: &Handle<D>,
  job: &Job,
  path: PathBuf,
  work: Work,
  colors: Signal<Vec<Color>>,
) -> Option<RegistrationToken> {
  let loop_handle = handle()?;
  let (sender, answers) = channel::channel();
  let worked = path.clone();
  let spawned = std::thread::Builder::new()
    .name(String::from("quantize"))
    .spawn(move || {
      let _ = sender.send(work(&worked));
    });
  if let Err(error) = spawned {
    failed(&path, &error.to_string(), colors);
    return None;
  }
  let (handle, job) = (Rc::clone(handle), Rc::clone(job));
  let mut asked = Some(path.clone());
  let inserted = loop_handle.insert_source(answers, move |event, (), _| {
    // The thread's end follows its answer, sometimes in the same call.
    let Some(path) = asked.take() else {
      return;
    };
    if let (Some(token), Some(handle)) = (job.take(), handle()) {
      handle.remove(token);
    }
    match event {
      Event::Msg(Ok(found)) => {
        colors.try_set(found);
      }
      Event::Msg(Err(error)) => failed(&path, &error, colors),
      Event::Closed => failed(&path, "the thread ended without an answer", colors),
    }
  });
  match inserted {
    Ok(token) => Some(token),
    Err(error) => {
      failed(&path, &error.error.to_string(), colors);
      None
    }
  }
}

fn failed(path: &Path, error: &str, colors: Signal<Vec<Color>>) {
  tracing::warn!(path = %path.display(), error, "couldn't find an image's colors");
  colors.try_set(Vec::new());
}
