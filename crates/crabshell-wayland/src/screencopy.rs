//! What screencopy views show, captured through
//! `ext-image-copy-capture-v1`: outputs, with
//! `ext-output-image-capture-source-v1`, and windows, with
//! `ext-foreign-toplevel-image-capture-source-v1`.
//!
//! The views of one source and cursor mode in the shown windows share
//! one session, with a shared-memory buffer of the size the session asks
//! for. A frame copies the source into the buffer; on `ready` the pixels
//! are cut once for the largest view and go to each, and while one of
//! them is live the session asks for the next frame, which the
//! compositor answers only once the source changes.

use std::time::Instant;

use crabshell_render::{PixelFormat, Turn};
use crabshell_ui::{CaptureFrame, CaptureRequest, CaptureSource};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
};
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::ext::image_capture_source::v1::client::{
  ext_image_capture_source_v1::ExtImageCaptureSourceV1,
  ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
  ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
  ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
  ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::ext_toplevels::ExtToplevels;
use crate::outputs::turn;
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

/// The views shown, the sessions they share, and the protocols they
/// need, bound when the first view shows.
#[derive(Default)]
pub(crate) struct Captures {
  /// Whether a view showed, so the managers are bound once at most.
  asked: bool,
  manager: Option<ExtImageCopyCaptureManagerV1>,
  outputs: Option<ExtOutputImageCaptureSourceManagerV1>,
  /// Windows to capture, once a view of one asks.
  pub(crate) toplevels: ExtToplevels,
  views: Vec<View>,
  /// One for each source and cursor mode a view still captures.
  sessions: Vec<Session>,
}

/// A view a shown window holds.
struct View {
  window: WindowId,
  request: CaptureRequest,
  /// What the source is now; `None` while it isn't there.
  target: Option<Target>,
  /// Whether its capture ended: a one-shot view's frame came, or the
  /// source stopped. It takes no session again until its source changes
  /// or its box outgrows its frame.
  ended: bool,
  /// The box the last frame it got was cut for, in buffer pixels, and
  /// whether that frame was the source's whole size, which a larger box
  /// can't improve on.
  cover: (u32, u32),
  whole: bool,
}

impl View {
  /// Whether `session` captures for this view.
  fn takes(&self, session: &Session) -> bool {
    !self.ended
      && self.target.as_ref() == Some(&session.target)
      && self.request.paint_cursors == session.cursors
  }
}

/// A source the compositor knows.
#[derive(Clone, PartialEq)]
enum Target {
  Output(WlOutput),
  Toplevel(ExtForeignToplevelHandleV1),
}

/// A capture session and its buffer, destroyed when dropped.
struct Session {
  target: Target,
  cursors: bool,
  proxy: ExtImageCopyCaptureSessionV1,
  /// The constraints announced since the last `done`.
  size: Option<(u32, u32)>,
  formats: Vec<wl_shm::Format>,
  buffer: Option<CaptureBuffer>,
  /// The frame being captured, and when it was asked for.
  frame: Option<(ExtImageCopyCaptureFrameV1, Instant)>,
  /// How the compositor turned the frame being captured.
  turn: Turn,
  /// The windows that showed the last frame. A live session's next frame
  /// waits for one of them to draw it, so frames come no faster than they
  /// show. Empty while none waits.
  waiting: Vec<WindowId>,
  /// Whether a failure was tried again since the last frame.
  retried: bool,
  /// The last frame, the box it was cut for and whether it was whole, for
  /// a view that joins later.
  last: Option<(CaptureFrame, (u32, u32), bool)>,
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
  fn session(&self, proxy: &ExtImageCopyCaptureSessionV1) -> Option<usize> {
    self.sessions.iter().position(|ours| ours.proxy == *proxy)
  }

  /// Ends session `index`, and the capture of each view it took.
  fn end(&mut self, index: usize) {
    let session = self.sessions.remove(index);
    for view in &mut self.views {
      if view.takes(&session) {
        view.ended = true;
      }
    }
  }

  /// A session for `target`, or `None` without the protocols.
  fn start(&self, target: &Target, cursors: bool, qh: &QueueHandle<Platform>) -> Option<Session> {
    let manager = self.manager.as_ref()?;
    let source = match target {
      Target::Output(output) => self.outputs.as_ref()?.create_source(output, qh, ()),
      Target::Toplevel(handle) => {
        let sources = self.toplevels.sources.as_ref()?;
        sources.create_source(handle, qh, ())
      }
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
      target: target.clone(),
      cursors,
      proxy,
      size: None,
      formats: Vec::new(),
      buffer: None,
      frame: None,
      turn: Turn::default(),
      waiting: Vec::new(),
      retried: false,
      last: None,
    })
  }
}

impl Session {
  /// Asks for the next frame into the buffer, damaged whole.
  fn capture(&mut self, qh: &QueueHandle<Platform>) {
    let Some(buffer) = &self.buffer else {
      return;
    };
    self.waiting.clear();
    self.turn = Turn::default();
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
    // One `wl_shm` takes too, or making the buffer would end the
    // connection.
    let format = READABLE
      .into_iter()
      .find(|format| formats.contains(format) && shm.formats().contains(format));
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
  /// How many capture sessions run: one for each source and cursor mode
  /// that a view shown still captures, shared by every view of it, until
  /// the one-shot views' frame comes or the source stops.
  pub fn capture_sessions(&self) -> usize {
    self.captures.sessions.len()
  }

  /// Gives each view in a shown window a session for its source, shared
  /// with the other views of it, or the frame a session it joins already
  /// has, and ends the sessions no view takes any more. The first view
  /// binds the protocols. Returns whether a window shows a frame for it.
  pub(crate) fn sync_captures(&mut self) -> bool {
    let mut wanted = Vec::new();
    for entry in &self.windows.entries {
      let shown = entry.surface.as_ref();
      if shown.is_some_and(|surface| surface.frames.pacing.configured()) {
        let requests = entry.window.captures();
        wanted.extend(requests.into_iter().map(|request| (entry.id, request)));
      }
    }
    if wanted.is_empty() && self.captures.views.is_empty() {
      return false;
    }
    if !std::mem::replace(&mut self.captures.asked, true) {
      self.bind_captures();
    }
    let targets: Vec<Option<Target>> = wanted
      .iter()
      .map(|(_, request)| self.target(&request.source))
      .collect();
    let scale = self.largest_scale();
    let captures = &mut self.captures;
    let mut gone = Vec::new();
    captures.views.retain(|view| {
      let id = view.request.id;
      let kept = wanted.iter().any(|(_, request)| request.id == id);
      if !kept {
        gone.push(id);
      }
      kept
    });
    if !gone.is_empty() {
      self.renderer.forget_captures(&gone);
    }
    // Sessions to start again, for a view whose box outgrew their frame.
    let mut again: Vec<(Target, bool)> = Vec::new();
    for ((window, request), target) in wanted.into_iter().zip(targets) {
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
        // Only the box may have changed, which the next frame reads,
        // unless it outgrew the frame shown: a one-shot view, or a live
        // one of a still source, gets no next frame, so it asks again.
        view.request.size = request.size;
        let cover = cover(request.size, scale);
        // Not before a first frame, whose box is none yet.
        let delivered = view.cover != (0, 0);
        let grew = cover.0 > view.cover.0 || cover.1 > view.cover.1;
        if delivered
          && grew
          && !view.whole
          && let Some(target) = &view.target
        {
          // Asked once: it counts as having no frame until the next.
          (view.ended, view.cover) = (false, (0, 0));
          again.push((target.clone(), view.request.paint_cursors));
        }
        continue;
      }
      let view = View {
        window,
        request,
        target,
        ended: false,
        cover: (0, 0),
        whole: false,
      };
      match index {
        Some(index) => captures.views[index] = view,
        None => captures.views.push(view),
      }
    }
    // A view new to a session that has a frame shows it now, if it is
    // large enough, since a live session of a still source sends no
    // other; one too small starts the session again.
    let mut shown = false;
    for view in &mut captures.views {
      if view.cover != (0, 0) {
        continue;
      }
      let Some(session) = captures.sessions.iter().find(|session| view.takes(session)) else {
        continue;
      };
      let Some((frame, cut, whole)) = &session.last else {
        continue;
      };
      let own = cover(view.request.size, scale);
      if !*whole && (own.0 > cut.0 || own.1 > cut.1) {
        again.push((session.target.clone(), session.cursors));
        continue;
      }
      if let Some(entry) = self.windows.get(view.window) {
        shown |= entry.window.show_capture(view.request.id, frame.clone());
      }
      (view.cover, view.whole) = (*cut, *whole);
      view.ended = !view.request.live;
    }
    let views = &captures.views;
    captures.sessions.retain_mut(|session| {
      let key = (session.target.clone(), session.cursors);
      let kept = !again.contains(&key) && views.iter().any(|view| view.takes(session));
      // A window the next frame waited for went: it waits no more.
      let waited = !session.waiting.is_empty();
      session
        .waiting
        .retain(|window| views.iter().any(|view| view.window == *window));
      if kept && waited && session.waiting.is_empty() {
        session.capture(&self.qh);
      }
      kept
    });
    for index in 0..captures.views.len() {
      let view = &captures.views[index];
      let taken = captures.sessions.iter().any(|session| view.takes(session));
      if view.ended || taken {
        continue;
      }
      let Some(target) = &view.target else {
        continue;
      };
      if let Some(session) = captures.start(target, view.request.paint_cursors, &self.qh) {
        captures.sessions.push(session);
      }
    }
    shown
  }

  /// What `source` is now, if it is there.
  fn target(&mut self, source: &CaptureSource) -> Option<Target> {
    match source {
      CaptureSource::Output(name) => {
        let (output, _) = self.known.iter().find(|(_, info)| info.name == *name)?;
        Some(Target::Output(output.clone()))
      }
      CaptureSource::Toplevel(id) => self.toplevel_handle(*id).map(Target::Toplevel),
      _ => None,
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

  /// The largest scale of any output, at which views are cut.
  fn largest_scale(&self) -> f32 {
    let scales = self.known.iter().map(|(_, info)| info.scale);
    scales.max().unwrap_or(1).max(1) as f32
  }

  /// Asks for the next frame of each live session whose frame a window
  /// in `drawn` just drew.
  pub(crate) fn paced_captures(&mut self, drawn: &[WindowId]) {
    for session in &mut self.captures.sessions {
      if session.waiting.iter().any(|window| drawn.contains(window)) {
        session.capture(&self.qh);
      }
    }
  }

  /// Hands the frame in session `index`'s buffer to each view it takes,
  /// cut once to what the largest of them needs at the largest output
  /// scale, and keeps it for views that join later. Returns the windows
  /// that repaint for it.
  fn deliver(&mut self, index: usize) -> Vec<WindowId> {
    let scale = self.largest_scale();
    let Captures {
      views, sessions, ..
    } = &mut self.captures;
    let session = &mut sessions[index];
    let cover = views
      .iter()
      .filter(|view| view.takes(session))
      .map(|view| cover(view.request.size, scale))
      .fold((0, 0), |most, own| (most.0.max(own.0), most.1.max(own.1)));
    let turn = session.turn;
    let Some(buffer) = session.buffer.as_mut() else {
      return Vec::new();
    };
    let (width, height) = (buffer.width, buffer.height);
    let order = match buffer.format {
      wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => PixelFormat::Argb8888,
      _ => PixelFormat::Abgr8888,
    };
    let opaque = matches!(
      buffer.format,
      wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888
    );
    if cover == (0, 0) {
      return Vec::new();
    }
    let Some(pixels) = buffer.buffer.canvas(&mut buffer.pool) else {
      return Vec::new();
    };
    let frame = crabshell_render::capture_frame(pixels, width, height, order, opaque, turn, cover);
    let Some(frame) = frame else {
      tracing::warn!(width, height, "a captured frame doesn't fit its buffer");
      return Vec::new();
    };
    let (kept, whole) = ((frame.width, frame.height), (width, height));
    let whole = kept == whole || kept == (whole.1, whole.0);
    let mut shown = Vec::new();
    for view in views.iter_mut().filter(|view| view.takes(session)) {
      (view.cover, view.whole) = (cover, whole);
      let window = self.windows.get(view.window);
      if window.is_some_and(|entry| entry.window.show_capture(view.request.id, frame.clone()))
        && !shown.contains(&view.window)
      {
        shown.push(view.window);
      }
    }
    session.last = Some((frame, cover, whole));
    shown
  }
}

/// A view's box in buffer pixels at `scale`.
fn cover(size: crabshell_ui::Size, scale: f32) -> (u32, u32) {
  // `as` saturates, and a view is at most 16384 logical pixels a side.
  (
    (size.width * scale).ceil() as u32,
    (size.height * scale).ceil() as u32,
  )
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
    let captures = &mut platform.captures;
    let Some(index) = captures.session(proxy) else {
      return;
    };
    let session = &mut captures.sessions[index];
    match event {
      Event::BufferSize { width, height } => session.size = Some((width, height)),
      Event::ShmFormat {
        format: WEnum::Value(format),
      } => session.formats.push(format),
      Event::Done => {
        if !session.constrained(&platform.shm, qh) {
          captures.end(index);
        }
      }
      Event::Stopped => {
        tracing::debug!("a capture's source stopped");
        captures.end(index);
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
    let Some(index) = captures.sessions.iter().position(|session| {
      let frame = session.frame.as_ref();
      frame.is_some_and(|(ours, _)| ours == proxy)
    }) else {
      return;
    };
    let session = &mut captures.sessions[index];
    match event {
      Event::Transform {
        transform: WEnum::Value(transform),
      } => session.turn = turn(transform),
      Event::Ready => {
        if let Some((frame, asked)) = session.frame.take() {
          frame.destroy();
          tracing::trace!(elapsed = ?asked.elapsed(), "a captured frame came");
        }
        session.retried = false;
        let shown = platform.deliver(index);
        let Captures {
          views, sessions, ..
        } = &mut platform.captures;
        let session = &mut sessions[index];
        let live = views
          .iter()
          .any(|view| view.takes(session) && view.request.live);
        // A one-shot view had its frame.
        for view in views.iter_mut() {
          if view.takes(session) && !view.request.live {
            view.ended = true;
          }
        }
        if !live {
          sessions.remove(index);
        } else if shown.is_empty() {
          // One the same as before draws nothing, so it asks at once,
          // which the compositor answers only when the source changes.
          session.capture(qh);
        } else {
          session.waiting = shown;
        }
      }
      Event::Failed { reason } => {
        if let Some((frame, _)) = session.frame.take() {
          frame.destroy();
        }
        let retried = std::mem::replace(&mut session.retried, true);
        let again = match reason {
          // The session's buffers changed: a new session asks again.
          WEnum::Value(FailureReason::BufferConstraints) => true,
          WEnum::Value(FailureReason::Stopped) => false,
          // The protocol lets a client try again; once, so a source that
          // always fails doesn't keep the shell busy.
          reason if !retried => {
            tracing::info!(?reason, "a capture failed; trying again once");
            true
          }
          reason => {
            tracing::warn!(?reason, "a capture failed again, so its views show no more");
            false
          }
        };
        let (target, cursors) = (session.target.clone(), session.cursors);
        let restarted = again
          .then(|| captures.start(&target, cursors, qh))
          .flatten();
        match restarted {
          Some(mut session) => {
            session.retried = retried || reason != WEnum::Value(FailureReason::BufferConstraints);
            session.last = captures.sessions[index].last.take();
            captures.sessions[index] = session;
          }
          None => captures.end(index),
        }
      }
      _ => {}
    }
  }
}
