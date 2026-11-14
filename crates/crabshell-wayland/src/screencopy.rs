//! What screencopy views show, captured through
//! `ext-image-copy-capture-v1`: outputs, with
//! `ext-output-image-capture-source-v1`.
//!
//! Each view a shown window holds gets one session for its source, and a
//! shared-memory buffer of the size the session asks for. A frame copies
//! the source into the buffer; on `ready` the pixels go to the view, and
//! a live view asks for the next frame, which the compositor answers only
//! once the source changes.

use std::time::Instant;

use crabshell_render::PixelFormat;
use crabshell_ui::{CaptureRequest, CaptureSource};
use smithay_client_toolkit::reexports::client::protocol::{wl_output::WlOutput, wl_shm};
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
};
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_protocols::ext::image_capture_source::v1::client::{
  ext_image_capture_source_v1::ExtImageCaptureSourceV1,
  ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
  ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
  ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
  ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::platform::Platform;
use crate::windows::WindowId;

/// The widest and tallest source captured, in pixels, as for an image
/// file.
const MAX_SIDE: u32 = 8192;

/// The most pixels a source captured may have, as for an image file.
const MAX_PIXELS: u64 = 24_000_000;

/// The shm formats a capture is read in, most wanted first: those with
/// alpha keep a window's see-through parts, and R, G, B, A in memory,
/// as headless Sway offers, needs no swapping.
const READABLE: [wl_shm::Format; 4] = [
  wl_shm::Format::Abgr8888,
  wl_shm::Format::Argb8888,
  wl_shm::Format::Xbgr8888,
  wl_shm::Format::Xrgb8888,
];

/// The views shown, and the protocols they need, bound when the first
/// one shows.
#[derive(Default)]
pub(crate) struct Captures {
  /// Whether a view showed, so the managers are bound once at most.
  asked: bool,
  manager: Option<ExtImageCopyCaptureManagerV1>,
  outputs: Option<ExtOutputImageCaptureSourceManagerV1>,
  views: Vec<View>,
}

/// A view a shown window holds, and its capture.
struct View {
  window: WindowId,
  request: CaptureRequest,
  /// What the source is now; `None` while it isn't there.
  target: Option<Target>,
  /// `None` while waiting for the target, and once a capture ended: a
  /// one-shot view's frame came, or the source stopped.
  session: Option<Session>,
}

/// A source the compositor knows.
#[derive(Clone, PartialEq)]
enum Target {
  Output(WlOutput),
}

/// A capture session and its buffer, destroyed when dropped.
struct Session {
  proxy: ExtImageCopyCaptureSessionV1,
  /// The constraints announced since the last `done`.
  size: Option<(u32, u32)>,
  formats: Vec<wl_shm::Format>,
  buffer: Option<CaptureBuffer>,
  /// The frame being captured, and when it was asked for.
  frame: Option<(ExtImageCopyCaptureFrameV1, Instant)>,
}

impl Drop for Session {
  fn drop(&mut self) {
    // Before the buffer, which drops after this.
    if let Some((frame, _)) = self.frame.take() {
      frame.destroy();
    }
    self.proxy.destroy();
  }
}

/// A buffer the compositor copies frames into, in a pool of its own.
struct CaptureBuffer {
  pool: SlotPool,
  buffer: Buffer,
  width: u32,
  height: u32,
  format: wl_shm::Format,
}

impl Captures {
  fn view(&mut self, proxy: &ExtImageCopyCaptureSessionV1) -> Option<&mut View> {
    self.views.iter_mut().find(|view| {
      view
        .session
        .as_ref()
        .is_some_and(|ours| ours.proxy == *proxy)
    })
  }

  /// A session for `target`, or `None` without the protocols.
  fn start(&self, target: &Target, cursors: bool, qh: &QueueHandle<Platform>) -> Option<Session> {
    let manager = self.manager.as_ref()?;
    let source = match target {
      Target::Output(output) => self.outputs.as_ref()?.create_source(output, qh, ()),
    };
    let options = if cursors {
      Options::PaintCursors
    } else {
      Options::empty()
    };
    let proxy = manager.create_session(&source, options, qh, ());
    // The session keeps what it captures.
    source.destroy();
    Some(Session {
      proxy,
      size: None,
      formats: Vec::new(),
      buffer: None,
      frame: None,
    })
  }
}

impl Session {
  /// Asks for the next frame into the buffer, damaged whole.
  fn capture(&mut self, qh: &QueueHandle<Platform>) {
    let Some(buffer) = &self.buffer else {
      return;
    };
    let frame = self.proxy.create_frame(qh, ());
    frame.attach_buffer(buffer.buffer.wl_buffer());
    // The sizes are checked against `MAX_SIDE`, so they fit an i32.
    frame.damage_buffer(0, 0, buffer.width as i32, buffer.height as i32);
    frame.capture();
    self.frame = Some((frame, Instant::now()));
  }

  /// Takes the constraints a `done` ends: a buffer of the size asked for,
  /// in a format this crate reads, and the first frame. `false`, logged,
  /// if the constraints can't be met.
  fn constrained(&mut self, shm: &Shm, qh: &QueueHandle<Platform>) -> bool {
    let formats = std::mem::take(&mut self.formats);
    let format = READABLE.into_iter().find(|format| formats.contains(format));
    let (Some((width, height)), Some(format)) = (self.size, format) else {
      tracing::warn!(
        ?formats,
        "a capture offers no 8-bit RGB buffers, so its view shows nothing"
      );
      return false;
    };
    if width == 0
      || height == 0
      || width > MAX_SIDE
      || height > MAX_SIDE
      || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
      tracing::warn!(width, height, "a capture's source is too large to show");
      return false;
    }
    // A frame on its way still writes the buffer. It comes, or fails for
    // the new constraints, which starts a new session.
    if self.frame.is_some() {
      return true;
    }
    let fits = self.buffer.as_ref().is_some_and(|buffer| {
      (buffer.width, buffer.height, buffer.format) == (width, height, format)
    });
    if !fits {
      self.buffer = None;
      match allocate(shm, width, height, format) {
        Some(buffer) => self.buffer = Some(buffer),
        None => return false,
      }
    }
    tracing::debug!(width, height, ?format, ?formats, "a capture's buffer");
    self.capture(qh);
    true
  }
}

/// A `width` by `height` buffer of `format` in a new pool.
fn allocate(shm: &Shm, width: u32, height: u32, format: wl_shm::Format) -> Option<CaptureBuffer> {
  let stride = width * 4;
  let mut pool = match SlotPool::new(stride as usize * height as usize, shm) {
    Ok(pool) => pool,
    Err(error) => {
      tracing::warn!(%error, "couldn't create a capture's shared-memory pool");
      return None;
    }
  };
  let sides = [width, height, stride].map(|side| side as i32);
  match pool.create_buffer(sides[0], sides[1], sides[2], format) {
    Ok((buffer, _)) => Some(CaptureBuffer {
      pool,
      buffer,
      width,
      height,
      format,
    }),
    Err(error) => {
      tracing::warn!(%error, "couldn't create a capture's buffer");
      None
    }
  }
}

impl Platform {
  /// How many captures run, one for each view shown whose source is
  /// there, until a one-shot view's frame comes or the source stops.
  pub fn capture_sessions(&self) -> usize {
    let views = self.captures.views.iter();
    views.filter(|view| view.session.is_some()).count()
  }

  /// Gives each view in a shown window a session for its source, and
  /// ends the sessions of views gone, moved to another source, or whose
  /// source went. The first view binds the protocols.
  pub(crate) fn sync_captures(&mut self) {
    let mut wanted = Vec::new();
    for entry in &self.windows.entries {
      let shown = entry.surface.as_ref();
      if shown.is_some_and(|surface| surface.frames.pacing.configured()) {
        let requests = entry.window.captures();
        wanted.extend(requests.into_iter().map(|request| (entry.id, request)));
      }
    }
    if wanted.is_empty() && self.captures.views.is_empty() {
      return;
    }
    if !std::mem::replace(&mut self.captures.asked, true) {
      self.bind_captures();
    }
    let captures = &mut self.captures;
    captures.views.retain(|view| {
      let id = view.request.id;
      wanted.iter().any(|(_, request)| request.id == id)
    });
    for (window, request) in wanted {
      let target = match &request.source {
        CaptureSource::Output(name) => self
          .known
          .iter()
          .find(|(_, info)| info.name == *name)
          .map(|(output, _)| Target::Output(output.clone())),
        _ => None,
      };
      let index = captures
        .views
        .iter()
        .position(|view| view.request.id == request.id);
      if let Some(view) = index.map(|index| &mut captures.views[index])
        && view.target == target
        && (
          &view.request.source,
          view.request.live,
          view.request.paint_cursors,
        ) == (&request.source, request.live, request.paint_cursors)
      {
        // Only the box may have changed, which the next frame reads.
        view.request.size = request.size;
        continue;
      }
      let session = target
        .as_ref()
        .and_then(|target| captures.start(target, request.paint_cursors, &self.qh));
      let view = View {
        window,
        request,
        target,
        session,
      };
      match index {
        Some(index) => captures.views[index] = view,
        None => captures.views.push(view),
      }
    }
  }

  fn bind_captures(&mut self) {
    let captures = &mut self.captures;
    captures.manager = self.registry.bind_one(&self.qh, 1..=1, ()).ok();
    captures.outputs = self.registry.bind_one(&self.qh, 1..=1, ()).ok();
    if captures.manager.is_none() || captures.outputs.is_none() {
      tracing::warn!(
        "the compositor lacks ext-image-copy-capture or ext-output-image-capture-source, so \
         screencopy views show nothing"
      );
    }
  }

  /// Hands the frame in `view`'s buffer to its window, scaled down to
  /// what the view needs at the largest output scale.
  fn deliver(&mut self, index: usize) {
    let scale = self
      .known
      .iter()
      .map(|(_, info)| info.scale)
      .max()
      .unwrap_or(1)
      .max(1) as f32;
    let view = &mut self.captures.views[index];
    let Some(buffer) = view
      .session
      .as_mut()
      .and_then(|session| session.buffer.as_mut())
    else {
      return;
    };
    let Some(entry) = self.windows.get(view.window) else {
      return;
    };
    let size = view.request.size;
    // `as` saturates, and a view is at most 16384 logical pixels a side.
    let cover = (
      (size.width * scale).ceil() as u32,
      (size.height * scale).ceil() as u32,
    );
    let (width, height) = (buffer.width, buffer.height);
    let order = match buffer.format {
      wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => PixelFormat::Argb8888,
      _ => PixelFormat::Abgr8888,
    };
    let opaque = matches!(
      buffer.format,
      wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888
    );
    let Some(pixels) = buffer.buffer.canvas(&mut buffer.pool) else {
      return;
    };
    match crabshell_render::capture_frame(pixels, width, height, order, opaque, cover) {
      Some(frame) => entry.window.show_capture(view.request.id, frame),
      None => tracing::warn!(width, height, "a captured frame doesn't fit its buffer"),
    }
  }
}

delegate_noop!(Platform: ExtImageCopyCaptureManagerV1);
delegate_noop!(Platform: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Platform: ExtImageCaptureSourceV1);

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ExtImageCopyCaptureSessionV1,
    event: ext_image_copy_capture_session_v1::Event,
    _: &(),
    _: &Connection,
    qh: &QueueHandle<Platform>,
  ) {
    use ext_image_copy_capture_session_v1::Event;
    let Some(view) = platform.captures.view(proxy) else {
      return;
    };
    let Some(session) = view.session.as_mut() else {
      return;
    };
    match event {
      Event::BufferSize { width, height } => session.size = Some((width, height)),
      Event::ShmFormat {
        format: WEnum::Value(format),
      } => session.formats.push(format),
      Event::Done => {
        if !session.constrained(&platform.shm, qh) {
          view.session = None;
        }
      }
      Event::Stopped => {
        tracing::debug!(source = ?view.request.source, "a capture's source stopped");
        view.session = None;
      }
      _ => {}
    }
  }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ExtImageCopyCaptureFrameV1,
    event: ext_image_copy_capture_frame_v1::Event,
    _: &(),
    _: &Connection,
    qh: &QueueHandle<Platform>,
  ) {
    use ext_image_copy_capture_frame_v1::Event;
    let captures = &mut platform.captures;
    let Some(index) = captures.views.iter().position(|view| {
      let session = view.session.as_ref();
      session.is_some_and(|session| {
        session
          .frame
          .as_ref()
          .is_some_and(|(ours, _)| ours == proxy)
      })
    }) else {
      return;
    };
    let view = &mut captures.views[index];
    let Some(session) = view.session.as_mut() else {
      return;
    };
    match event {
      Event::Ready => {
        if let Some((frame, asked)) = session.frame.take() {
          frame.destroy();
          tracing::trace!(elapsed = ?asked.elapsed(), "a captured frame came");
        }
        platform.deliver(index);
        let view = &mut platform.captures.views[index];
        match &mut view.session {
          Some(session) if view.request.live => session.capture(qh),
          _ => view.session = None,
        }
      }
      Event::Failed { reason } => {
        if let Some((frame, _)) = session.frame.take() {
          frame.destroy();
        }
        view.session = None;
        match reason {
          // The session's buffers changed: a new session asks again.
          WEnum::Value(FailureReason::BufferConstraints) => {
            let restart = view.target.clone();
            let cursors = view.request.paint_cursors;
            let session = restart.and_then(|target| captures.start(&target, cursors, qh));
            captures.views[index].session = session;
          }
          WEnum::Value(FailureReason::Stopped) => {}
          reason => tracing::warn!(?reason, "a capture failed, so its view shows no more"),
        }
      }
      _ => {}
    }
  }
}
