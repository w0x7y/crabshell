//! [`SessionLock`], a lock screen over every output, and [`Lock`], the
//! live one the platform locks the session for and builds each output's
//! lock surface from.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use crabshell_reactive::{ReadSignal, Signal, untrack};

use crate::cx::{Anchor, Cx};
use crate::input::{Handler, panic_message};
use crate::keyboard::{KeyEvent, KeyHandler};
use crate::prop::{IntoProp, Prop, PropKind};
use crate::style::Style;
use crate::tree::{Tree, borrow};
use crate::widgets::Widget;
use crate::window::{Output, Window};

/// A `surface` builder, as stored.
type Build = Rc<dyn Fn(&Cx, &Output) -> Box<dyn Widget>>;

/// An `on_key` handler, as stored.
type OnKey = Rc<dyn Fn(&Cx, &KeyEvent) -> bool>;

/// The style, shared by every surface, the builder and `on_key`.
type Parts = (Option<Rc<dyn Fn() -> Style>>, Option<Build>, Option<OnKey>);

/// A lock screen: while `locked` is true, the compositor shows only its
/// surfaces, one covering each output, and nothing else gets input. Lock
/// with `cx.session_lock` from the `crabshell` crate. Every property
/// takes a value or a closure.
///
/// ```
/// use crabshell_ui::{Color, SessionLock, Style, Text};
///
/// let lock = SessionLock::new()
///     .locked(true)
///     .style(Style::new().background(Color::rgb(0, 0, 0)))
///     .surface(|_cx, output| Text::new(format!("locked on {}", output.name())));
/// ```
#[derive(Default)]
pub struct SessionLock {
  locked: Option<Prop<bool>>,
  style: Option<Prop<Style>>,
  surface: Option<Build>,
  on_key: Option<OnKey>,
}

impl SessionLock {
  /// A lock that never locks, with nothing on its surfaces.
  pub fn new() -> SessionLock {
    SessionLock::default()
  }

  /// Sets whether the session is locked. Turning it false unlocks, as
  /// after the user's password was checked.
  pub fn locked(mut self, locked: impl IntoProp<bool>) -> SessionLock {
    self.locked = Some(locked.into_prop());
    self
  }

  /// Sets the style of every output's surface. Its color and font are
  /// inherited by every [`Text`](crate::Text) on them that doesn't set its
  /// own.
  pub fn style(mut self, style: impl IntoProp<Style>) -> SessionLock {
    self.style = Some(style.into_prop());
    self
  }

  /// Sets what each output's surface shows: `build` runs for every output
  /// while the session locks, and for each output added while it is
  /// locked, under a context that goes with that surface.
  pub fn surface<W: Widget>(mut self, build: impl Fn(&Cx, &Output) -> W + 'static) -> SessionLock {
    self.surface = Some(Rc::new(move |cx: &Cx, output: &Output| {
      Box::new(build(cx, output)) as Box<dyn Widget>
    }));
    self
  }

  /// Runs `handler` with every key a surface's focused field or button
  /// didn't use, as [`PanelWindow::on_key`](crate::PanelWindow::on_key)
  /// does, with that surface's context; it returns `true` when it used
  /// the key. Escape clearing the password field is the usual one. A
  /// handler that panics is logged with the surface's path, such as
  /// `SessionLock[DP-1].on_key`, and that surface's stops running it.
  pub fn on_key(mut self, handler: impl Fn(&Cx, &KeyEvent) -> bool + 'static) -> SessionLock {
    self.on_key = Some(Rc::new(handler));
    self
  }
}

/// A [`SessionLock`] as the platform holds it: whether the config wants
/// the session locked, what the compositor says, and each output's lock
/// surface. Dropping it disposes its owner, which frees everything it
/// made.
pub struct Lock {
  cx: Cx,
  /// What `locked` says.
  wanted: Signal<bool>,
  /// What the compositor says.
  locked: Signal<bool>,
  /// Kept in the reactive runtime, which drops the user's closures when
  /// the lock's owner goes.
  parts: Signal<Parts>,
}

impl Lock {
  /// Builds `lock` under a new child owner of `cx`. A `locked` binding is
  /// an effect, so it first runs at the next flush.
  ///
  /// # Panics
  ///
  /// If `cx`'s owner was disposed, as [`Cx::signal`] does.
  pub fn new(cx: &Cx, lock: SessionLock) -> Lock {
    let cx = cx.child().named("SessionLock");
    let wanted = cx.signal(false);
    match lock.locked.map(|locked| locked.0) {
      None => {}
      Some(PropKind::Value(value)) => wanted.set(value),
      Some(PropKind::Binding(mut compute)) => {
        cx.effect(move || wanted.set(compute()))
          .named("SessionLock.locked");
      }
    }
    Lock {
      wanted,
      locked: cx.signal(false),
      parts: cx.signal((lock.style.map(Prop::shared), lock.surface, lock.on_key)),
      cx,
    }
  }

  /// Whether `locked` says to lock; `None` once the lock's owner is gone.
  pub fn wanted(&self) -> Option<bool> {
    untrack(|| self.wanted.try_get())
  }

  /// `true` while the compositor says the session is locked.
  pub fn locked(&self) -> ReadSignal<bool> {
    self.locked.read_only()
  }

  /// Records what the compositor said; a value it holds already is left
  /// alone, so nothing reruns.
  pub fn set_locked(&self, locked: bool) {
    if untrack(|| self.locked.try_get()) == Some(!locked) {
      self.locked.set(locked);
    }
  }

  /// Builds `output`'s lock surface under a new child owner, its path
  /// `SessionLock[DP-1]`. A `surface` builder that panics is logged with
  /// that path, and the surface shows only the lock's style. `None` once
  /// the lock's owner is gone.
  pub fn surface(&self, output: &Output) -> Option<Window> {
    let (style, build, on_key) = untrack(|| self.parts.try_with(Clone::clone))?;
    let path: Rc<str> = format!("SessionLock[{}]", output.name()).into();
    let cx = self
      .cx
      .child()
      .named(path.to_string())
      .at(Rc::new(Anchor::default()));
    // Untracked, as a popup's builder: nothing reruns it.
    let built = panic::catch_unwind(AssertUnwindSafe(|| {
      untrack(|| build.map(|build| build(&cx, output).into_element()))
    }));
    let child = built.unwrap_or_else(|payload| {
      tracing::error!(
        "{path} panicked while it was built, so it shows only the lock's style: {}",
        panic_message(payload.as_ref())
      );
      None
    });
    let owner = cx.owner();
    let tree = Rc::new_cyclic(|this| {
      RefCell::new(Tree::new(
        this.clone(),
        "SessionLock",
        output.clone(),
        cx,
        path,
      ))
    });
    let style = style.map(|style| (move || style()).into_prop());
    let mut mounted = borrow(&tree);
    // Each surface's own, so a panic disables it on that surface only.
    mounted.keyboard.handler = on_key.map(|on_key| {
      Rc::new(Handler::new(move |cx: &Cx, event: &KeyEvent| {
        on_key(cx, event)
      })) as KeyHandler
    });
    mounted.mount_root(style, child);
    drop(mounted);
    Some(Window { tree, owner })
  }
}

impl Drop for Lock {
  fn drop(&mut self) {
    self.cx.owner().dispose();
  }
}
