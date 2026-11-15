//! Peak meters: how loud a sink, a source or an app's stream is now. Each
//! meter is a record stream the server fills with one peak per channel
//! [`RATE`] times a second, open only while a config holds the meter.

use std::panic::{self, AssertUnwindSafe, Location};
use std::rc::Rc;
use std::sync::Arc;

use calloop::channel::Sender;
use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::{IntoProp, Prop};
use libpulse_binding::channelmap::Map;
use libpulse_binding::context::{self, Context};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{FlagSet, PeekResult, State, Stream};

use super::{Audio, AudioService, Event, Locked, Message, Nodes, Pulse, on_pa_thread};
use crate::dbus::set_changed;
use crate::lines::panic_message;

/// Peaks a second, as pavucontrol reads them: smooth on a level bar, and
/// each one wakes the shell, so no more than that.
const RATE: u32 = 25;

/// What a peak meter listens to, for [`Audio::peaks`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeakTarget {
  /// The sink called this, through its monitor: what it plays.
  Sink(String),
  /// The source called this, such as a microphone or a sink's monitor.
  Source(String),
  /// The stream numbered this: what that app plays.
  Stream(u32),
}

/// A target that stays.
impl IntoProp<Option<PeakTarget>> for PeakTarget {
  fn into_prop(self) -> Prop<Option<PeakTarget>> {
    // A closure that reads no signal runs once.
    (move || Some(self.clone())).into_prop()
  }
}

impl Audio {
  /// A peak meter, as Quickshell's `PwNodePeakMonitor`: each channel's
  /// loudest sample over the last 1/25 s, from 0 to 1, in the order of
  /// the target's `channels`. The loudest of them, Quickshell's `peak`, is
  /// `peaks.with(|p| p.iter().copied().fold(0.0, f32::max))`. Empty
  /// before the first reading, while there is no target and once it goes;
  /// all 0 while a sink plays nothing or a stream is paused.
  ///
  /// `target` takes a [`PeakTarget`] or a closure returning an `Option`
  /// of one: the meter moves when a signal the closure read changes, and
  /// `None` stops it, as Quickshell's `enabled`. A panic in the closure is
  /// logged and stops the meter until the next change.
  ///
  /// This is the one audio reading that wakes the shell while nothing
  /// changes on screen: while the meter records, libpulse's thread and the
  /// shell wake 25 times a second. The signal is set only when a peak
  /// changes, so silence repaints nothing. A sink's or a stream's meter
  /// records only while something plays, and never keeps a device awake;
  /// a source's records all the time, as any recording of a microphone
  /// does. Nothing records until a config asks for a meter, and it stops
  /// when the context that asked goes.
  ///
  /// On PipeWire a sink's peaks come before its volume; a source's and a
  /// stream's come after their own volume, as a recording of them would.
  #[track_caller]
  pub fn peaks(&self, target: impl IntoProp<Option<PeakTarget>>) -> ReadSignal<Vec<f32>> {
    let at = Location::caller();
    let Some(service) = self.service.clone().filter(|_| !self.owner.is_disposed()) else {
      return self.no_peaks;
    };
    let peaks = self.owner.signal(Vec::new());
    let target = target.into_prop().shared();
    // A memo, so a closure that reads a sink's volume moves the meter only
    // when the target itself changes.
    let target = self.owner.memo(move || {
      let chosen = panic::catch_unwind(AssertUnwindSafe(|| target()));
      chosen.unwrap_or_else(|payload| {
        let message = panic_message(payload.as_ref());
        tracing::error!(
          "cx.audio().peaks (called at {at}) panicked choosing its target: {message}"
        );
        None
      })
    });
    // Each run's scope holds the meter it opened, so a new target or the
    // owner going closes it.
    self.owner.effect_scoped(move |scope| {
      if let Some(target) = target.get() {
        let id = service.meter(target, peaks);
        scope.signal(Unmeter(Rc::clone(&service), id));
      }
    });
    peaks.read_only()
  }
}

/// A meter a config holds: what it listens to, and where its peaks go.
pub(super) struct Wanted {
  id: u64,
  target: PeakTarget,
  pub(super) peaks: Signal<Vec<f32>>,
}

/// Closes a meter when the scope that opened it drops this.
struct Unmeter(Rc<AudioService>, u64);

impl Drop for Unmeter {
  fn drop(&mut self) {
    self.0.unmeter(self.1);
  }
}

impl AudioService {
  /// Adds a meter for `target`, recording at once if the server has it.
  fn meter(&self, target: PeakTarget, peaks: Signal<Vec<f32>>) -> u64 {
    let id = self.next_meter.get();
    self.next_meter.set(id + 1);
    self.meters.borrow_mut().push(Wanted { id, target, peaks });
    self.sync_meters();
    id
  }

  fn unmeter(&self, id: u64) {
    self.meters.borrow_mut().retain(|meter| meter.id != id);
    self.sync_meters();
  }

  /// Opens and closes record streams to match the meters. Untracked: a
  /// meter's effect calls this, and must not come to depend on the peaks
  /// it sets.
  fn sync_meters(&self) {
    untrack(|| {
      let wanted = self.meters.borrow();
      if let Some(pulse) = self.pulse.borrow_mut().as_mut() {
        pulse.sync_meters(&wanted);
      }
    });
  }
}

/// Where a meter records from.
#[derive(Clone, PartialEq)]
pub(super) struct Source {
  /// The source's name, a sink's monitor for a sink or a stream.
  name: String,
  map: Map,
  /// The stream to keep to, of those its sink plays.
  stream: Option<u32>,
  /// Records only while something else keeps the sink playing, so a
  /// meter alone never wakes a device. A source's meter isn't: a
  /// microphone's level shows with nothing else recording.
  passive: bool,
}

impl Nodes {
  /// Where a meter for `target` records from, as last read, and whether
  /// it is quiet: its sink idle or its stream paused, which sends nothing,
  /// so the last peaks would stay. `None` while the server doesn't have
  /// the target.
  fn source_of(&self, target: &PeakTarget) -> Option<(Source, bool)> {
    let (name, map, stream, quiet) = match target {
      PeakTarget::Sink(name) => {
        let sink = self.sinks.iter().find(|sink| sink.name == *name)?;
        (sink.monitor_source.clone()?, sink.map, None, !sink.running)
      }
      PeakTarget::Source(name) => {
        let source = self.sources.iter().find(|source| source.name == *name)?;
        (source.name.clone(), source.map, None, false)
      }
      PeakTarget::Stream(index) => {
        let stream = self.streams.iter().find(|stream| stream.index == *index)?;
        let sink = self.sinks.iter().find(|sink| sink.index == stream.sink)?;
        let quiet = stream.corked || !sink.running;
        let name = sink.monitor_source.clone()?;
        (name, stream.map, Some(*index), quiet)
      }
    };
    let passive = !matches!(target, PeakTarget::Source(_));
    let source = Source {
      name,
      map,
      stream,
      passive,
    };
    Some((source, quiet))
  }
}

/// A meter's record stream.
pub(super) struct Meter {
  id: u64,
  peaks: Signal<Vec<f32>>,
  source: Source,
  /// `None` once the server ended it, or if it didn't open. Either way it
  /// isn't tried again until its source changes.
  stream: Option<Stream>,
  /// While quiet, its peaks are 0 and what it reads is dropped.
  quiet: bool,
}

impl Meter {
  /// Closes the stream, under the mainloop's lock or with libpulse's
  /// thread stopped.
  fn stop(&mut self) {
    if let Some(mut stream) = self.stream.take() {
      // Cleared first: libpulse calls them again as the stream ends.
      stream.set_read_callback(None);
      stream.set_state_callback(None);
    }
  }
}

impl Drop for Meter {
  fn drop(&mut self) {
    self.stop();
  }
}

impl Pulse {
  /// Closes each meter no longer wanted or whose source changed, and
  /// opens each wanted one the server has.
  pub(super) fn sync_meters(&mut self, wanted: &[Wanted]) {
    let next: Vec<_> = wanted
      .iter()
      .map(|meter| (meter.id, meter.peaks, self.nodes.source_of(&meter.target)))
      .collect();
    let _lock = Locked::new(&mut self.mainloop);
    self.meters.retain(|meter| {
      let same = |(id, _, found): &(u64, _, Option<(Source, bool)>)| {
        *id == meter.id
          && found
            .as_ref()
            .is_some_and(|(source, _)| *source == meter.source)
      };
      let kept = next.iter().any(same);
      if !kept {
        set_changed(meter.peaks, Vec::new());
      }
      kept
    });
    let mut context = self.context.as_mut();
    let ready = context
      .as_ref()
      .is_some_and(|c| c.get_state() == context::State::Ready);
    for (id, peaks, found) in next {
      let Some((source, quiet)) = found else {
        continue;
      };
      let meter = match self.meters.iter_mut().find(|meter| meter.id == id) {
        Some(meter) => meter,
        None => match context.as_mut().filter(|_| ready) {
          Some(context) => {
            let stream = open(context, &source, id, &self.sender, self.generation);
            self.meters.push(Meter {
              id,
              peaks,
              source,
              stream,
              quiet,
            });
            self.meters.last_mut().expect("the meter just pushed")
          }
          None => continue,
        },
      };
      meter.quiet = quiet;
      if quiet && meter.stream.is_some() {
        let channels = usize::from(meter.source.map.len());
        set_changed(meter.peaks, vec![0.0; channels]);
      }
    }
  }

  /// Shows the peaks meter `id`'s stream has read since the last time:
  /// for each channel, the loudest.
  pub(super) fn read_peaks(&mut self, id: u64) {
    let _lock = Locked::new(&mut self.mainloop);
    let Some(meter) = self.meters.iter_mut().find(|meter| meter.id == id) else {
      return;
    };
    let channels = usize::from(meter.source.map.len());
    let Some(stream) = meter.stream.as_mut() else {
      return;
    };
    let mut peaks = None;
    loop {
      match stream.peek() {
        Ok(PeekResult::Data(bytes)) => louder(peaks.get_or_insert(vec![0.0; channels]), bytes),
        Ok(PeekResult::Hole(_)) => {}
        Ok(PeekResult::Empty) | Err(_) => break,
      }
      if stream.discard().is_err() {
        break;
      }
    }
    // Quiet, a late reading from before would stay.
    if let Some(peaks) = peaks.filter(|_| !meter.quiet) {
      set_changed(meter.peaks, peaks);
    }
  }

  /// Closes meter `id` if the server ended its stream, as when its source
  /// goes.
  pub(super) fn on_meter_state(&mut self, id: u64) {
    let _lock = Locked::new(&mut self.mainloop);
    let Some(meter) = self.meters.iter_mut().find(|meter| meter.id == id) else {
      return;
    };
    let state = meter.stream.as_ref().map(Stream::get_state);
    if matches!(state, Some(State::Failed | State::Terminated)) {
      let source = &meter.source.name;
      tracing::info!(source, "the audio server ended a peak meter");
      meter.stop();
      set_changed(meter.peaks, Vec::new());
    }
  }
}

/// Closes every meter as the connection goes, under the mainloop's lock.
/// Each opens again once the next connection's lists have its source.
pub(super) fn close_all(meters: &mut Vec<Meter>) {
  for meter in meters.drain(..) {
    set_changed(meter.peaks, Vec::new());
  }
}

/// Starts recording peaks from `source`, under the mainloop's lock.
/// `None`, logged, if the server or libpulse refused.
fn open(
  context: &mut Context,
  source: &Source,
  id: u64,
  sender: &Arc<Sender<Message>>,
  generation: u32,
) -> Option<Stream> {
  let spec = Spec {
    format: Format::FLOAT32NE,
    rate: RATE,
    channels: source.map.len(),
  };
  let name = &source.name;
  let Some(mut stream) = Stream::new(context, "Peak meter", &spec, Some(&source.map)) else {
    tracing::warn!(name, "libpulse made no stream for a peak meter");
    return None;
  };
  if let Some(index) = source.stream
    && let Err(error) = stream.set_monitor_stream(index)
  {
    tracing::warn!(name, index, %error, "a peak meter couldn't keep to its stream");
    return None;
  }
  // One frame a fragment, so each peak arrives as it is measured.
  let frame = u32::from(spec.channels) * 4;
  let attr = BufferAttr {
    maxlength: u32::MAX,
    tlength: u32::MAX,
    prebuf: u32::MAX,
    minreq: u32::MAX,
    fragsize: frame,
  };
  let mut flags = FlagSet::PEAK_DETECT | FlagSet::ADJUST_LATENCY | FlagSet::DONT_MOVE;
  if source.passive {
    flags |= FlagSet::DONT_INHIBIT_AUTO_SUSPEND;
  }
  if let Err(error) = stream.connect_record(Some(name), Some(&attr), flags) {
    tracing::warn!(name, %error, "the audio server refused a peak meter");
    return None;
  }
  // Set under the lock, so neither runs before the stream is kept.
  let read = Arc::clone(sender);
  let on_read = on_pa_thread(move |_| {
    let _ = read.send((generation, Event::Peaks(id)));
  });
  stream.set_read_callback(Some(Box::new(on_read)));
  let state = Arc::clone(sender);
  let on_state = on_pa_thread(move || {
    let _ = state.send((generation, Event::MeterState(id)));
  });
  stream.set_state_callback(Some(Box::new(on_state)));
  Some(stream)
}

/// Raises each channel's peak to the loudest sample of the frames of
/// 32-bit floats in `bytes`, at most 1. A sample that isn't a number
/// counts as silence.
pub(super) fn louder(peaks: &mut [f32], bytes: &[u8]) {
  if peaks.is_empty() {
    return;
  }
  for frame in bytes.chunks_exact(4 * peaks.len()) {
    for (peak, sample) in peaks.iter_mut().zip(frame.as_chunks::<4>().0) {
      // `max` passes over NaN.
      *peak = peak.max(f32::from_ne_bytes(*sample).abs()).min(1.0);
    }
  }
}
