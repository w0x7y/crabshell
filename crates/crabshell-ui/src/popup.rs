//! Popups: [`PopupWindow`], the description of one, and how a widget opens
//! and closes it with [`Cx::open_popup`] and [`Cx::close_popup`].

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use crabshell_reactive::untrack;

use crate::cx::{Anchor, Cx};
use crate::geometry::Rect;
use crate::input::panic_message;
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::tree::{PopupState, Tree, borrow};
use crate::widgets::Widget;
use crate::window::Window;

/// A popup: a window next to the widget that opened it, sized to fit its
/// one child. Open one with [`Cx::open_popup`].
///
/// ```
/// use crabshell_ui::{Button, PopupWindow, Text};
///
/// let button = Button::new(Text::new("open")).on_click(|cx| {
///     cx.open_popup(|_cx| PopupWindow::new().child(Text::new("hello")))
/// });
/// ```
#[derive(Default)]
pub struct PopupWindow {
  style: Option<Prop<Style>>,
  child: Option<Box<dyn Widget>>,
}

impl PopupWindow {
  /// An empty popup.
  pub fn new() -> PopupWindow {
    PopupWindow::default()
  }

  /// Sets the popup's style. Its color and font are inherited by every
  /// [`Text`](crate::Text) in it that doesn't set its own.
  pub fn style(mut self, style: impl IntoProp<Style>) -> PopupWindow {
    self.style = Some(style.into_prop());
    self
  }

  /// Sets the popup's one child. The popup is as large as the child wants
  /// to be, plus the style's padding.
  pub fn child(mut self, child: impl Widget) -> PopupWindow {
    self.child = Some(Box::new(child));
    self
  }
}

impl Cx {
  /// Opens a popup next to the widget this context belongs to, such as the
  /// button whose handler got it. `build` describes the popup and gets its
  /// context: what it creates there goes when the popup does.
  ///
  /// The popup belongs to the widget, so it closes when the widget goes,
  /// as well as on [`close_popup`](Self::close_popup) and when the user
  /// clicks outside it. If `build` panics, that is logged with the
  /// widget's path and nothing opens. A context of no widget, such as the
  /// one `per_output` gives, logs that and opens nothing.
  pub fn open_popup(&self, build: impl FnOnce(&Cx) -> PopupWindow) {
    let Some((anchor, (tree, node))) = self
      .anchor
      .clone()
      .and_then(|anchor| anchor.get().map(|found| (anchor, found)))
    else {
      return tracing::warn!("open_popup needs a widget's context, such as a handler's");
    };
    let opener = {
      let tree = tree
        .try_borrow()
        .expect("invariant: a window's tree is not borrowed while user code runs");
      tree
        .node(node)
        .map(|node| node.path.clone())
        .zip(Some(tree.output.clone()))
    };
    let Some((opener, output)) = opener else {
      return;
    };
    let path: Rc<str> = format!("{opener}/PopupWindow").into();
    let root = Rc::new(Anchor::default());
    let cx = self.child().named(path.to_string()).at(Rc::clone(&root));
    // Untracked, so an effect that opens a popup doesn't rerun, and open
    // another, when something `build` read changes.
    let built = panic::catch_unwind(AssertUnwindSafe(|| {
      untrack(|| {
        let popup = build(&cx);
        let child = popup.child.map(|child| child.into_element());
        (popup.style, child)
      })
    }));
    let (style, child) = match built {
      Ok(built) => built,
      Err(payload) => {
        tracing::error!(
          "{path} panicked while it was built, so it doesn't open: {}",
          panic_message(payload.as_ref())
        );
        cx.owner().dispose();
        return;
      }
    };
    let owner = cx.owner();
    let alive = cx.signal(());
    let popup = Rc::new_cyclic(|this| {
      let mut tree = Tree::new(this.clone(), "PopupWindow", output, cx, path);
      tree.popup = Some(PopupState {
        opener: anchor,
        close: false,
        alive,
      });
      RefCell::new(tree)
    });
    borrow(&popup).mount_root(style, child);
    borrow(&tree).popups.push(Window { tree: popup, owner });
  }

  /// Closes the popup this context's widget is in, with every popup it
  /// opened and every popup that opened it, as a menu does once one of
  /// its entries is clicked. Outside a popup it logs that and does
  /// nothing.
  pub fn close_popup(&self) {
    if !self.close_popups_if_any() {
      tracing::warn!("close_popup was called outside a popup");
    }
  }

  /// [`close_popup`](Self::close_popup), saying whether there was a popup
  /// to close. It marks this popup and each popup that opened it, one
  /// tree borrowed at a time; the popups they opened close with them.
  pub(crate) fn close_popups_if_any(&self) -> bool {
    let mut next = self
      .anchor
      .as_ref()
      .and_then(|anchor| anchor.get())
      .map(|(tree, _)| tree);
    let mut closed = false;
    while let Some(tree) = next.take() {
      let mut tree = borrow(&tree);
      let Some(popup) = tree.popup.as_mut() else {
        break;
      };
      popup.close = true;
      closed = true;
      next = popup.opener.get().map(|(tree, _)| tree);
    }
    closed
  }
}

impl Window {
  /// Takes the popups this window's widgets opened since the last call.
  pub fn take_popups(&self) -> Vec<Window> {
    std::mem::take(&mut borrow(&self.tree).popups)
  }

  /// For a popup, the rectangle of the widget that opened it, at its
  /// window's last layout, in that window's logical pixels. The platform
  /// places the popup next to it. `None` for a panel, once the widget is
  /// gone, or before its first layout.
  pub fn anchor(&self) -> Option<Rect> {
    let opener = Rc::clone(&self.tree.borrow().popup.as_ref()?.opener);
    let (tree, node) = opener.get()?;
    let tree = tree.borrow();
    tree.node(node)?.rect
  }

  /// Whether this window is a popup.
  pub fn is_popup(&self) -> bool {
    self.tree.borrow().popup.is_some()
  }

  /// Whether this popup should close: [`Cx::close_popup`] asked, or the
  /// widget that opened it is gone. Always `false` for a panel.
  pub fn wants_close(&self) -> bool {
    let tree = self.tree.borrow();
    tree
      .popup
      .as_ref()
      .is_some_and(|popup| popup.close || popup.alive.try_with(|_| ()).is_none())
  }
}
