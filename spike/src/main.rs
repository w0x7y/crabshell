//! Crabshell Phase 0 spike: a static "Crabshell" bar on one wlr-layer-shell surface.
//!
//! It tests the core stack before any framework code exists: smithay-client-toolkit
//! on a calloop event loop, tiny-skia drawing straight into wl_shm memory,
//! cosmic-text with fonts resolved through fontconfig, and one zbus call polled
//! by calloop's own futures executor. See RESULTS.md for what it measured.

// The spike's own code needs no `unsafe`; this makes the compiler enforce that.
#![forbid(unsafe_code)]

mod bar;
mod text;

use std::{process::ExitCode, time::Instant};

use calloop::{
  EventLoop,
  signals::{Signal, Signals},
};
use cosmic_text::Metrics;
use smithay_client_toolkit::reexports::{
  calloop_wayland_source::WaylandSource,
  client::{Connection, globals::registry_queue_init},
};
use tracing::{error, info};

use crate::{bar::Bar, text::Text};

/// Font size in logical pixels. The line height equals the bar height, and
/// cosmic-text centres the glyphs inside the line, so the text is vertically centred.
const FONT_SIZE: f32 = 15.0;

fn main() -> ExitCode {
  let started = Instant::now();
  tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .init();

  // Set up signal handling before anything starts a thread. calloop reads
  // SIGINT/SIGTERM from a signalfd, which only works while those signals are
  // blocked in every thread, and new threads copy the blocked set of the
  // thread that starts them. zbus's runtime starts a helper thread later.
  let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])
    .expect("failed to set up SIGINT/SIGTERM handling");

  let text = Text::new(
    "sans-serif",
    "Crabshell",
    Metrics::new(FONT_SIZE, bar::HEIGHT as f32),
  );
  info!(
    "fonts ready {:.2} ms after main() started",
    started.elapsed().as_secs_f64() * 1000.0
  );

  // `'static` because the loop owns its callbacks for the whole program.
  let mut event_loop: EventLoop<'static, Bar> =
    EventLoop::try_new().expect("failed to create the calloop event loop");
  let handle = event_loop.handle();

  let connection = Connection::connect_to_env()
    .expect("failed to connect to the Wayland compositor (is WAYLAND_DISPLAY set?)");
  let (globals, queue) =
    registry_queue_init::<Bar>(&connection).expect("failed to read the Wayland registry");
  let mut bar = Bar::new(
    &globals,
    &queue.handle(),
    text,
    handle.clone(),
    event_loop.get_signal(),
    started,
  );

  WaylandSource::new(connection, queue)
    .insert(handle.clone())
    .expect("failed to add the Wayland connection to the event loop");

  handle
    .insert_source(signals, |event, _, bar| {
      info!(signal = ?event.signal(), "received signal, exiting");
      bar.exit();
    })
    .expect("failed to add the signal source to the event loop");

  // One zbus call as a future on calloop's executor. The executor polls it on
  // this thread; its result arrives in the callback below, also on this thread.
  let (executor, scheduler) =
    calloop::futures::executor().expect("failed to create the calloop futures executor");
  let dbus_started = Instant::now();
  handle
    .insert_source(executor, move |result, _, bar| match result {
      Ok((dbus, name_count)) => {
        info!(
          names = name_count,
          "D-Bus ListNames returned on calloop's executor after {:.2} ms",
          dbus_started.elapsed().as_secs_f64() * 1000.0
        );
        bar.keep_dbus(dbus);
      }
      Err(err) => error!(%err, "D-Bus ListNames on calloop's executor failed"),
    })
    .expect("failed to add the futures executor to the event loop");
  scheduler
    .schedule(list_names())
    .expect("the executor was dropped before the D-Bus call was scheduled");

  // `None` = no timeout: the thread sleeps in epoll until a source is ready.
  // An error here comes from outside (e.g. the compositor went away), so log it
  // and exit with a failure code instead of panicking.
  match event_loop.run(None, &mut bar, |_| {}) {
    Ok(()) => {
      info!("event loop stopped, exiting cleanly");
      ExitCode::SUCCESS
    }
    Err(err) => {
      error!(%err, "event loop failed, exiting");
      ExitCode::FAILURE
    }
  }
}

/// Connects to the session bus and calls org.freedesktop.DBus.ListNames.
///
/// Returns the connection too, so the caller can keep it open. `async fn` builds
/// a future: nothing runs until an executor polls it. zbus's own runtime
/// (zruntime) watches the socket on a helper thread and wakes the future when
/// the reply arrives; calloop's executor then polls it again on the main thread.
async fn list_names() -> zbus::Result<(zbus::Connection, usize)> {
  let connection = zbus::Connection::session().await?;
  let names = zbus::fdo::DBusProxy::new(&connection)
    .await?
    .list_names()
    .await?;
  Ok((connection, names.len()))
}
