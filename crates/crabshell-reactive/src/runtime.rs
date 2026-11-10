//! The runtime: this thread's arena of nodes, the dependency edges between
//! them, and `flush`.
//!
//! Every handle (`Signal`, `Effect`, `Owner`) is a [`NodeId`] into one arena
//! that lives in a thread-local `RefCell`. Functions here borrow it for a few
//! lines at a time and never while user code runs. An effect's closure is
//! taken out of its node before it is called and put back after, and user
//! values are dropped only after the borrow has ended. So user code can call
//! back into the runtime freely.

use std::any::{Any, type_name};
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::mem;
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

/// An effect's closure with its type erased.
pub(crate) type Compute = Box<dyn FnMut()>;

/// Why a read or write found no value. This can happen in correct code, so
/// `try_get()` and `try_set()` report it instead of panicking.
pub(crate) enum Missing {
    /// The node's owner was disposed.
    Disposed,
}

impl Missing {
    #[track_caller]
    pub(crate) fn panic<T>(self, handle: &str) -> ! {
        let ty = type_name::<T>();
        match self {
            Missing::Disposed => panic!(
                "{handle}<{ty}> was used after its owner was disposed (try_get() returns None instead)"
            ),
        }
    }
}

struct Node {
    kind: Kind,
    /// The owner this node was created under. `None` for a root owner.
    owner: Option<NodeId>,
    /// Creation order. Effects in one flush round run in this order.
    seq: u64,
    /// Debug name, such as a widget path, used in log lines.
    name: Option<String>,
    state: State,
    /// Set once the effect was caught in a cycle. A disabled node never runs
    /// again.
    disabled: bool,
    /// The effect's closure. `None` for signals and owners, while the
    /// closure is running, and once the node is disabled.
    compute: Option<Compute>,
    /// Everything created under this owner.
    children: Vec<NodeId>,
    /// The signals this effect read on its last run.
    sources: Vec<NodeId>,
    /// The effects that read this signal on their last run.
    subscribers: Vec<NodeId>,
}

enum Kind {
    Owner,
    /// `value` is an `Rc<RefCell<Option<T>>>` that always holds `Some`.
    Signal {
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
            Kind::Effect { .. } => "effect",
        }
    }
}

/// How up to date an effect is. Signals and owners stay `Clean`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum State {
    Clean,
    /// Something it read has changed, so it must run again.
    Dirty,
}

#[derive(Default)]
struct Graph {
    nodes: SlotMap<NodeId, Node>,
    /// Effects waiting for the next flush round.
    queue: Vec<NodeId>,
    /// The effect that what is read now becomes a dependency of: the one
    /// whose closure is running. `None` outside any run.
    observer: Option<NodeId>,
    next_seq: u64,
    flushing: bool,
}

thread_local! {
    static GRAPH: RefCell<Graph> = RefCell::new(Graph::default());
}

fn with_graph<R>(f: impl FnOnce(&mut Graph) -> R) -> R {
    GRAPH.with(|graph| {
        let mut graph = graph
            .try_borrow_mut()
            .expect("invariant: the runtime is never borrowed while user code runs");
        f(&mut graph)
    })
}

/// True if nothing holds a borrow of this thread's runtime.
#[cfg(test)]
pub(crate) fn is_unborrowed() -> bool {
    GRAPH.with(|graph| graph.try_borrow_mut().is_ok())
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
    pub fn flush(&self) {
        flush();
    }

    /// The number of live nodes (owners, signals and effects) on this
    /// thread. Tests use it to check that disposing an owner frees
    /// everything created under it.
    pub fn node_count(&self) -> usize {
        with_graph(|g| g.nodes.len())
    }
}

#[track_caller]
pub(crate) fn create_owner(parent: Option<NodeId>) -> NodeId {
    create(parent, Kind::Owner, None)
}

#[track_caller]
pub(crate) fn create_signal(owner: NodeId, value: Rc<dyn Any>) -> NodeId {
    create(Some(owner), Kind::Signal { value }, None)
}

/// The new effect is queued straight away. It runs in the next flush round:
/// later in the current flush if one is running, otherwise at the next flush.
#[track_caller]
pub(crate) fn create_effect(owner: NodeId, compute: Compute) -> NodeId {
    create(Some(owner), Kind::Effect { queued: true }, Some(compute))
}

#[track_caller]
fn create(owner: Option<NodeId>, kind: Kind, compute: Option<Compute>) -> NodeId {
    if !with_graph(|g| owner.is_none_or(|owner| g.nodes.contains_key(owner))) {
        // `kind` and `compute` can hold user values. They are dropped while
        // this panic unwinds, after the borrow above has ended.
        panic!(
            "tried to create a new {} under an owner that was already disposed",
            kind.name()
        );
    }
    with_graph(|g| {
        let seq = g.next_seq;
        g.next_seq += 1;
        let is_effect = matches!(kind, Kind::Effect { .. });
        // Effects start dirty: they haven't run yet.
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
    with_graph(|g| {
        if let Some(node) = g.nodes.get_mut(id) {
            node.name = Some(name);
        }
    });
}

/// Reads a signal's value. If `track` is true and an effect is running, it
/// starts depending on this signal.
#[track_caller]
pub(crate) fn read<T: 'static, R>(
    id: NodeId,
    track: bool,
    handle: &str,
    f: impl FnOnce(&T) -> R,
) -> Result<R, Missing> {
    let value = with_graph(|g| match g.nodes.get(id).map(|node| &node.kind) {
        Some(Kind::Signal { value }) => Some(Rc::clone(value)),
        _ => None,
    });
    let Some(value) = value else {
        return Err(Missing::Disposed);
    };
    if track {
        with_graph(|g| subscribe(g, id));
    }
    let value = downcast::<T>(value);
    let Ok(slot) = value.try_borrow() else {
        panic!(
            "{handle}<{}> was read inside its own update()",
            type_name::<T>()
        );
    };
    let current = slot
        .as_ref()
        .expect("invariant: a signal always holds a value");
    Ok(f(current))
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

/// Changes a signal's value through `f`, then marks its dependents stale and
/// queues effects. Whatever `f` returns (such as the old value) is handed
/// back for the caller to drop, outside every borrow.
#[track_caller]
pub(crate) fn write<T: 'static, R>(
    id: NodeId,
    f: impl FnOnce(&mut Option<T>) -> R,
) -> Result<R, Missing> {
    let value = with_graph(|g| match g.nodes.get(id).map(|node| &node.kind) {
        Some(Kind::Signal { value }) => Some(Rc::clone(value)),
        _ => None,
    });
    let Some(value) = value else {
        return Err(Missing::Disposed);
    };
    let value = downcast::<T>(value);
    let result = {
        let Ok(mut slot) = value.try_borrow_mut() else {
            panic!(
                "Signal<{}> was written while a with() call on it was still running",
                type_name::<T>()
            );
        };
        f(&mut slot)
    };
    with_graph(|g| mark_subscribers(g, id, State::Dirty));
    Ok(result)
}

/// [`write`] for `set()` and `update()`: panics on a disposed signal.
#[track_caller]
pub(crate) fn write_or_panic<T: 'static, R>(id: NodeId, f: impl FnOnce(&mut Option<T>) -> R) -> R {
    match write(id, f) {
        Ok(result) => result,
        Err(missing) => missing.panic::<T>("Signal"),
    }
}

fn downcast<T: 'static>(value: Rc<dyn Any>) -> Rc<RefCell<Option<T>>> {
    value
        .downcast()
        .expect("invariant: a handle's type matches its node's value type")
}

/// Disposes an owner and everything created under it, child owners
/// included. Their values and closures are dropped after the borrow ends.
pub(crate) fn dispose(owner: NodeId) {
    let removed = with_graph(|g| {
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
    });
    drop(removed);
}

fn flush() {
    // A flush further up the stack is already running and will pick up
    // whatever gets queued.
    if with_graph(|g| mem::replace(&mut g.flushing, true)) {
        return;
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
        for id in round {
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
            } else if refresh(id) {
                *count += 1;
            }
        }
    }
    with_graph(|g| g.flushing = false);
}

/// Runs an effect if something it read has changed. Returns whether it ran.
fn refresh(id: NodeId) -> bool {
    let dirty = with_graph(|g| {
        g.nodes
            .get(id)
            .is_some_and(|node| !node.disabled && node.state == State::Dirty)
    });
    dirty && run(id)
}

/// Runs an effect's closure with no borrow held, recording what it reads.
/// Returns whether it ran.
fn run(id: NodeId) -> bool {
    let taken = with_graph(|g| {
        let node = g.nodes.get_mut(id)?;
        let compute = node.compute.take()?;
        // Clean before the run, so a write during the run that affects this
        // effect marks it dirty again.
        node.state = State::Clean;
        // Dependencies are recorded afresh on every run.
        let sources = mem::take(&mut node.sources);
        unsubscribe_all(g, id, &sources);
        Some(compute)
    });
    let Some(mut compute) = taken else {
        return false;
    };

    let outer = with_graph(|g| g.observer.replace(id));
    compute();
    with_graph(|g| g.observer = outer);

    let orphan = with_graph(|g| {
        // The closure disposed its own owner: hand the closure back to be
        // dropped outside the borrow.
        let Some(node) = g.nodes.get_mut(id) else {
            return Some(compute);
        };
        node.compute = Some(compute);
        None
    });
    drop(orphan);
    true
}

/// Stops an effect for good.
fn disable(id: NodeId) {
    let compute = with_graph(|g| {
        let node = g.nodes.get_mut(id)?;
        node.disabled = true;
        let sources = mem::take(&mut node.sources);
        let compute = node.compute.take();
        unsubscribe_all(g, id, &sources);
        compute
    });
    drop(compute);
}

/// Marks the effects that read `id` as `state` and queues them.
fn mark_subscribers(g: &mut Graph, id: NodeId, state: State) {
    let Some(node) = g.nodes.get(id) else {
        return;
    };
    for subscriber in node.subscribers.clone() {
        let Some(node) = g.nodes.get_mut(subscriber) else {
            continue;
        };
        if node.disabled || node.state >= state {
            continue;
        }
        node.state = state;
        if let Kind::Effect { queued } = &mut node.kind
            && !*queued
        {
            *queued = true;
            g.queue.push(subscriber);
        }
    }
}

/// Makes the running effect, if any, depend on `source`.
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
/// creation number and the name of its nearest named ancestor. Rows are
/// rarely named; the widget above them usually is.
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
