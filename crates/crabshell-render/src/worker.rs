//! Decoding image files off the loop's thread. One worker thread takes the
//! jobs in turn; it starts with the first job and ends once none is left,
//! so a shell decoding nothing has no thread waiting and nothing to wake.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crabshell_ui::{Fit, ImageFilters};

use crate::animation::Frames;

/// What to decode: the file at `path` as [`crate::animation::frames`]
/// draws it.
pub(crate) struct Job {
  pub(crate) path: PathBuf,
  pub(crate) size: (u32, u32),
  pub(crate) fit: Fit,
  pub(crate) filters: ImageFilters,
  pub(crate) scale: f32,
  /// The bytes of frames a GIF may keep.
  pub(crate) room: usize,
}

/// A job done: its number and what decoding gave.
pub(crate) type Done = (u64, Result<Frames, String>);

/// Called once from the worker when a job is done.
type Wake = Box<dyn FnOnce() + Send>;

/// The jobs waiting and done, shared with the worker.
#[derive(Default)]
struct Queue {
  waiting: VecDeque<(u64, Job)>,
  done: Vec<Done>,
  /// Whether a worker thread is running; it takes every job waiting.
  running: bool,
  wake: Option<Wake>,
}

/// Hands jobs to the worker thread and takes what it decoded.
#[derive(Default)]
pub(crate) struct Worker {
  // `Arc` and `Mutex`: the worker thread and the loop's both hold it.
  queue: Arc<Mutex<Queue>>,
  next: u64,
}

impl Worker {
  /// Queues `job`, starting the worker if none runs, and returns its
  /// number.
  pub(crate) fn send(&mut self, job: Job) -> u64 {
    self.next += 1;
    let number = self.next;
    let start = {
      let mut queue = lock(&self.queue);
      queue.waiting.push_back((number, job));
      !std::mem::replace(&mut queue.running, true)
    };
    if start {
      let queue = Arc::clone(&self.queue);
      let spawned = std::thread::Builder::new()
        .name(String::from("decode"))
        .spawn(move || work(&queue));
      if let Err(error) = spawned {
        // Decoded here instead: slow, but the picture still shows.
        tracing::warn!(%error, "couldn't start the image decoding thread");
        work(&self.queue);
      }
    }
    number
  }

  /// Takes job `number` back if the worker hasn't started it, and returns
  /// whether it did.
  pub(crate) fn cancel(&mut self, number: u64) -> bool {
    let mut queue = lock(&self.queue);
    let before = queue.waiting.len();
    queue.waiting.retain(|(waiting, _)| *waiting != number);
    queue.waiting.len() < before
  }

  /// Whether the worker thread runs, for tests.
  #[cfg(test)]
  pub(crate) fn running(&self) -> bool {
    lock(&self.queue).running
  }

  /// The jobs done since the last call.
  pub(crate) fn take(&mut self) -> Vec<Done> {
    std::mem::take(&mut lock(&self.queue).done)
  }

  /// Calls `wake` once a job is done, from the worker, or at once if one
  /// is done already. It replaces a `wake` given before.
  pub(crate) fn wake_when_done(&mut self, wake: Wake) {
    let now = {
      let mut queue = lock(&self.queue);
      if queue.done.is_empty() {
        queue.wake = Some(wake);
        None
      } else {
        Some(wake)
      }
    };
    if let Some(wake) = now {
      wake();
    }
  }
}

/// Decodes every job waiting, in order, and returns once none is left.
fn work(queue: &Mutex<Queue>) {
  loop {
    let Some((number, job)) = ({
      let mut queue = lock(queue);
      let next = queue.waiting.pop_front();
      if next.is_none() {
        queue.running = false;
      }
      next
    }) else {
      return;
    };
    let Job {
      path,
      size,
      fit,
      filters,
      scale,
      room,
    } = job;
    // A decoder's bug ends this job, not the thread or the shell.
    let frames = std::panic::catch_unwind(AssertUnwindSafe(|| {
      crate::animation::frames(&path, size, fit, &filters, scale, room)
    }))
    .unwrap_or_else(|_| Err(String::from("the decoder panicked")));
    let wake = {
      let mut queue = lock(queue);
      queue.done.push((number, frames));
      queue.wake.take()
    };
    if let Some(wake) = wake {
      wake();
    }
  }
}

/// The queue, locked. Nothing panics while holding it, but a poisoned lock
/// still holds good data.
fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
  queue
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}
