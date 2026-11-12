//! [`Cx`], the context that widget code, event handlers and list views get.

use std::any::{Any, TypeId};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crabshell_reactive::{Effect, Memo, Owner, Signal};

use crate::tree::{NodeId, Tree};

/// The context widget code runs in.
///
/// A `Cx` wraps the reactive [`Owner`] of one part of the shell: a window,
/// a widget or one item of a keyed list. Signals, memos and effects created
/// through it belong to that owner, so they are freed when that part of the
/// shell goes away. Event handlers receive the `Cx` of their button, and
/// [`children_keyed`](crate::Row::children_keyed) views receive the `Cx` of
/// their item.
///
/// A context also carries the shell's services, keyed by type: the
/// platform, and later the clock, Sway, audio and the tray. Every context
/// made from it shares the same map, so a service provided once at the
/// root reaches every widget and handler.
#[derive(Clone)]
pub struct Cx {
  owner: Owner,
  services: Rc<Services>,
  /// The widget this context belongs to, if any, for
  /// [`open_popup`](Self::open_popup). A child context shares it.
  pub(crate) anchor: Option<Rc<Anchor>>,
}

/// Where a widget is: its window's tree and its node. Set once the node
/// exists, which is after its context is made.
#[derive(Default)]
pub(crate) struct Anchor(OnceCell<(Weak<RefCell<Tree>>, NodeId)>);

impl Anchor {
  pub(crate) fn set(&self, tree: Weak<RefCell<Tree>>, node: NodeId) {
    // A second set can't happen: each anchor is made for one node.
    let _ = self.0.set((tree, node));
  }

  /// The tree, while the window is there, and the node.
  pub(crate) fn get(&self) -> Option<(Rc<RefCell<Tree>>, NodeId)> {
    let (tree, node) = self.0.get()?;
    Some((tree.upgrade()?, *node))
  }
}

/// The services of one shell, by type.
#[derive(Default)]
struct Services(RefCell<HashMap<TypeId, Rc<dyn Any>>>);

impl std::fmt::Debug for Cx {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let services = self.services.0.borrow().len();
    f.debug_struct("Cx")
      .field("owner", &self.owner)
      .field("services", &services)
      .finish()
  }
}

impl Cx {
  /// A context that creates everything under `owner`, with no services
  /// yet.
  pub fn new(owner: Owner) -> Cx {
    Cx {
      owner,
      services: Rc::default(),
      anchor: None,
    }
  }

  /// The owner that signals, memos and effects created through this
  /// context belong to.
  pub fn owner(&self) -> Owner {
    self.owner
  }

  /// A context for a part of the shell inside this one: what it creates
  /// is freed when this context's owner is. The services are shared with
  /// the child.
  pub fn child(&self) -> Cx {
    Cx {
      owner: self.owner.child(),
      services: Rc::clone(&self.services),
      anchor: self.anchor.clone(),
    }
  }

  /// Gives the context's owner a debug name, such as a widget path.
  pub(crate) fn named(self, name: impl Into<String>) -> Cx {
    Cx {
      owner: self.owner.named(name),
      ..self
    }
  }

  /// The context of a widget's own node, at `anchor`.
  pub(crate) fn at(self, anchor: Rc<Anchor>) -> Cx {
    Cx {
      anchor: Some(anchor),
      ..self
    }
  }

  /// Makes `service` reachable from every context of this shell, through
  /// [`service`](Self::service), and returns it shared. A service of the
  /// same type provided before is replaced; whoever holds it keeps it.
  pub fn provide<T: 'static>(&self, service: T) -> Rc<T> {
    let service = Rc::new(service);
    let shared: Rc<dyn Any> = Rc::clone(&service) as Rc<dyn Any>;
    let replaced = self
      .services
      .0
      .borrow_mut()
      .insert(TypeId::of::<T>(), shared);
    // Dropped after the map is released: its `Drop` may use a context.
    drop(replaced);
    service
  }

  /// The service of type `T`, if one was provided.
  pub fn service<T: 'static>(&self) -> Option<Rc<T>> {
    let service = self.services.0.borrow().get(&TypeId::of::<T>()).cloned()?;
    service.downcast::<T>().ok()
  }

  /// Creates a [`Signal`] holding `value`. See [`Owner::signal`].
  #[track_caller]
  pub fn signal<T: 'static>(&self, value: T) -> Signal<T> {
    self.owner.signal(value)
  }

  /// Creates a [`Memo`] that derives its value with `f`. See
  /// [`Owner::memo`].
  #[track_caller]
  pub fn memo<T: PartialEq + 'static>(&self, f: impl FnMut() -> T + 'static) -> Memo<T> {
    self.owner.memo(f)
  }

  /// Creates an [`Effect`] that reruns `f` when something it read changes.
  /// See [`Owner::effect`].
  #[track_caller]
  pub fn effect(&self, f: impl FnMut() + 'static) -> Effect {
    self.owner.effect(f)
  }
}
