//! The runtime: this thread's arena of nodes, the dependency edges between
//! them, and `flush`.
//!
//! Every handle (`Signal`, `Memo`, `Effect`, `Owner`) is a [`NodeId`] into one
//! arena that lives in a thread-local `RefCell`. Functions here borrow it for a
//! few lines at a time and never while user code runs. A memo's or effect's
//! closure is taken out of its node before it is called and put back after.
//! User values and closures are dropped only after the borrow has ended, and
//! through [`drop_isolated`], so a panicking `Drop` can't escape either, and
//! nothing a `Drop` reads becomes a dependency. So user code can call back
//! into the runtime freely, and a panic in user code can't leave the runtime
//! borrowed or half-updated.
//!
//! When the thread exits, the arena is dropped with everything in it. User
//! `Drop` code that calls back in then finds the runtime gone and is treated
//! as touching a disposed handle: writes and disposes do nothing, and reads
//! find no value.

use std::any::{Any, type_name};
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::mem;
use std::ops::Deref;
use std::panic::{self, AssertUnwindSafe, Location};
use std::rc::Rc;

use slotmap::{SlotMap, new_key_type};

use crate::Owner;

/// How many times one effect may run in a single flush. An effect that is
/// queued again after that is in a dependency cycle, so it is logged and
/// disabled.
pub const MAX_RUNS_PER_FLUSH: u32 = 100;

new_key_type! {
    /// The key of one node in the arena. A key also records the slot's
    /// version, so once its node is removed it never matches a newer node
    /// that reuses the slot.
    pub(crate) struct NodeId;
}

/// What one run of a memo's or effect's closure produced.
pub(crate) struct Computed {
  /// Whether the value changed. Always true for an effect.
  pub(crate) changed: bool,
  /// A value the run replaced or didn't keep. The runtime drops it after
  /// telling the readers, so a panic in its `Drop` can't stop that.
  pub(crate) leftover: Option<Box<dyn Any>>,
}

/// A memo's or effect's closure with its types erased.
pub(crate) type Compute = Box<dyn FnMut() -> Computed>;

/// Why a read or write found no value. These can happen in correct code, so
/// `try_get()` and `try_set()` report them instead of panicking.
pub(crate) enum Missing {
  /// The node's owner was disposed.
  Disposed,
  /// A memo whose closure panicked before producing its first value.
  NoValue,
  /// The thread is exiting and its runtime has been dropped.
  Gone,
}

impl Missing {
  #[track_caller]
  pub(crate) fn panic<T>(self, handle: &str) -> ! {
    let ty = type_name::<T>();
    match self {
      Missing::Disposed => panic!(
        "{handle}<{ty}> was used after its owner was disposed (try_get() returns None instead)"
      ),
      Missing::NoValue => panic!(
        "{handle}<{ty}> has no value: its closure panicked before producing one and was disabled"
      ),
      Missing::Gone => panic!(
        "{handle}<{ty}> was used after this thread's runtime was torn down (try_get() returns None instead)"
      ),
    }
  }
}

/// The panic payload of a borrow conflict inside a memo: the memo read a
/// signal that is being updated further up the stack, or its own value is
/// still borrowed by a `with()` further up. The memo is not at fault, so
/// [`run`] puts it back unchanged and passes the panic on to its reader.
struct BorrowConflict(String);

struct Node {
  kind: Kind,
  /// The owner or effect this node was created under. `None` for a root
  /// owner.
  owner: Option<NodeId>,
  /// Creation order. Effects in one flush round run in this order.
  seq: u64,
  /// Debug name, such as a widget path, used in log lines.
  name: Option<String>,
  state: State,
  /// Set once the closure panicked or was caught in a cycle. A disabled
  /// node never runs again.
  disabled: bool,
  /// The memo's or effect's closure. `None` for signals and owners, while
  /// the closure is running, and once the node is disabled.
  compute: Option<Compute>,
  /// What was created under this node: everything an owner made, or the
  /// scope an effect made with [`new_scope`] on its last run.
  children: Vec<NodeId>,
  /// The signals and memos this memo or effect read on its last run.
  sources: Vec<NodeId>,
  /// The memos and effects that read this signal or memo on their last run.
  subscribers: Vec<NodeId>,
}

enum Kind {
  Owner,
  /// `value` is an `Rc<RefCell<Option<T>>>` that always holds `Some`.
  Signal {
    value: Rc<dyn Any>,
  },
  /// `value` is an `Rc<RefCell<Option<T>>>`, `None` until the first
  /// computation succeeds.
  Memo {
    value: Rc<dyn Any>,
  },
  /// `queued` is true while the effect waits in the flush queue, so it is
  /// never queued twice.
  Effect {
    queued: bool,
  },
}

impl Kind {
  fn name(&self) -> &'static str {
    match self {
      Kind::Owner => "owner",
      Kind::Signal { .. } => "signal",
      Kind::Memo { .. } => "memo",
      Kind::Effect { .. } => "effect",
    }
  }
}

/// How up to date a memo or effect is. Signals and owners stay `Clean`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum State {
  Clean,
  /// A memo it read may have changed. Refreshing those memos tells.
  Check,
  /// Something it read has changed, so it must run again.
  Dirty,
}

/// The memo or effect whose closure is running.
#[derive(Clone, Copy)]
struct Running {
  id: NodeId,
  is_memo: bool,
}

#[derive(Default)]
struct Graph {
  nodes: SlotMap<NodeId, Node>,
  /// Effects waiting for the next flush round.
  queue: Vec<NodeId>,
  /// The memo or effect that what is read now becomes a dependency of: the
  /// one whose closure is running. `None` outside any run and inside
  /// [`untrack`].
  observer: Option<NodeId>,
  /// The innermost memo or effect whose closure is running. Unlike
  /// `observer`, [`untrack`] leaves it alone.
  running: Option<Running>,
  next_seq: u64,
  flushing: bool,
}

impl Graph {
  /// True while the innermost running closure is a memo's. Writing a
  /// signal or flushing then panics.
  fn memo_running(&self) -> bool {
    self.running.is_some_and(|running| running.is_memo)
  }
}

impl Drop for Graph {
  fn drop(&mut self) {
    // This runs when the thread exits. The runtime already counts as
    // gone, so user `Drop` code that calls back in finds nothing to do.
    for (_, node) in self.nodes.drain() {
      drop_isolated(node);
    }
  }
}

thread_local! {
    static GRAPH: RefCell<Graph> = RefCell::new(Graph::default());
}

/// Runs `f` on this thread's runtime. Returns `None` if the runtime is gone
/// because the thread is exiting. Every call from outside the runtime starts
/// here.
fn try_graph<R>(f: impl FnOnce(&mut Graph) -> R) -> Option<R> {
  GRAPH
    .try_with(|graph| {
      let mut graph = graph
        .try_borrow_mut()
        .expect("invariant: the runtime is never borrowed while user code runs");
      f(&mut graph)
    })
    .ok()
}

/// Like [`try_graph`], for the steps after a call has found the runtime alive.
fn with_graph<R>(f: impl FnOnce(&mut Graph) -> R) -> R {
  try_graph(f).expect("invariant: the runtime outlives every call that found it alive")
}

/// Drops a user value or closure that the runtime owned, with no borrow
/// held. A panic in its `Drop` is caught and logged, so it can't escape
/// halfway through a flush or a dispose and leave the runtime inconsistent.
/// The drop is untracked, so what the `Drop` reads doesn't become a
/// dependency of whichever memo or effect happens to be running.
pub(crate) fn drop_isolated<T>(value: T) {
  let caught = untrack(|| panic::catch_unwind(AssertUnwindSafe(move || drop(value))));
  if let Err(payload) = caught {
    tracing::error!(
      "a Drop impl panicked while the runtime was dropping a value or closure: {}",
      panic_message(payload.as_ref())
    );
  }
}

/// Holds a value and drops it through [`drop_isolated`] when it goes, even
/// while a panic unwinds.
struct DropIsolated<T>(Option<T>);

impl<T> Deref for DropIsolated<T> {
  type Target = T;

  fn deref(&self) -> &T {
    self
      .0
      .as_ref()
      .expect("invariant: a DropIsolated holds its value until it is dropped")
  }
}

impl<T> Drop for DropIsolated<T> {
  fn drop(&mut self) {
    drop_isolated(self.0.take());
  }
}

/// True if nothing holds a borrow of this thread's runtime.
#[cfg(test)]
pub(crate) fn is_unborrowed() -> bool {
  GRAPH.with(|graph| graph.try_borrow_mut().is_ok())
}

/// How many memos and effects read this node on their last run.
#[cfg(test)]
pub(crate) fn subscriber_count(id: NodeId) -> usize {
  with_graph(|g| g.nodes.get(id).map_or(0, |node| node.subscribers.len()))
}

/// A key's slot index and version.
#[cfg(test)]
pub(crate) fn slot_of(id: NodeId) -> (u32, u32) {
  use slotmap::Key;
  let ffi = id.data().as_ffi();
  (ffi as u32, (ffi >> 32) as u32)
}

/// This thread's reactive runtime.
///
/// There is one per thread, created on first use. In the shell that is the
/// main thread. A `Runtime` value is only a handle to it, so
/// [`Runtime::current`] is cheap and every copy refers to the same runtime.
#[derive(Clone, Copy)]
pub struct Runtime {
  /// Keeps the handle on its thread: `*const ()` is neither `Send` nor `Sync`.
  _not_send: PhantomData<*const ()>,
}

impl Runtime {
  /// Returns a handle to this thread's runtime.
  pub fn current() -> Runtime {
    Runtime {
      _not_send: PhantomData,
    }
  }

  /// Creates a root owner: one with no parent, freed only by its own
  /// [`Owner::dispose`].
  #[track_caller]
  pub fn new_owner(&self) -> Owner {
    Owner::new(None)
  }

  /// Runs every queued effect. The event loop calls this once per
  /// iteration, before layout and painting.
  ///
  /// Each round runs the queued effects once each, in creation order, so an
  /// effect reached through several paths (a diamond) still runs once.
  /// Effects queued during a round, because an effect wrote a signal they
  /// read or was just created, run in the next round of the same flush. An
  /// effect that has run [`MAX_RUNS_PER_FLUSH`] times in this flush and is
  /// queued again is in a cycle: it is logged with its debug name and
  /// disabled, and the flush carries on with the other effects.
  ///
  /// Calling `flush` from inside an effect does nothing: the flush that is
  /// already running picks up whatever got queued.
  ///
  /// # Panics
  ///
  /// Inside a memo, which must not run effects in the middle of its
  /// computation.
  #[track_caller]
  pub fn flush(&self) {
    flush();
  }

  /// The number of live nodes (owners, signals, memos and effects) on this
  /// thread. Tests use it to check that disposing an owner frees
  /// everything created under it.
  pub fn node_count(&self) -> usize {
    try_graph(|g| g.nodes.len()).unwrap_or(0)
  }
}

/// Runs `f` without recording what it reads as dependencies of the running
/// memo or effect, and returns `f`'s result. Recording resumes afterward,
/// even if `f` panics.
///
/// Use it to read a value once without rerunning when it changes, such as
/// the initial value a child widget is built with inside a parent's effect.
pub fn untrack<R>(f: impl FnOnce() -> R) -> R {
  /// Puts the observer back when dropped, which also happens while a panic
  /// unwinds out of `f`.
  struct Restore(Option<NodeId>);

  impl Drop for Restore {
    fn drop(&mut self) {
      let observer = self.0;
      try_graph(|g| g.observer = observer);
    }
  }

  let _restore = try_graph(|g| g.observer.take()).map(Restore);
  f()
}

#[track_caller]
pub(crate) fn create_owner(parent: Option<NodeId>) -> NodeId {
  create(parent, Kind::Owner, None)
}

#[track_caller]
pub(crate) fn create_signal(owner: NodeId, value: Rc<dyn Any>) -> NodeId {
  create(Some(owner), Kind::Signal { value }, None)
}

#[track_caller]
pub(crate) fn create_memo(owner: NodeId, value: Rc<dyn Any>, compute: Compute) -> NodeId {
  create(Some(owner), Kind::Memo { value }, Some(compute))
}

/// The new effect is queued straight away. It runs in the next flush round:
/// later in the current flush if one is running, otherwise at the next flush.
#[track_caller]
pub(crate) fn create_effect(owner: NodeId, compute: Compute) -> NodeId {
  create(Some(owner), Kind::Effect { queued: true }, Some(compute))
}

/// Starts a fresh scope for the running effect, for
/// [`Owner::effect_scoped`]: disposes the scope of its previous run and
/// creates a new owner under the effect itself. So disposing or disabling
/// the effect frees its last scope too.
#[track_caller]
pub(crate) fn new_scope() -> NodeId {
  let (effect, previous) = with_graph(|g| {
    let effect = g
      .running
      .expect("invariant: new_scope() runs inside an effect")
      .id;
    let previous = g
      .nodes
      .get_mut(effect)
      .map(|node| mem::take(&mut node.children))
      .unwrap_or_default();
    (effect, previous)
  });
  for scope in previous {
    dispose(scope);
  }
  create_owner(Some(effect))
}

#[track_caller]
fn create(owner: Option<NodeId>, kind: Kind, compute: Option<Compute>) -> NodeId {
  let owner_alive = try_graph(|g| owner.is_none_or(|owner| g.nodes.contains_key(owner)));
  if owner_alive != Some(true) {
    let what = kind.name();
    // `kind` and `compute` can hold user values. Drop them now, so a
    // panicking `Drop` can't turn the panic below into an abort.
    drop_isolated((kind, compute));
    if owner_alive.is_none() {
      panic!("tried to create a new {what} after this thread's runtime was torn down");
    }
    panic!("tried to create a new {what} under an owner that was already disposed");
  }
  with_graph(|g| {
    let seq = g.next_seq;
    g.next_seq += 1;
    let is_effect = matches!(kind, Kind::Effect { .. });
    // Memos and effects start dirty: they haven't run yet.
    let state = if compute.is_some() {
      State::Dirty
    } else {
      State::Clean
    };
    let id = g.nodes.insert(Node {
      kind,
      owner,
      seq,
      name: None,
      state,
      disabled: false,
      compute,
      children: Vec::new(),
      sources: Vec::new(),
      subscribers: Vec::new(),
    });
    if let Some(owner) = owner
      && let Some(parent) = g.nodes.get_mut(owner)
    {
      parent.children.push(id);
    }
    if is_effect {
      g.queue.push(id);
    }
    id
  })
}

pub(crate) fn set_name(id: NodeId, name: String) {
  try_graph(|g| {
    if let Some(node) = g.nodes.get_mut(id) {
      node.name = Some(name);
    }
  });
}

/// Reads a signal's or memo's value. A stale memo is recomputed first. If
/// `track` is true and a memo or effect is running, it starts depending on
/// this node.
#[track_caller]
pub(crate) fn read<T: 'static, R>(
  id: NodeId,
  track: bool,
  handle: &str,
  f: impl FnOnce(&T) -> R,
) -> Result<R, Missing> {
  let found = try_graph(|g| {
    let node = g.nodes.get(id)?;
    match &node.kind {
      Kind::Signal { value } => Some((Rc::clone(value), false, false)),
      // A memo whose closure is out of its node (and not because it was
      // disabled) is computing right now.
      Kind::Memo { value } => Some((
        Rc::clone(value),
        true,
        node.compute.is_none() && !node.disabled,
      )),
      Kind::Owner | Kind::Effect { .. } => None,
    }
  });
  let Some(found) = found else {
    return Err(Missing::Gone);
  };
  let Some((value, is_memo, computing)) = found else {
    return Err(Missing::Disposed);
  };
  if computing {
    drop_isolated(f);
    panic!(
      "{handle}<{}> was read while computing its own value (a dependency cycle)",
      type_name::<T>()
    );
  }
  if is_memo {
    refresh(id);
  }
  // Subscribe only after the refresh. If the memo changed, it marked its
  // subscribers dirty, and the reader, which is about to see the new
  // value, shouldn't be one of them yet.
  if track {
    with_graph(|g| subscribe(g, id));
  }
  // If `f` disposes this node's owner, this is the value's last reference.
  let value = DropIsolated(Some(downcast::<T>(value)));
  let Ok(slot) = value.try_borrow() else {
    drop_isolated(f);
    borrow_conflict(format!(
      "{handle}<{}> was read inside its own update() (at {})",
      type_name::<T>(),
      Location::caller()
    ));
  };
  match slot.as_ref() {
    Some(current) => Ok(f(current)),
    None => Err(Missing::NoValue),
  }
}

#[track_caller]
pub(crate) fn read_or_panic<T: 'static, R>(
  id: NodeId,
  track: bool,
  handle: &str,
  f: impl FnOnce(&T) -> R,
) -> R {
  match read(id, track, handle, f) {
    Ok(result) => result,
    Err(missing) => missing.panic::<T>(handle),
  }
}

/// Raises a borrow-conflict panic. Inside a memo it is a [`BorrowConflict`]
/// for [`run`] to recognize, raised without the panic hook; `run` raises a
/// readable panic once the conflict has left the memos. Anywhere else it is
/// a plain panic.
#[track_caller]
pub(crate) fn borrow_conflict(message: String) -> ! {
  if with_graph(|g| g.memo_running()) {
    panic::resume_unwind(Box::new(BorrowConflict(message)));
  }
  panic!("{message}");
}

/// Changes a signal's value through `f`, then marks its dependents stale and
/// queues effects, even if `f` panics. Whatever `f` returns (such as the old
/// value) is handed back for the caller to drop, outside every borrow.
///
/// # Panics
///
/// Inside a memo, because a memo that writes a signal it read would leave
/// itself stale and its readers would miss every later change. Also passes
/// on a panic from `f`, after marking.
#[track_caller]
pub(crate) fn write<T: 'static, R>(
  id: NodeId,
  f: impl FnOnce(&mut Option<T>) -> R,
) -> Result<R, Missing> {
  let found = try_graph(|g| {
    let value = match g.nodes.get(id).map(|node| &node.kind) {
      Some(Kind::Signal { value }) => Some(Rc::clone(value)),
      _ => None,
    };
    (value, g.memo_running())
  });
  // `f` can hold the new value; it is dropped unused on every early exit.
  let (value, memo_running) = match found {
    Some((Some(value), memo_running)) => (value, memo_running),
    Some((None, _)) => {
      drop_isolated(f);
      return Err(Missing::Disposed);
    }
    None => {
      drop_isolated(f);
      return Err(Missing::Gone);
    }
  };
  if memo_running {
    drop_isolated(f);
    panic!(
      "Signal<{}> was written inside a memo: memos must not write signals; use an effect",
      type_name::<T>()
    );
  }
  // If `f` disposes this signal's owner, this is the value's last reference.
  let value = DropIsolated(Some(downcast::<T>(value)));
  let outcome = {
    let Ok(mut slot) = value.try_borrow_mut() else {
      drop_isolated(f);
      panic!(
        "Signal<{}> was written while a with() call on it was still running",
        type_name::<T>()
      );
    };
    panic::catch_unwind(AssertUnwindSafe(|| f(&mut slot)))
  };
  // Even if `f` panicked, it may have changed the value first.
  with_graph(|g| mark_subscribers(g, id, State::Dirty));
  match outcome {
    Ok(result) => Ok(result),
    Err(payload) => panic::resume_unwind(payload),
  }
}

/// [`write`] for `set()` and `update()`: panics on a disposed signal, and
/// returns `None` without writing once the runtime is gone.
#[track_caller]
pub(crate) fn write_or_panic<T: 'static, R>(
  id: NodeId,
  f: impl FnOnce(&mut Option<T>) -> R,
) -> Option<R> {
  match write(id, f) {
    Ok(result) => Some(result),
    Err(Missing::Gone) => None,
    Err(missing) => missing.panic::<T>("Signal"),
  }
}

fn downcast<T: 'static>(value: Rc<dyn Any>) -> Rc<RefCell<Option<T>>> {
  value
    .downcast()
    .expect("invariant: a handle's type matches its node's value type")
}

/// Disposes an owner and everything created under it, child owners and
/// effect scopes included. Their values and closures are dropped after the
/// borrow ends.
pub(crate) fn dispose(owner: NodeId) {
  // If the runtime is gone, its nodes are already being dropped.
  let Some(removed) = try_graph(|g| {
    if let Some(parent) = g.nodes.get(owner).and_then(|node| node.owner)
      && let Some(parent) = g.nodes.get_mut(parent)
    {
      parent.children.retain(|&child| child != owner);
    }
    let mut removed = Vec::new();
    let mut stack = vec![owner];
    while let Some(id) = stack.pop() {
      let Some(node) = g.nodes.remove(id) else {
        continue;
      };
      stack.extend_from_slice(&node.children);
      unsubscribe_all(g, id, &node.sources);
      removed.push(node);
    }
    removed
  }) else {
    return;
  };
  for node in removed {
    drop_isolated(node);
  }
}

#[track_caller]
fn flush() {
  // `None` inside: a memo is running.
  let started = try_graph(|g| (!g.memo_running()).then(|| !mem::replace(&mut g.flushing, true)));
  match started {
    Some(Some(true)) => {}
    Some(None) => {
      panic!("Runtime::flush() was called inside a memo: memos must not run effects")
    }
    // The runtime is gone, or a flush further up the stack is already
    // running and will pick up whatever gets queued.
    None | Some(Some(false)) => return,
  }
  let mut runs: HashMap<NodeId, u32> = HashMap::new();
  loop {
    let round = with_graph(|g| {
      let mut round = mem::take(&mut g.queue);
      round.sort_by_key(|&id| g.nodes.get(id).map_or(0, |node| node.seq));
      round
    });
    if round.is_empty() {
      break;
    }
    let mut round = round.into_iter();
    while let Some(id) = round.next() {
      // From here on, a new write queues the effect for the next round.
      let runnable = with_graph(|g| match g.nodes.get_mut(id) {
        Some(Node {
          kind: Kind::Effect { queued },
          disabled,
          ..
        }) => {
          *queued = false;
          !*disabled
        }
        _ => false,
      });
      if !runnable {
        continue;
      }
      let count = runs.entry(id).or_insert(0);
      if *count >= MAX_RUNS_PER_FLUSH {
        let what = with_graph(|g| describe(g, id));
        tracing::error!(
          "{what} is in a dependency cycle: it ran {MAX_RUNS_PER_FLUSH} times in one \
                     flush and was queued again, so it is disabled"
        );
        disable(id);
        continue;
      }
      match panic::catch_unwind(AssertUnwindSafe(|| refresh(id))) {
        Ok(ran) => *count += u32::from(ran),
        Err(payload) => {
          // Only a borrow conflict from a memo this effect reads
          // gets here, when flush() runs inside with() or update().
          // Leave this effect and the rest of the round queued, end
          // the flush, and pass the panic on to flush()'s caller.
          with_graph(|g| {
            if let Some(Node {
              kind: Kind::Effect { queued },
              ..
            }) = g.nodes.get_mut(id)
              && !*queued
            {
              *queued = true;
              g.queue.push(id);
            }
            // These still have `queued` set.
            g.queue.extend(round);
            g.flushing = false;
          });
          panic::resume_unwind(payload);
        }
      }
    }
  }
  with_graph(|g| g.flushing = false);
}

/// Brings a memo or effect up to date, running it if needed. Returns whether
/// it ran (for a memo, whether its value changed). Signals and owners are
/// always up to date.
fn refresh(id: NodeId) -> bool {
  let sources = with_graph(|g| match g.nodes.get(id) {
    Some(node) if !node.disabled && node.state == State::Check => node.sources.clone(),
    _ => Vec::new(),
  });
  // A memo this node read may have changed. Refresh those memos in order.
  // One whose value changes marks this node dirty, and then there's no
  // need to look further.
  for source in sources {
    refresh(source);
    if with_graph(|g| g.nodes.get(id).map(|node| node.state)) != Some(State::Check) {
      break;
    }
  }
  let dirty = with_graph(|g| match g.nodes.get_mut(id) {
    Some(node) if !node.disabled => {
      if node.state == State::Check {
        node.state = State::Clean;
      }
      node.state == State::Dirty
    }
    _ => false,
  });
  dirty && run(id)
}

/// Runs a memo's or effect's closure with no borrow held, recording what it
/// reads. Returns whether it ran (for a memo, whether its value changed). A
/// panic is caught, logged, and disables the node, except a memo's borrow
/// conflict, which goes on to its reader.
fn run(id: NodeId) -> bool {
  let taken = with_graph(|g| {
    let node = g.nodes.get_mut(id)?;
    let compute = node.compute.take()?;
    let is_memo = matches!(node.kind, Kind::Memo { .. });
    // Clean before the run, so a write during the run that affects this
    // node marks it dirty again.
    node.state = State::Clean;
    // Dependencies are recorded afresh on every run.
    let sources = mem::take(&mut node.sources);
    unsubscribe_all(g, id, &sources);
    Some((compute, is_memo))
  });
  let Some((mut compute, is_memo)) = taken else {
    return false;
  };

  let outer = with_graph(|g| {
    (
      g.observer.replace(id),
      g.running.replace(Running { id, is_memo }),
    )
  });
  // `AssertUnwindSafe`: after a panic the closure is either dropped without
  // being called again, or, after a borrow conflict, called again from the
  // start with its value untouched.
  let result = panic::catch_unwind(AssertUnwindSafe(&mut compute));
  with_graph(|g| (g.observer, g.running) = outer);

  let payload = match result {
    Ok(Computed { changed, leftover }) => {
      let orphan = put_back(id, compute, |g| {
        if changed {
          mark_subscribers(g, id, State::Dirty);
        }
      });
      // After the readers were told, so a panic in the old value's
      // `Drop` can't hide the change.
      drop_isolated(leftover);
      drop_isolated(orphan);
      return changed;
    }
    Err(payload) => payload,
  };
  match payload.downcast::<BorrowConflict>() {
    Ok(conflict) => {
      // Not this memo's fault: leave it dirty, so the next read
      // recomputes it, and pass the panic on to its reader. A memo
      // reading this one handles it the same way; past the memos it
      // becomes a readable panic.
      let orphan = put_back(id, compute, |g| {
        if let Some(node) = g.nodes.get_mut(id) {
          node.state = State::Dirty;
        }
      });
      drop_isolated(orphan);
      if with_graph(|g| g.memo_running()) {
        panic::resume_unwind(conflict);
      }
      panic!("{}", conflict.0);
    }
    Err(payload) => {
      let what = with_graph(|g| describe(g, id));
      tracing::error!(
        "{what} panicked and is disabled: {}",
        panic_message(payload.as_ref())
      );
      // Disable first, so nothing the closure's values do while they
      // drop can queue or run it again.
      disable(id);
      drop_isolated(compute);
      drop_isolated(payload);
      false
    }
  }
}

/// Puts a closure back into its node and then runs `then`. If the closure
/// disposed its own owner, the node is gone: the closure is handed back to
/// be dropped outside the borrow.
fn put_back(id: NodeId, compute: Compute, then: impl FnOnce(&mut Graph)) -> Option<Compute> {
  with_graph(|g| {
    let Some(node) = g.nodes.get_mut(id) else {
      return Some(compute);
    };
    node.compute = Some(compute);
    then(g);
    None
  })
}

/// Stops a memo or effect for good. A disabled memo keeps its last value. A
/// scoped effect's last scope is disposed, everything in it included.
fn disable(id: NodeId) {
  let Some((compute, scopes)) = with_graph(|g| {
    let node = g.nodes.get_mut(id)?;
    node.disabled = true;
    let sources = mem::take(&mut node.sources);
    let compute = node.compute.take();
    let scopes = mem::take(&mut node.children);
    unsubscribe_all(g, id, &sources);
    Some((compute, scopes))
  }) else {
    return;
  };
  drop_isolated(compute);
  for scope in scopes {
    dispose(scope);
  }
}

/// Marks what read `id` as `state`, everything further downstream as
/// `Check`, and queues the effects among them.
fn mark_subscribers(g: &mut Graph, id: NodeId, state: State) {
  let Some(node) = g.nodes.get(id) else {
    return;
  };
  let mut stack: Vec<(NodeId, State)> = node.subscribers.iter().map(|&s| (s, state)).collect();
  while let Some((id, state)) = stack.pop() {
    let Some(node) = g.nodes.get_mut(id) else {
      continue;
    };
    if node.disabled || node.state >= state {
      continue;
    }
    // A node that was already stale has marked its own subscribers.
    let was_clean = node.state == State::Clean;
    node.state = state;
    match &mut node.kind {
      Kind::Effect { queued } if !*queued => {
        *queued = true;
        g.queue.push(id);
      }
      Kind::Memo { .. } if was_clean => {
        stack.extend(node.subscribers.iter().map(|&s| (s, State::Check)));
      }
      _ => {}
    }
  }
}

/// Makes the running memo or effect, if any, depend on `source`.
fn subscribe(g: &mut Graph, source: NodeId) {
  let Some(observer) = g.observer else {
    return;
  };
  if observer == source || !g.nodes.contains_key(source) {
    return;
  }
  // The observer can be gone if its closure disposed its own owner.
  let Some(node) = g.nodes.get_mut(observer) else {
    return;
  };
  if node.sources.contains(&source) {
    return;
  }
  node.sources.push(source);
  if let Some(source) = g.nodes.get_mut(source) {
    source.subscribers.push(observer);
  }
}

/// Removes `id` from the subscriber lists of everything in `sources`.
fn unsubscribe_all(g: &mut Graph, id: NodeId, sources: &[NodeId]) {
  for &source in sources {
    if let Some(source) = g.nodes.get_mut(source) {
      source.subscribers.retain(|&s| s != id);
    }
  }
}

/// How log lines name a node: its debug name if it has one, otherwise its
/// creation number and the name of its nearest named ancestor. Scopes and
/// rows are rarely named; the widget above them usually is.
fn describe(g: &Graph, id: NodeId) -> String {
  let Some(node) = g.nodes.get(id) else {
    return String::from("a disposed node");
  };
  let kind = node.kind.name();
  if let Some(name) = &node.name {
    return format!("{kind} {name}");
  }
  let mut parent = node.owner;
  while let Some(ancestor) = parent.and_then(|parent| g.nodes.get(parent)) {
    if let Some(name) = &ancestor.name {
      return format!("unnamed {kind} #{} in {name}", node.seq);
    }
    parent = ancestor.owner;
  }
  format!("unnamed {kind} #{}", node.seq)
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
  if let Some(message) = payload.downcast_ref::<&str>() {
    message
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message
  } else if let Some(conflict) = payload.downcast_ref::<BorrowConflict>() {
    &conflict.0
  } else {
    "(panic payload is not a string)"
  }
}
