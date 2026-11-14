//! Windows to capture. A capture names a window by its handle from
//! `ext-foreign-toplevel-list-v1`, while [`Toplevel`](crate::Toplevel) ids
//! come from `wlr-foreign-toplevel-management`, so each ext handle is
//! paired with the wlr window of the same app id and title.

use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
  ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
  ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1;

use crate::foreign_toplevels::cut;
use crate::platform::Platform;

/// The most ext handles kept, as for wlr toplevels.
const MAX_HANDLES: usize = 1024;

/// The ext handles, once a view of a window asked.
#[derive(Default)]
pub(crate) struct ExtToplevels {
  /// Whether a view asked, so the protocols are bound once at most.
  asked: bool,
  list: Option<ExtForeignToplevelListV1>,
  pub(crate) sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
  /// In the order the compositor announced them.
  handles: Vec<Handle>,
  /// Whether handles past the cap were logged.
  warned: bool,
}

/// An ext handle, destroyed when dropped.
struct Handle {
  proxy: ExtForeignToplevelHandleV1,
  /// The app id and title since the last `done`.
  pending: (String, String),
  /// As of the last `done`; `None` before the first.
  current: Option<(String, String)>,
  /// The wlr toplevel's id, once paired.
  paired: Option<u64>,
}

impl Drop for Handle {
  fn drop(&mut self) {
    self.proxy.destroy();
  }
}

/// Pairs each unpaired ext handle, oldest first, with the oldest wlr
/// toplevel of the same app id and title that no handle has. `ext` holds
/// each listed handle's app id and title and its pair; `wlr` holds each
/// listed toplevel's id, app id and title, oldest first.
// shortcut: two windows with the same app id and title may swap, since
// nothing ties a wlr handle to an ext one and only the order agrees;
// follow windows on one protocol once a compositor lists states there.
pub(crate) fn pair<'a>(
  mut ext: Vec<((&'a str, &'a str), &mut Option<u64>)>,
  wlr: &[(u64, (&'a str, &'a str))],
) {
  // A partner gone from the wlr list frees its handle to pair again.
  for (_, paired) in &mut ext {
    if paired.is_some_and(|id| !wlr.iter().any(|(theirs, _)| *theirs == id)) {
      **paired = None;
    }
  }
  let mut taken: Vec<u64> = ext.iter().filter_map(|(_, paired)| **paired).collect();
  for (names, paired) in ext.iter_mut().filter(|(_, paired)| paired.is_none()) {
    let partner = wlr
      .iter()
      .find(|(id, theirs)| theirs == names && !taken.contains(id));
    if let Some(&(id, _)) = partner {
      **paired = Some(id);
      taken.push(id);
    }
  }
}

impl Platform {
  /// The ext handle of wlr toplevel `id`, to capture it. The first call
  /// binds `ext_foreign_toplevel_list_v1` and the toplevel capture source
  /// manager. `None` while no handle is paired with it.
  pub(crate) fn toplevel_handle(&mut self, id: u64) -> Option<ExtForeignToplevelHandleV1> {
    let ext = &mut self.captures.toplevels;
    if !std::mem::replace(&mut ext.asked, true) {
      ext.list = self.registry.bind_one(&self.qh, 1..=1, ()).ok();
      ext.sources = self.registry.bind_one(&self.qh, 1..=1, ()).ok();
      if ext.list.is_none() || ext.sources.is_none() {
        tracing::warn!(
          "the compositor lacks ext-foreign-toplevel-list or \
           ext-foreign-toplevel-image-capture-source, so views of windows show nothing"
        );
      }
    }
    let wlr: Vec<(u64, (&str, &str))> = self.foreign.names().collect();
    let ext_names = ext.handles.iter_mut().filter_map(|handle| {
      let (app_id, title) = handle.current.as_ref()?;
      Some(((app_id.as_str(), title.as_str()), &mut handle.paired))
    });
    pair(ext_names.collect(), &wlr);
    let handle = ext
      .handles
      .iter()
      .find(|handle| handle.paired == Some(id))?;
    Some(handle.proxy.clone())
  }
}

delegate_noop!(Platform: ExtForeignToplevelImageCaptureSourceManagerV1);

impl Dispatch<ExtForeignToplevelListV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    _: &ExtForeignToplevelListV1,
    event: ext_foreign_toplevel_list_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let ext = &mut platform.captures.toplevels;
    match event {
      ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
        if ext.handles.len() >= MAX_HANDLES {
          if !std::mem::replace(&mut ext.warned, true) {
            tracing::warn!("more than {MAX_HANDLES} windows; the rest can't be captured");
          }
          toplevel.destroy();
          return;
        }
        ext.handles.push(Handle {
          proxy: toplevel,
          pending: Default::default(),
          current: None,
          paired: None,
        });
      }
      ext_foreign_toplevel_list_v1::Event::Finished => {
        tracing::info!("the compositor stopped listing windows to capture");
        ext.handles.clear();
      }
      _ => {}
    }
  }

  event_created_child!(Platform, ExtForeignToplevelListV1, [
    ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
  ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ExtForeignToplevelHandleV1,
    event: ext_foreign_toplevel_handle_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    use ext_foreign_toplevel_handle_v1::Event;
    let handles = &mut platform.captures.toplevels.handles;
    // A handle past the cap was destroyed, and isn't here.
    let Some(index) = handles.iter().position(|ours| ours.proxy == *proxy) else {
      return;
    };
    let handle = &mut handles[index];
    match event {
      Event::AppId { app_id } => handle.pending.0 = cut(app_id),
      Event::Title { title } => handle.pending.1 = cut(title),
      Event::Done => handle.current = Some(handle.pending.clone()),
      Event::Closed => {
        handles.remove(index);
      }
      _ => {}
    }
  }
}
