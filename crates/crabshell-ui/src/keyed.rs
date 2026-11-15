//! `children_keyed`: a list of children that follows a list of items,
//! keeping each child while its item's key stays in the list, and while
//! it leaves when the list has an exit.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::{self, AssertUnwindSafe};
use std::rc::{Rc, Weak};
use std::time::Duration;

use crabshell_reactive::{Memo, Owner, Signal, untrack};

use crate::cx::Cx;
use crate::input::panic_message;
use crate::prop::Prop;
use crate::timer::Timer;
use crate::tree::{NodeId, Tree, borrow};
use crate::widgets::{Element, Stack, Widget};

/// Where a keyed list lives: which child group of which container.
pub(crate) struct KeyedSlot {
  pub(crate) tree: Weak<RefCell<Tree>>,
  pub(crate) container: NodeId,
  pub(crate) group: usize,
  /// The container's context; each item gets a child of it.
  pub(crate) cx: Cx,
  /// The container's widget path, for log lines.
  pub(crate) path: Rc<str>,
}

/// Starts a keyed list once its container is in a window. The closures are
/// type-erased here, so containers don't carry the item types.
pub(crate) type KeyedMount = Box<dyn FnOnce(KeyedSlot)>;

/// How long a leaving child stays, as a value or a binding.
type Exit = Rc<dyn Fn() -> Duration>;

/// A keyed list before its container is in a window, with the exit that
/// `exit` may still set.
pub(crate) struct KeyedList {
  pub(crate) exit: Option<Prop<Duration>>,
  start: Box<dyn FnOnce(KeyedSlot, Option<Exit>)>,
}

impl KeyedList {
  pub(crate) fn into_mount(self) -> KeyedMount {
    let KeyedList { exit, start } = self;
    Box::new(move |slot| start(slot, exit.map(Prop::shared)))
  }
}

/// One item's child.
struct Entry<T> {
  /// Owns the item's signal, memo and everything `view` created.
  cx: Cx,
  value: Signal<T>,
  node: NodeId,
  /// What its context's `leaving` reads, in a list with an exit.
  leaving: Option<Signal<bool>>,
  /// The owner and timer of its exit while it leaves. The owner goes when
  /// its key comes back, so a child holds one timer however often it
  /// leaves.
  exit: Option<(Owner, Timer)>,
}

/// The element `view` builds. If it panics, that is logged and an empty
/// `Stack` stands in, so the other items still show and keep their places.
fn build<W: Widget>(label: &str, view: impl FnOnce() -> W) -> Element {
  match panic::catch_unwind(AssertUnwindSafe(|| Box::new(view()).into_element())) {
    Ok(element) => element,
    Err(payload) => {
      tracing::error!(
        "{label}: building an item panicked; it shows empty: {}",
        panic_message(payload.as_ref())
      );
      Box::new(Stack::new()).into_element()
    }
  }
}

/// How long a child leaves for, read untracked as its key leaves. If the
/// binding panics, that is logged and the child goes at once.
fn exit_duration(label: &str, exit: &dyn Fn() -> Duration) -> Duration {
  match panic::catch_unwind(AssertUnwindSafe(|| untrack(exit))) {
    Ok(duration) => duration,
    Err(payload) => {
      tracing::error!(
        "{label}: its exit panicked; the child goes at once: {}",
        panic_message(payload.as_ref())
      );
      Duration::ZERO
    }
  }
}

pub(crate) fn mount<T, K, W>(
  mut items: impl FnMut() -> Vec<T> + 'static,
  mut key: impl FnMut(&T) -> K + 'static,
  mut view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
) -> KeyedList
where
  T: Clone + PartialEq + 'static,
  K: Eq + Hash + 'static,
  W: Widget,
{
  let start = move |slot: KeyedSlot, exit: Option<Exit>| {
    let label = format!("{}.children", slot.path);
    let mut entries: HashMap<K, Entry<T>> = HashMap::new();
    let mut order: Vec<NodeId> = Vec::new();
    let mut warned = false;
    // An exit timer sets `finished` as it ends, which reruns the list.
    let exit = exit.map(|exit| (exit, slot.cx.signal(())));
    let cx = slot.cx.clone();
    let name = label.clone();
    cx.effect(move || {
      if let Some((_, finished)) = &exit {
        finished.get();
      }
      let list = items();

      // Keys aren't `Clone`, so duplicates are found by comparing with
      // the keys kept so far. Lists in a bar are short.
      let mut next: Vec<(K, T)> = Vec::with_capacity(list.len());
      for item in list {
        let item_key = key(&item);
        if next.iter().any(|(kept, _)| *kept == item_key) {
          if !warned {
            tracing::warn!("{label}: two items have the same key; showing the first");
            warned = true;
          }
          continue;
        }
        next.push((item_key, item));
      }

      // Reuse the children whose keys stay. Build the new ones' widgets
      // now, before the tree is borrowed, without tracking what `view`
      // reads, so this effect doesn't rerun for it.
      let mut old = std::mem::take(&mut entries);
      let mut added = Vec::new();
      let mut kept = Vec::with_capacity(next.len());
      for (item_key, item) in next {
        match old.remove(&item_key) {
          Some(mut entry) => {
            // Its memo tells the child only if the value changed.
            entry.value.set(item);
            // Back while leaving: it stays, and its exit's timer goes.
            if let Some(leaving) = entry.leaving
              && leaving.get_untracked()
            {
              leaving.set(false);
              if let Some((owner, _)) = entry.exit.take() {
                owner.dispose();
              }
            }
            kept.push((item_key, Some(entry)));
          }
          None => {
            let cx = slot.cx.child();
            let leaving = exit.as_ref().map(|_| cx.signal(false));
            let cx = match leaving {
              Some(leaving) => cx.in_item(leaving.read_only()),
              None => cx,
            };
            let value = cx.signal(item);
            let memo = cx.memo(move || value.get());
            let element = untrack(|| build(&label, || view(&cx, memo)));
            added.push((cx, value, leaving, element));
            kept.push((item_key, None));
          }
        }
      }

      // The keys that left go now, or start or go on leaving.
      let mut removed = Vec::new();
      let mut going = Vec::new();
      for (item_key, mut entry) in old {
        let (Some((duration, finished)), Some(leaving)) = (exit.as_ref(), entry.leaving) else {
          removed.push(entry);
          continue;
        };
        if !leaving.get_untracked() {
          let duration = exit_duration(&label, duration.as_ref());
          // An exit of 0 removes at once, as a list without one does.
          if duration.is_zero() {
            removed.push(entry);
            continue;
          }
          leaving.set(true);
          let finished = *finished;
          let scope = entry.cx.child();
          let timer = scope.after(duration, move || {
            finished.try_set(());
          });
          entry.exit = Some((scope.owner(), timer));
          going.push((item_key, entry));
        } else if entry
          .exit
          .is_some_and(|(_, timer)| untrack(|| timer.running()))
        {
          going.push((item_key, entry));
        } else {
          removed.push(entry);
        }
      }

      let Some(tree) = slot.tree.upgrade() else {
        return;
      };
      {
        let mut tree = borrow(&tree);
        let mut added = added.into_iter();
        let mut nodes = Vec::with_capacity(kept.len());
        for (item_key, entry) in kept {
          let entry = match entry {
            Some(entry) => entry,
            None => {
              let (cx, value, leaving, element) = added
                .next()
                .expect("invariant: every new key has a built element");
              let node = tree.mount(element, &cx, slot.container);
              Entry {
                cx,
                value,
                node,
                leaving,
                exit: None,
              }
            }
          };
          nodes.push(entry.node);
          entries.insert(item_key, entry);
        }
        // A leaving child goes right after the child before it in the last
        // order, and one that was first stays first.
        let mut at = 0;
        for node in &order {
          if going.iter().any(|(_, entry)| entry.node == *node) {
            nodes.insert(at, *node);
            at += 1;
          } else if let Some(index) = nodes.iter().position(|kept| kept == node) {
            at = index + 1;
          }
        }
        entries.extend(going);
        for entry in &removed {
          tree.unmount(entry.node);
        }
        if nodes != order {
          tree.set_group(slot.container, slot.group, nodes.clone());
          order = nodes;
        }
      }
      // Free the removed children only after the tree is released, since
      // that drops user values and closures.
      for entry in removed {
        entry.cx.owner().dispose();
      }
    })
    .named(name);
  };
  KeyedList {
    exit: None,
    start: Box::new(start),
  }
}
