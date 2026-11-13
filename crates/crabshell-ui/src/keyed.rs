//! `children_keyed`: a list of children that follows a list of items,
//! keeping each child while its item's key stays in the list.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::{self, AssertUnwindSafe};
use std::rc::{Rc, Weak};

use crabshell_reactive::{Memo, Signal, untrack};

use crate::cx::Cx;
use crate::input::panic_message;
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

/// One item's child.
struct Entry<T> {
  /// Owns the item's signal, memo and everything `view` created.
  cx: Cx,
  value: Signal<T>,
  node: NodeId,
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

pub(crate) fn mount<T, K, W>(
  mut items: impl FnMut() -> Vec<T> + 'static,
  mut key: impl FnMut(&T) -> K + 'static,
  mut view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
) -> KeyedMount
where
  T: Clone + PartialEq + 'static,
  K: Eq + Hash + 'static,
  W: Widget,
{
  Box::new(move |slot: KeyedSlot| {
    let label = format!("{}.children", slot.path);
    let mut entries: HashMap<K, Entry<T>> = HashMap::new();
    let mut order: Vec<NodeId> = Vec::new();
    let mut warned = false;
    let cx = slot.cx.clone();
    let name = label.clone();
    cx.effect(move || {
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
          Some(entry) => {
            // Its memo tells the child only if the value changed.
            entry.value.set(item);
            kept.push((item_key, Some(entry)));
          }
          None => {
            let cx = slot.cx.child();
            let value = cx.signal(item);
            let memo = cx.memo(move || value.get());
            let element = untrack(|| build(&label, || view(&cx, memo)));
            added.push((cx, value, element));
            kept.push((item_key, None));
          }
        }
      }
      let removed: Vec<Entry<T>> = old.into_values().collect();

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
              let (cx, value, element) = added
                .next()
                .expect("invariant: every new key has a built element");
              let node = tree.mount(element, &cx, slot.container);
              Entry { cx, value, node }
            }
          };
          nodes.push(entry.node);
          entries.insert(item_key, entry);
        }
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
  })
}
