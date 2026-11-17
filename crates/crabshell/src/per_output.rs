//! `cx.per_output`: building windows for every output as it comes, each
//! output's under an owner of its own, and dropping them when it goes.
//!
//! A builder lives as long as the context it was registered with: a guard
//! kept by that context's owner marks it dead when the owner is disposed,
//! and everything it built is removed at the next step. The bookkeeping is
//! generic over [`Host`], what holds the windows: the platform in the shell,
//! and a stand-in in tests, which can't start a compositor.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::fmt::Debug;
use std::panic::{self, AssertUnwindSafe, Location};
use std::rc::Rc;

use crabshell_reactive::{Owner, untrack};
use crabshell_ui::{Cx, Output, PanelWindow, Window};

/// What a `per_output` closure returns: one window, or several.
pub trait IntoPanels {
  /// The windows to show.
  fn into_panels(self) -> Vec<PanelWindow>;
}

impl IntoPanels for PanelWindow {
  fn into_panels(self) -> Vec<PanelWindow> {
    vec![self]
  }
}

impl IntoPanels for Vec<PanelWindow> {
  fn into_panels(self) -> Vec<PanelWindow> {
    self
  }
}

/// What holds the windows `per_output` builds.
pub(crate) trait Host {
  type Id: Copy + PartialEq + Debug;

  fn add_window(&mut self, window: Window) -> Self::Id;
  fn remove_window(&mut self, id: Self::Id);
}

/// A `per_output` closure, as stored.
pub(crate) type Build = Rc<dyn Fn(&Cx, &Output) -> Vec<PanelWindow>>;

/// One `cx.per_output` call: the context it was called with, whose child
/// each output's owner is, its closure and where it was called.
struct Builder {
  cx: Cx,
  build: Build,
  /// Cleared when `cx`'s owner is disposed.
  alive: Rc<Cell<bool>>,
  at: &'static Location<'static>,
}

/// Kept by a builder's context: dropped when that context's owner is
/// disposed, which marks the builder dead and calls `on_dead`.
struct Guard {
  alive: Rc<Cell<bool>>,
  on_dead: Option<Box<dyn FnOnce()>>,
}

impl Drop for Guard {
  fn drop(&mut self) {
    self.alive.set(false);
    if let Some(on_dead) = self.on_dead.take() {
      on_dead();
    }
  }
}

/// What one builder built for one output.
struct Built<Id> {
  builder: usize,
  output: String,
  /// Everything the closure created belongs to it.
  owner: Owner,
  windows: Vec<Id>,
  /// The closure panicked, so nothing was shown.
  panicked: bool,
}

/// Every `per_output` call and what each built, by output.
pub(crate) struct PerOutput<Id> {
  /// By number; a dead builder's place is kept empty so the numbers in
  /// pending requests stay right.
  builders: Vec<Option<Builder>>,
  /// An output's entry stays after the compositor closed one of its
  /// windows, with nothing in it, so nothing is built for that output
  /// again until it is announced again.
  built: Vec<Built<Id>>,
}

impl<Id> PerOutput<Id> {
  pub(crate) fn new() -> PerOutput<Id> {
    PerOutput {
      builders: Vec::new(),
      built: Vec::new(),
    }
  }

  /// Keeps a `per_output` call made at `at`. Returns its number, for
  /// [`existing`]. `on_dead` runs when `cx`'s owner is disposed, so the
  /// shell can remove the builder's windows soon.
  pub(crate) fn register(
    &mut self,
    cx: Cx,
    build: Build,
    at: &'static Location<'static>,
    on_dead: Box<dyn FnOnce()>,
  ) -> usize {
    let alive = Rc::new(Cell::new(true));
    let guard = Guard {
      alive: Rc::clone(&alive),
      on_dead: Some(on_dead),
    };
    // A context disposed already can't keep it: the guard is dropped
    // at once, and the builder is dead from the start.
    let owner = cx.owner();
    if panic::catch_unwind(AssertUnwindSafe(move || owner.signal(guard))).is_err() {
      tracing::warn!("per_output at {at} was called with a context that is gone; nothing is built");
    }
    self.builders.push(Some(Builder {
      cx,
      build,
      alive,
      at,
    }));
    self.builders.len() - 1
  }

  /// Whether something was built and every build panicked: the shell has
  /// nothing to show. A closure that returns no windows didn't panic.
  pub(crate) fn only_panicked(&self) -> bool {
    !self.built.is_empty() && self.built.iter().all(|built| built.panicked)
  }

  /// Takes out the builders whose context is gone and everything they
  /// built, to be dropped once the state is released.
  fn take_dead(&mut self) -> (Vec<Builder>, Vec<Built<Id>>) {
    let mut dead = Vec::new();
    for slot in &mut self.builders {
      if slot.as_ref().is_some_and(|builder| !builder.alive.get())
        && let Some(builder) = slot.take()
      {
        dead.push(builder);
      }
    }
    let (gone, kept) = std::mem::take(&mut self.built)
      .into_iter()
      .partition(|built| self.builders.get(built.builder).is_none_or(Option::is_none));
    self.built = kept;
    (dead, gone)
  }
}

/// Removes the windows of builders whose context is gone. Every step below
/// starts and ends with it; the shell also runs it soon after a builder's
/// context is disposed.
pub(crate) fn sweep<H: Host>(state: &RefCell<PerOutput<H::Id>>, host: &mut H) {
  let (dead, gone) = state.borrow_mut().take_dead();
  for built in gone {
    for window in built.windows {
      host.remove_window(window);
    }
    built.owner.dispose();
  }
  // Their closures and contexts are dropped here, with the state free.
  drop(dead);
}

/// Builds `builder`'s windows for each of `outputs` that it hasn't built
/// for yet: the outputs there when `per_output` was called.
pub(crate) fn existing<H: Host>(
  state: &RefCell<PerOutput<H::Id>>,
  host: &mut H,
  builder: usize,
  outputs: &[String],
) {
  sweep(state, host);
  for output in outputs {
    build(state, host, builder, output);
  }
  sweep(state, host);
}

/// An output was announced: every builder builds for it.
pub(crate) fn added<H: Host>(state: &RefCell<PerOutput<H::Id>>, host: &mut H, output: &str) {
  sweep(state, host);
  // Builders registered while these run are built for this output by
  // their own `existing` request.
  let builders = state.borrow().builders.len();
  for builder in 0..builders {
    build(state, host, builder, output);
  }
  sweep(state, host);
}

/// An output went away: everything built for it goes, owners and windows.
/// The platform has dropped the windows on it already; any others the
/// closure built are removed here. If another output of the same name is
/// still announced, among `announced`, it is built for again.
pub(crate) fn removed<H: Host>(
  state: &RefCell<PerOutput<H::Id>>,
  host: &mut H,
  output: &str,
  announced: &[String],
) {
  let gone: Vec<Built<H::Id>> = {
    let mut state = state.borrow_mut();
    let (gone, kept) = std::mem::take(&mut state.built)
      .into_iter()
      .partition(|built| built.output == output);
    state.built = kept;
    gone
  };
  for built in gone {
    for window in built.windows {
      host.remove_window(window);
    }
    built.owner.dispose();
  }
  // Builders registered inside those closures went with them.
  sweep(state, host);
  if announced.iter().any(|name| name == output) {
    added(state, host, output);
  }
}

/// The compositor closed `window`: the rest of what was built for its
/// output goes too. Nothing is built for that output again until it is
/// announced again, since a compositor that closed a window once may well
/// close a new one. Sway closes every layer surface on an output before
/// it removes the output, so this is the usual path for an unplug.
pub(crate) fn closed<H: Host>(state: &RefCell<PerOutput<H::Id>>, host: &mut H, window: H::Id) {
  let found = {
    let mut state = state.borrow_mut();
    state
      .built
      .iter_mut()
      .find(|built| built.windows.contains(&window))
      .map(|built| {
        let windows = std::mem::take(&mut built.windows);
        (built.output.clone(), built.owner, windows)
      })
  };
  let Some((output, owner, windows)) = found else {
    return;
  };
  tracing::info!(
    output,
    "the compositor closed a window on {output}, so per_output dropped what it built there until the output is announced again"
  );
  for other in windows.into_iter().filter(|other| *other != window) {
    host.remove_window(other);
  }
  owner.dispose();
  sweep(state, host);
}

/// Runs `builder` for `output` under a new child owner and shows its
/// windows, unless it built for that output already or its context is
/// gone. A panic is logged with its path and drops whatever the closure
/// made; other outputs and builders carry on.
fn build<H: Host>(state: &RefCell<PerOutput<H::Id>>, host: &mut H, builder: usize, output: &str) {
  // Take what's needed and let go of the state: the closure may call
  // `per_output` again.
  let (parent, build, at) = {
    let state = state.borrow();
    if state
      .built
      .iter()
      .any(|built| built.builder == builder && built.output == output)
    {
      return;
    }
    let Some(Some(found)) = state.builders.get(builder) else {
      return;
    };
    if !found.alive.get() {
      return;
    }
    (found.cx.clone(), Rc::clone(&found.build), found.at)
  };
  let path = format!("per_output[{output}] (called at {at})");
  let mut owner = None;
  let made = panic::catch_unwind(AssertUnwindSafe(|| {
    // Inside the catch: a context disposed since the check above panics
    // here, and is logged like any other failure.
    let cx = parent.child();
    // The name shows in the reactive core's log lines about this owner.
    owner = Some(cx.owner().named(path.clone()));
    untrack(|| {
      build(&cx, &Output::new(output))
        .into_iter()
        .map(|panel| Window::new(&cx, panel))
        .collect::<Vec<Window>>()
    })
  }));
  let panicked = made.is_err();
  let windows = match made {
    Ok(windows) => windows
      .into_iter()
      .map(|window| host.add_window(window))
      .collect(),
    Err(payload) => {
      tracing::error!(
        "{path} panicked, so nothing is shown on {output}: {}",
        panic_message(payload.as_ref())
      );
      // Whatever it made before the panic goes now.
      if let Some(owner) = owner {
        owner.dispose();
      }
      Vec::new()
    }
  };
  // Recorded even after a panic, so it isn't retried until the output
  // is announced again.
  let Some(owner) = owner else {
    return;
  };
  state.borrow_mut().built.push(Built {
    builder,
    output: output.to_owned(),
    owner,
    windows,
    panicked,
  });
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
  if let Some(message) = payload.downcast_ref::<&str>() {
    message
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message
  } else {
    "(panic payload is not a string)"
  }
}
