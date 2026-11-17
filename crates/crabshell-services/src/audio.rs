//! Audio: the default sink's volume and mute, and every sink, source and
//! app stream with theirs, from the PulseAudio server, which
//! pipewire-pulse serves on PipeWire systems.
//!
//! libpulse runs its mainloop on a thread of its own. Every libpulse call
//! from this side runs on the main thread under the mainloop's lock.
//! libpulse's callbacks run on its thread and only send a message over a
//! calloop channel, so the signal is set on the main thread like any other.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use calloop::channel::Sender;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{Owner, ReadSignal, Signal};
use crabshell_ui::Cx;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::channelmap::{Map, Position};
use libpulse_binding::context::introspect::{
  Introspector, ServerInfo, SinkInfo, SinkInputInfo, SourceInfo,
};
use libpulse_binding::context::subscribe::{Facility, InterestMaskSet};
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::def::SinkState;
use libpulse_binding::mainloop::threaded::Mainloop;
use libpulse_binding::volume::{ChannelVolumes, Volume};

use crate::Shared;
use crate::dbus::set_changed;

mod peaks;
mod sound;
#[cfg(test)]
mod tests;

pub use peaks::PeakTarget;

/// How long the service waits before each reconnect.
pub(crate) const RETRY: Duration = Duration::from_secs(2);

/// How long the service keeps trying, from startup and from each drop of
/// a working connection.
pub(crate) const RETRY_FOR: Duration = Duration::from_secs(60);

/// How long a command's result stands in for the server's reads.
pub(crate) const SETTLE: Duration = Duration::from_millis(250);

/// The most sinks, sources or streams kept, each.
const MAX_NODES: usize = 128;

/// Where the audio server is.
pub(crate) struct Server {
  /// In libpulse's server syntax. `None` for libpulse's default, which
  /// `PULSE_SERVER` and client.conf can set.
  pub(crate) address: Option<String>,
  /// How long to wait before each reconnect: [`RETRY`], less in tests.
  pub(crate) retry: Duration,
  /// How long to keep trying: [`RETRY_FOR`], less in tests.
  pub(crate) retry_for: Duration,
}

/// The default sink: where sound goes unless an app picks another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sink {
  /// Its name on the server, such as
  /// `alsa_output.pci-0000_03_00.6.analog-stereo`.
  pub name: String,
  /// The average of its channels' volumes, rounded as `pactl` rounds
  /// them. 100 is full volume, and other apps can set more.
  pub volume_percent: u32,
  /// Whether it is muted.
  pub muted: bool,
}

/// A sink or a source: where sound goes out, such as speakers, or comes
/// in, such as a microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioNode {
  /// Its name on the server, which the setters take.
  pub name: String,
  /// What a person calls it, such as `Built-in Audio Analog Stereo`.
  pub description: String,
  /// The average of its channels' volumes, as [`Sink`]'s.
  pub volume_percent: u32,
  /// Each channel's volume, rounded as `volume_percent` is, in the order
  /// of `channels`.
  pub volumes: Vec<u32>,
  /// Where each channel plays, such as [`AudioChannel::FrontLeft`]. As
  /// long as `volumes`.
  pub channels: Vec<AudioChannel>,
  /// Whether it is muted.
  pub muted: bool,
}

/// An app playing sound: one of the server's sink inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioStream {
  /// The server's number for it, which the setters take.
  pub index: u32,
  /// The app's name, such as `Firefox`.
  pub app_name: String,
  /// What it plays, such as a tab's title.
  pub title: String,
  /// The app's icon name, or `""`.
  pub icon_name: String,
  /// The name of the sink it plays to.
  pub sink: String,
  /// The average of its channels' volumes, as [`Sink`]'s.
  pub volume_percent: u32,
  /// Each channel's volume, as [`AudioNode::volumes`].
  pub volumes: Vec<u32>,
  /// Where each channel plays, as [`AudioNode::channels`].
  pub channels: Vec<AudioChannel>,
  /// Whether it is muted.
  pub muted: bool,
}

/// Where a channel plays, as PulseAudio's channel map names it, with
/// Quickshell's names for `PwAudioChannel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "each name says where it plays")]
pub enum AudioChannel {
  Mono,
  FrontLeft,
  FrontRight,
  FrontCenter,
  RearCenter,
  RearLeft,
  RearRight,
  LowFrequencyEffects,
  FrontLeftCenter,
  FrontRightCenter,
  SideLeft,
  SideRight,
  /// An auxiliary channel, 0 to 31.
  Aux(u8),
  TopCenter,
  TopFrontLeft,
  TopFrontRight,
  TopFrontCenter,
  TopRearLeft,
  TopRearRight,
  TopRearCenter,
  /// A position the server didn't give.
  Unknown,
}

/// The audio service, from `cx.audio()`. Cloning it is cheap.
#[derive(Clone)]
pub struct Audio {
  sink: ReadSignal<Option<Sink>>,
  lists: ReadLists,
  /// The asking context's, which owns its peak meters.
  owner: Owner,
  /// Empty and never set, for a meter that can't record.
  no_peaks: ReadSignal<Vec<f32>>,
  /// `None` on a context the shell didn't set up.
  service: Option<Rc<AudioService>>,
}

impl Audio {
  /// The default sink. `None` while there is no audio server or the
  /// server has no sink. Every caller gets the same signal.
  pub fn default_sink(&self) -> ReadSignal<Option<Sink>> {
    self.sink
  }

  /// Changes the default sink's volume by `percent`, louder when
  /// positive, and keeps it within 0-100%, so a step from above 100%
  /// lands on 100%. Every channel moves by the same amount, keeping their
  /// balance, except that one reaching 0% or 100% stops there. With no
  /// sink or no server it does nothing.
  pub fn change_volume(&self, percent: i32) {
    self.command(|pulse| pulse.change_volume(percent));
  }

  /// Mutes the default sink, or unmutes it. With no sink or no server it
  /// does nothing.
  pub fn toggle_mute(&self) {
    self.command(Pulse::toggle_mute);
  }

  /// Every sink, in the server's order. Empty while there is no server.
  pub fn sinks(&self) -> ReadSignal<Vec<AudioNode>> {
    self.lists.sinks
  }

  /// Every source except sinks' monitors, in the server's order.
  pub fn sources(&self) -> ReadSignal<Vec<AudioNode>> {
    self.lists.sources
  }

  /// The default source, where a call's microphone sound comes from.
  pub fn default_source(&self) -> ReadSignal<Option<AudioNode>> {
    self.lists.default_source
  }

  /// Every app playing sound, in the server's order.
  pub fn streams(&self) -> ReadSignal<Vec<AudioStream>> {
    self.lists.streams
  }

  /// Makes the sink called `name` the default.
  pub fn set_default_sink(&self, name: &str) {
    let name = name.to_owned();
    self.command(move |pulse| pulse.set_default(Kind::Sink, &name));
  }

  /// Makes the source called `name` the default.
  pub fn set_default_source(&self, name: &str) {
    let name = name.to_owned();
    self.command(move |pulse| pulse.set_default(Kind::Source, &name));
  }

  /// Sets every channel of the sink called `name` to `percent`, at most
  /// 100.
  pub fn set_sink_volume(&self, name: &str, percent: u32) {
    let name = name.to_owned();
    self.command(move |pulse| pulse.set_node_volume(Kind::Sink, &name, percent));
  }

  /// Sets each channel of the sink called `name` to its own percent, at
  /// most 100, in the order of [`AudioNode::channels`], as a balance
  /// slider does. A list of another length than the sink's channels is
  /// logged and not sent.
  pub fn set_sink_volumes(&self, name: &str, percents: &[u32]) {
    let (name, percents) = (name.to_owned(), percents.to_vec());
    self.command(move |pulse| pulse.set_node_volumes(Kind::Sink, &name, &percents));
  }

  /// Mutes the sink called `name`, or unmutes it.
  pub fn set_sink_mute(&self, name: &str, muted: bool) {
    let name = name.to_owned();
    self.command(move |pulse| pulse.set_node_mute(Kind::Sink, &name, muted));
  }

  /// Sets every channel of the source called `name` to `percent`, at most
  /// 100.
  pub fn set_source_volume(&self, name: &str, percent: u32) {
    let name = name.to_owned();
    self.command(move |pulse| pulse.set_node_volume(Kind::Source, &name, percent));
  }

  /// Sets each channel of the source called `name`, as
  /// [`set_sink_volumes`](Self::set_sink_volumes) does a sink's.
  pub fn set_source_volumes(&self, name: &str, percents: &[u32]) {
    let (name, percents) = (name.to_owned(), percents.to_vec());
    self.command(move |pulse| pulse.set_node_volumes(Kind::Source, &name, &percents));
  }

  /// Mutes the source called `name`, or unmutes it, as a microphone
  /// button does.
  pub fn set_source_mute(&self, name: &str, muted: bool) {
    let name = name.to_owned();
    self.command(move |pulse| pulse.set_node_mute(Kind::Source, &name, muted));
  }

  /// Sets every channel of the stream numbered `index` to `percent`, at
  /// most 100.
  pub fn set_stream_volume(&self, index: u32, percent: u32) {
    self.command(move |pulse| pulse.set_stream_volume(index, percent));
  }

  /// Sets each channel of the stream numbered `index`, as
  /// [`set_sink_volumes`](Self::set_sink_volumes) does a sink's.
  pub fn set_stream_volumes(&self, index: u32, percents: &[u32]) {
    let percents = percents.to_vec();
    self.command(move |pulse| pulse.set_stream_volumes(index, &percents));
  }

  /// Mutes the stream numbered `index`, or unmutes it.
  pub fn set_stream_mute(&self, index: u32, muted: bool) {
    self.command(move |pulse| pulse.set_stream_mute(index, muted));
  }

  /// Moves the stream numbered `index` to play to the sink called `sink`.
  pub fn move_stream(&self, index: u32, sink: &str) {
    let sink = sink.to_owned();
    self.command(move |pulse| pulse.move_stream(index, &sink));
  }

  fn command(&self, command: impl FnOnce(&mut Pulse)) {
    let mut pulse = self
      .service
      .as_ref()
      .map(|service| service.pulse.borrow_mut());
    match pulse.as_deref_mut().and_then(Option::as_mut) {
      Some(pulse) => command(pulse),
      None => tracing::debug!("no audio service: nothing to change"),
    }
  }
}

/// `cx.audio()`.
pub(crate) fn audio(cx: &Cx) -> Audio {
  if let Some(service) = cx.service::<AudioService>() {
    return service.audio(cx.owner());
  }
  static LOGGED: Once = Once::new();
  LOGGED.call_once(|| {
    tracing::warn!("cx.audio() needs the services the shell provides; there is no sink");
  });
  Audio {
    sink: cx.signal(None).read_only(),
    lists: Lists {
      sinks: cx.signal(Vec::new()).read_only(),
      sources: cx.signal(Vec::new()).read_only(),
      streams: cx.signal(Vec::new()).read_only(),
      default_source: cx.signal(None).read_only(),
    },
    owner: cx.owner(),
    no_peaks: cx.signal(Vec::new()).read_only(),
    service: None,
  }
}

/// The lists the service shows, as signals to set or read.
#[derive(Clone, Copy)]
struct Lists<N, S, D> {
  sinks: N,
  sources: N,
  streams: S,
  default_source: D,
}

type ReadLists =
  Lists<ReadSignal<Vec<AudioNode>>, ReadSignal<Vec<AudioStream>>, ReadSignal<Option<AudioNode>>>;

type ListSignals =
  Lists<Signal<Vec<AudioNode>>, Signal<Vec<AudioStream>>, Signal<Option<AudioNode>>>;

impl ListSignals {
  fn read_only(&self) -> ReadLists {
    Lists {
      sinks: self.sinks.read_only(),
      sources: self.sources.read_only(),
      streams: self.streams.read_only(),
      default_source: self.default_source.read_only(),
    }
  }
}

/// A sink or a source, for the setters both have.
#[derive(Clone, Copy, Debug)]
enum Kind {
  Sink,
  Source,
}

/// The audio service of one shell. It connects on first use.
pub(crate) struct AudioService {
  shared: Rc<Shared>,
  /// The default sink, under the root owner. Made on first use.
  sink: OnceCell<Signal<Option<Sink>>>,
  /// The sinks, sources and streams, made with `sink`.
  lists: OnceCell<ListSignals>,
  /// libpulse, from first use on. `None` before that, and if libpulse
  /// couldn't start its thread.
  pulse: RefCell<Option<Pulse>>,
  /// The peak meters configs hold, recording or not.
  meters: RefCell<Vec<peaks::Wanted>>,
  next_meter: Cell<u64>,
  /// What a meter that can't record shows, made with `sink`.
  no_peaks: OnceCell<Signal<Vec<f32>>>,
  bad_sounds: RefCell<sound::BadSounds>,
}

/// What libpulse's thread sends: the generation of the context it came
/// from, and what happened.
pub(crate) type Message = (u32, Event);

pub(crate) enum Event {
  /// The context's state changed.
  State,
  /// The server changed, maybe its default sink.
  Server,
  /// The sink with this index changed.
  Sink(u32),
  /// The server's default sink name.
  Default(Option<String>),
  /// The server's default source name.
  DefaultSource(Option<String>),
  /// A source changed.
  Source,
  /// A stream changed.
  Stream,
  /// Every sink, read.
  Sinks(Vec<NodeInfo>),
  /// Every source but monitors, read.
  Sources(Vec<NodeInfo>),
  /// Every stream, read.
  Streams(Vec<StreamInfo>),
  /// The sink asked for by name, or `None` for no such sink.
  Info(Option<Info>),
  /// The peak meter with this id has peaks to read.
  Peaks(u64),
  /// The peak meter with this id changed its state.
  MeterState(u64),
  /// A closing peak meter's stream changed its state.
  Closing,
  /// The sound with this id changed its state or wants samples.
  Sound(u64),
  /// The server played the sound with this id.
  SoundDone(u64),
}

impl AudioService {
  pub(crate) fn new(shared: Rc<Shared>) -> AudioService {
    AudioService {
      shared,
      sink: OnceCell::new(),
      lists: OnceCell::new(),
      pulse: RefCell::new(None),
      meters: RefCell::new(Vec::new()),
      next_meter: Cell::new(0),
      no_peaks: OnceCell::new(),
      bad_sounds: RefCell::default(),
    }
  }

  fn audio(self: &Rc<Self>, owner: Owner) -> Audio {
    let sink = *self.sink.get_or_init(|| {
      // On the loop's next turn, so `cx.audio()` never waits on libpulse.
      self.after(Duration::ZERO, AudioService::start);
      self.shared.root.signal(None)
    });
    let lists = *self.lists.get_or_init(|| {
      let root = &self.shared.root;
      Lists {
        sinks: root.signal(Vec::new()),
        sources: root.signal(Vec::new()),
        streams: root.signal(Vec::new()),
        default_source: root.signal(None),
      }
    });
    let no_peaks = *self
      .no_peaks
      .get_or_init(|| self.shared.root.signal(Vec::new()));
    Audio {
      sink: sink.read_only(),
      lists: lists.read_only(),
      owner,
      no_peaks: no_peaks.read_only(),
      service: Some(Rc::clone(self)),
    }
  }

  /// Starts libpulse's thread and the first connection. Neither waits
  /// for the server.
  fn start(self: &Rc<Self>) {
    let Some(server) = &self.shared.config.audio else {
      return tracing::info!("no audio server is set, so there is no sink");
    };
    let sink = *self
      .sink
      .get()
      .expect("the sink is made before libpulse starts");
    let lists = *self
      .lists
      .get()
      .expect("the lists are made before libpulse starts");
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let Some(mut mainloop) = Mainloop::new() else {
      tracing::error!("libpulse couldn't make its mainloop: no audio");
      return;
    };
    if let Err(error) = mainloop.start() {
      tracing::error!(%error, "libpulse's thread didn't start: no audio");
      return;
    }
    let (sender, channel) = calloop::channel::channel();
    // From here on, dropping `pulse` stops libpulse's thread.
    let pulse = Pulse {
      meters: Vec::new(),
      closing: Vec::new(),
      sounds: Vec::new(),
      next_sound: 0,
      intro: None,
      context: None,
      mainloop,
      sender: Arc::new(sender),
      generation: 0,
      shown: None,
      asked: None,
      sink,
      lists,
      nodes: Nodes::default(),
      down: false,
      retry_until: Instant::now() + server.retry_for,
    };
    let service = Rc::clone(self);
    let on_message = Box::new(move |message| service.on_message(message));
    if event_loop.insert_audio_channel(channel, on_message) {
      *self.pulse.borrow_mut() = Some(pulse);
      self.connect();
    }
  }

  /// Connects to the server, or tries again later.
  fn connect(self: &Rc<Self>) {
    let mut pulse = self.pulse.borrow_mut();
    let (Some(pulse), Some(server)) = (pulse.as_mut(), &self.shared.config.audio) else {
      return;
    };
    // The root is gone, and the shell with it.
    if pulse.sink.try_with(|_| ()).is_none() {
      return;
    }
    if let Err(why) = pulse.connect(server.address.as_deref()) {
      self.lost(pulse, &why);
    }
  }

  fn on_message(self: &Rc<Self>, (generation, event): Message) {
    let mut pulse = self.pulse.borrow_mut();
    // A message from a context that is gone is stale.
    let Some(pulse) = pulse
      .as_mut()
      .filter(|pulse| pulse.generation == generation)
    else {
      return;
    };
    match event {
      Event::State => {
        if let Err(why) = pulse.on_state() {
          self.lost(pulse, &why);
        }
      }
      Event::Server => {
        pulse.read_default();
        pulse.read_lists();
      }
      Event::Sink(index) => {
        if pulse.shown.as_ref().is_some_and(|s| s.index == index) {
          pulse.read_default();
        }
        pulse.read_sinks();
      }
      Event::Default(name) => pulse.read_sink(name),
      Event::Info(info) => pulse.show(info),
      Event::DefaultSource(name) => {
        pulse.nodes.default_source = name;
        pulse.show_lists();
      }
      Event::Source => pulse.read_sources(),
      Event::Stream => pulse.read_streams(),
      Event::Sinks(sinks) => {
        pulse.nodes.sinks = sinks;
        self.show_lists(pulse);
      }
      Event::Sources(sources) => {
        pulse.nodes.sources = sources;
        self.show_lists(pulse);
      }
      Event::Streams(streams) => {
        pulse.nodes.streams = streams;
        self.show_lists(pulse);
      }
      Event::Peaks(id) => pulse.read_peaks(id),
      Event::MeterState(id) => pulse.on_meter_state(id),
      Event::Closing => pulse.on_closing(),
      Event::Sound(id) => pulse.feed_sound(id),
      Event::SoundDone(id) => pulse.sound_done(id),
    }
  }

  /// Shows the lists as last read, and moves the peak meters to match.
  fn show_lists(&self, pulse: &mut Pulse) {
    pulse.show_lists();
    pulse.sync_meters(&self.meters.borrow());
  }

  /// Drops the connection, shows no sink and tries again every `retry`
  /// until `retry_for` has passed since startup, or since a working
  /// connection dropped. Logs once per outage, not on every try, and
  /// once on giving up.
  fn lost(self: &Rc<Self>, pulse: &mut Pulse, why: &str) {
    let Some(Server {
      retry, retry_for, ..
    }) = self.shared.config.audio
    else {
      return;
    };
    let now = Instant::now();
    // It was ready, so the server was there: give it a new while.
    if pulse.intro.is_some() {
      pulse.retry_until = now + retry_for;
    }
    if pulse.disconnect() {
      tracing::warn!(
        reason = why,
        "no audio server, trying again every {retry:?} for up to {retry_for:?}"
      );
    }
    if now + retry > pulse.retry_until {
      // No timer from here on, so audio never wakes the shell again.
      return tracing::info!(
        "stopped trying to reach the audio server; it is tried again when the shell restarts"
      );
    }
    self.after(retry, AudioService::connect);
  }

  /// Runs `f` from the event loop once, `delay` from now.
  fn after(self: &Rc<Self>, delay: Duration, f: fn(&Rc<AudioService>)) {
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let service = Rc::clone(self);
    let once = move || {
      f(&service);
      TimeoutAction::Drop
    };
    event_loop.insert_timer(Instant::now() + delay, Box::new(once));
  }
}

/// One shell's libpulse objects and what they last told us. It stays on
/// the main thread: `Mainloop` holds an `Rc`, so the compiler won't let it
/// go to another.
struct Pulse {
  // Declared before `mainloop`, so they drop before it, the streams
  // before their context.
  meters: Vec<peaks::Meter>,
  /// Streams of meters closed before the server made them, waiting to be.
  closing: Vec<peaks::Recording>,
  sounds: Vec<sound::Sound>,
  next_sound: u64,
  intro: Option<Introspector>,
  context: Option<Context>,
  mainloop: Mainloop,
  /// Shared by every callback. Each `Sender` wakes the loop when it
  /// drops, so a callback that holds its own would wake the shell when
  /// libpulse drops it, with nothing to read.
  sender: Arc<Sender<Message>>,
  /// Bumped when a context goes, so the messages it left behind are
  /// dropped.
  generation: u32,
  /// The default sink as last read.
  shown: Option<Info>,
  /// The default sink as the last command left it, and when that was
  /// sent. pipewire-pulse answers a read sent right after a set with the
  /// sink as it was before, so commands build on this instead until a
  /// read echoes it or [`SETTLE`] passes.
  asked: Option<(Info, Instant)>,
  sink: Signal<Option<Sink>>,
  lists: ListSignals,
  /// Every sink, source and stream as last read.
  nodes: Nodes,
  /// Whether the server is gone and that was logged.
  down: bool,
  /// When the service stops trying to connect, if it can't.
  retry_until: Instant,
}

/// Every sink, source and stream as libpulse last reported them.
#[derive(Default)]
struct Nodes {
  sinks: Vec<NodeInfo>,
  sources: Vec<NodeInfo>,
  streams: Vec<StreamInfo>,
  default_source: Option<String>,
}

/// A sink or source as libpulse reported it.
#[derive(Clone, PartialEq)]
pub(crate) struct NodeInfo {
  index: u32,
  name: String,
  description: String,
  volumes: ChannelVolumes,
  map: Map,
  muted: bool,
  /// A sink's monitor: a source, but not one listed.
  monitor: bool,
  /// The name of a sink's monitor, which a peak meter records.
  monitor_source: Option<String>,
  /// Whether a sink plays: an idle one sends its meters nothing.
  running: bool,
}

impl NodeInfo {
  fn node(&self) -> AudioNode {
    let (volumes, channels) = each_channel(&self.volumes, &self.map);
    AudioNode {
      name: self.name.clone(),
      description: self.description.clone(),
      volume_percent: percent(self.volumes.avg()),
      volumes,
      channels,
      muted: self.muted,
    }
  }
}

/// A sink input as libpulse reported it.
#[derive(Clone, PartialEq)]
pub(crate) struct StreamInfo {
  index: u32,
  app_name: String,
  title: String,
  icon_name: String,
  sink: u32,
  volumes: ChannelVolumes,
  map: Map,
  muted: bool,
  /// Paused: it sends its meters nothing.
  corked: bool,
}

impl StreamInfo {
  fn read(info: &SinkInputInfo) -> StreamInfo {
    use libpulse_binding::proplist::properties::{APPLICATION_ICON_NAME, APPLICATION_NAME};

    let property = |key| info.proplist.get_str(key).unwrap_or_default();
    StreamInfo {
      index: info.index,
      app_name: property(APPLICATION_NAME),
      title: info.name.as_deref().unwrap_or_default().to_owned(),
      icon_name: property(APPLICATION_ICON_NAME),
      sink: info.sink,
      volumes: info.volume,
      map: info.channel_map,
      muted: info.mute,
      corked: info.corked,
    }
  }
}

/// The default sink as libpulse reported it.
#[derive(Clone, PartialEq)]
pub(crate) struct Info {
  index: u32,
  name: String,
  volumes: ChannelVolumes,
  muted: bool,
}

impl Pulse {
  /// Makes a context and starts connecting it to `server`, or to
  /// libpulse's default for `None`.
  fn connect(&mut self, server: Option<&str>) -> Result<(), String> {
    let lock = Locked::new(&mut self.mainloop);
    let mut context = Context::new(&*lock.0, "crabshell").ok_or("libpulse made no context")?;
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    context.set_state_callback(Some(Box::new(on_pa_thread(move || {
      let _ = sender.send((generation, Event::State));
    }))));
    // NOAUTOSPAWN: a retry must never start a daemon next to pipewire-pulse.
    let connected = context.connect(server, FlagSet::NOAUTOSPAWN, None);
    self.context = Some(context);
    connected.map_err(|error| format!("{error}"))
  }

  /// Acts on the context's new state. Once it is ready, subscribes to
  /// sink and server events and reads the default sink. `Err` when the
  /// connection failed or ended.
  fn on_state(&mut self) -> Result<(), String> {
    let lock = Locked::new(&mut self.mainloop);
    let Some(context) = self.context.as_mut() else {
      return Ok(());
    };
    match context.get_state() {
      // Several queued messages can each find it ready.
      State::Ready if self.intro.is_none() => {
        let (sender, generation) = (Arc::clone(&self.sender), self.generation);
        let on_event = move |facility, _, index| {
          let event = match facility {
            Some(Facility::Server) => Event::Server,
            Some(Facility::Sink) => Event::Sink(index),
            Some(Facility::Source) => Event::Source,
            Some(Facility::SinkInput) => Event::Stream,
            _ => return,
          };
          let _ = sender.send((generation, event));
        };
        context.set_subscribe_callback(Some(Box::new(on_pa_thread(on_event))));
        let interest = InterestMaskSet::SINK
          | InterestMaskSet::SOURCE
          | InterestMaskSet::SINK_INPUT
          | InterestMaskSet::SERVER;
        context.subscribe(interest, on_pa_thread(|_| {}));
        self.intro = Some(context.introspect());
        drop(lock);
        if std::mem::take(&mut self.down) {
          tracing::info!("the audio server is back");
        } else {
          tracing::debug!("connected to the audio server");
        }
        self.read_default();
        self.read_lists();
        Ok(())
      }
      State::Failed | State::Terminated => Err(format!("{}", context.errno())),
      _ => Ok(()),
    }
  }

  /// Runs `call` under the lock if the context is ready, and says
  /// whether it ran. libpulse returns no operation from a context that
  /// isn't, and the binding panics on that.
  fn when_ready(&mut self, call: impl FnOnce(&mut Introspector)) -> bool {
    self.when_ready_with(|_, intro| call(intro))
  }

  /// [`when_ready`](Self::when_ready), with the context too.
  fn when_ready_with(&mut self, call: impl FnOnce(&mut Context, &mut Introspector)) -> bool {
    let _lock = Locked::new(&mut self.mainloop);
    match (&mut self.context, &mut self.intro) {
      (Some(context), Some(intro)) if context.get_state() == State::Ready => {
        call(context, intro);
        true
      }
      _ => false,
    }
  }

  /// Asks the server for its default sink's name.
  fn read_default(&mut self) {
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    self.when_ready(|intro| {
      // shortcut: on an error reply libpulse-binding panics on its thread instead of calling
      // this, and the panic hook prints it raw; logging it needs the C API, so `unsafe`.
      intro.get_server_info(on_pa_thread(move |info: &ServerInfo| {
        let name = |name: &Option<std::borrow::Cow<'_, str>>| {
          let name = name.as_deref().filter(|name| !name.is_empty());
          name.map(str::to_owned)
        };
        let _ = sender.send((generation, Event::Default(name(&info.default_sink_name))));
        let source = Event::DefaultSource(name(&info.default_source_name));
        let _ = sender.send((generation, source));
      }));
    });
  }

  /// Asks for the sink called `name`, or shows none.
  fn read_sink(&mut self, name: Option<String>) {
    let Some(name) = name else {
      return self.show(None);
    };
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    self.when_ready(|intro| {
      let mut found = None;
      let on_result = move |result: ListResult<&SinkInfo>| match result {
        ListResult::Item(info) => found = Info::read(info),
        ListResult::End | ListResult::Error => {
          let _ = sender.send((generation, Event::Info(found.take())));
        }
      };
      intro.get_sink_info_by_name(&name, on_pa_thread(on_result));
    });
  }

  /// Asks for every sink, source and stream.
  fn read_lists(&mut self) {
    self.read_sinks();
    self.read_sources();
    self.read_streams();
  }

  fn read_sinks(&mut self) {
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    self.when_ready(|intro| {
      let mut found = Vec::new();
      let on_result = move |result: ListResult<&SinkInfo>| match result {
        ListResult::Item(info) => {
          let read = node(
            info.index,
            &info.name,
            &info.description,
            info.volume,
            info.channel_map,
            info.mute,
          );
          let monitor_source = info.monitor_source_name.as_deref().map(str::to_owned);
          let running = info.state == SinkState::Running;
          found.extend(read.map(|node| NodeInfo {
            monitor_source,
            running,
            ..node
          }));
        }
        ListResult::End => {
          let _ = sender.send((generation, Event::Sinks(std::mem::take(&mut found))));
        }
        // The last list stays.
        ListResult::Error => tracing::warn!("the audio server didn't list its sinks"),
      };
      intro.get_sink_info_list(on_pa_thread(on_result));
    });
  }

  fn read_sources(&mut self) {
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    self.when_ready(|intro| {
      let mut found = Vec::new();
      let on_result = move |result: ListResult<&SourceInfo>| match result {
        ListResult::Item(info) => {
          let read = node(
            info.index,
            &info.name,
            &info.description,
            info.volume,
            info.channel_map,
            info.mute,
          );
          let monitor = info.monitor_of_sink.is_some();
          found.extend(read.map(|node| NodeInfo { monitor, ..node }));
        }
        ListResult::End => {
          let _ = sender.send((generation, Event::Sources(std::mem::take(&mut found))));
        }
        ListResult::Error => tracing::warn!("the audio server didn't list its sources"),
      };
      intro.get_source_info_list(on_pa_thread(on_result));
    });
  }

  fn read_streams(&mut self) {
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    self.when_ready(|intro| {
      let mut found = Vec::new();
      let on_result = move |result: ListResult<&SinkInputInfo>| match result {
        ListResult::Item(info) if info.volume.is_valid() => found.push(StreamInfo::read(info)),
        ListResult::Item(_) => {}
        ListResult::End => {
          let _ = sender.send((generation, Event::Streams(std::mem::take(&mut found))));
        }
        ListResult::Error => tracing::warn!("the audio server didn't list its streams"),
      };
      intro.get_sink_input_info_list(on_pa_thread(on_result));
    });
  }

  /// Shows the lists as last read, capped. Each signal is set only when
  /// what it shows changed.
  fn show_lists(&mut self) {
    let Nodes {
      sinks,
      sources,
      streams,
      default_source,
    } = &self.nodes;
    let capped = |nodes: &[NodeInfo]| -> Vec<AudioNode> {
      nodes.iter().take(MAX_NODES).map(NodeInfo::node).collect()
    };
    let sink_name = |index| {
      let sink = sinks.iter().find(|sink| sink.index == index);
      sink.map_or(String::new(), |sink| sink.name.clone())
    };
    let streams = streams
      .iter()
      .take(MAX_NODES)
      .map(|stream| {
        let (volumes, channels) = each_channel(&stream.volumes, &stream.map);
        AudioStream {
          index: stream.index,
          app_name: stream.app_name.clone(),
          title: stream.title.clone(),
          icon_name: stream.icon_name.clone(),
          sink: sink_name(stream.sink),
          volume_percent: percent(stream.volumes.avg()),
          volumes,
          channels,
          muted: stream.muted,
        }
      })
      .collect();
    let default = default_source
      .as_ref()
      .and_then(|name| sources.iter().find(|source| source.name == *name))
      .map(NodeInfo::node);
    set_changed(self.lists.sinks, capped(sinks));
    // A sink's monitor isn't a source a person picks, though it can be
    // the default, as while recording what plays.
    let listed: Vec<NodeInfo> = sources.iter().filter(|s| !s.monitor).cloned().collect();
    set_changed(self.lists.sources, capped(&listed));
    set_changed(self.lists.streams, streams);
    set_changed(self.lists.default_source, default);
  }

  fn set_default(&mut self, kind: Kind, name: &str) {
    let sent = self.when_ready_with(|context, _| {
      let done = on_pa_thread(|_| {});
      match kind {
        Kind::Sink => drop(context.set_default_sink(name, done)),
        Kind::Source => drop(context.set_default_source(name, done)),
      }
    });
    if !sent {
      tracing::debug!(name, "no audio server: the default stays");
    }
  }

  fn set_node_volume(&mut self, kind: Kind, name: &str, percent: u32) {
    let nodes = match kind {
      Kind::Sink => &self.nodes.sinks,
      Kind::Source => &self.nodes.sources,
    };
    let Some(node) = nodes.iter().find(|node| node.name == name) else {
      return tracing::debug!(name, ?kind, "no such node: its volume stays");
    };
    let volumes = at(node.volumes, percent);
    self.when_ready(|intro| match kind {
      Kind::Sink => drop(intro.set_sink_volume_by_name(name, &volumes, None)),
      Kind::Source => drop(intro.set_source_volume_by_name(name, &volumes, None)),
    });
  }

  fn set_node_volumes(&mut self, kind: Kind, name: &str, percents: &[u32]) {
    let nodes = match kind {
      Kind::Sink => &self.nodes.sinks,
      Kind::Source => &self.nodes.sources,
    };
    let Some(node) = nodes.iter().find(|node| node.name == name) else {
      return tracing::debug!(name, ?kind, "no such node: its volumes stay");
    };
    let Some(volumes) = each_at(node.volumes, percents) else {
      return tracing::warn!(
        name,
        ?kind,
        ?percents,
        "not one volume per channel: none set"
      );
    };
    self.when_ready(|intro| match kind {
      Kind::Sink => drop(intro.set_sink_volume_by_name(name, &volumes, None)),
      Kind::Source => drop(intro.set_source_volume_by_name(name, &volumes, None)),
    });
  }

  fn set_node_mute(&mut self, kind: Kind, name: &str, muted: bool) {
    self.when_ready(|intro| match kind {
      Kind::Sink => drop(intro.set_sink_mute_by_name(name, muted, None)),
      Kind::Source => drop(intro.set_source_mute_by_name(name, muted, None)),
    });
  }

  fn set_stream_volume(&mut self, index: u32, percent: u32) {
    let Some(stream) = self
      .nodes
      .streams
      .iter()
      .find(|stream| stream.index == index)
    else {
      return tracing::debug!(index, "no such stream: its volume stays");
    };
    let volumes = at(stream.volumes, percent);
    self.when_ready(|intro| drop(intro.set_sink_input_volume(index, &volumes, None)));
  }

  fn set_stream_volumes(&mut self, index: u32, percents: &[u32]) {
    let streams = &self.nodes.streams;
    let Some(stream) = streams.iter().find(|stream| stream.index == index) else {
      return tracing::debug!(index, "no such stream: its volumes stay");
    };
    let Some(volumes) = each_at(stream.volumes, percents) else {
      return tracing::warn!(index, ?percents, "not one volume per channel: none set");
    };
    self.when_ready(|intro| drop(intro.set_sink_input_volume(index, &volumes, None)));
  }

  fn set_stream_mute(&mut self, index: u32, muted: bool) {
    self.when_ready(|intro| drop(intro.set_sink_input_mute(index, muted, None)));
  }

  fn move_stream(&mut self, index: u32, sink: &str) {
    self.when_ready(|intro| drop(intro.move_sink_input_by_name(index, sink, None)));
  }

  /// Shows `info`, or no sink. The signal is set only when that changes
  /// what the bar shows, since every set repaints.
  fn show(&mut self, info: Option<Info>) {
    // An echo of the last command, or another sink, ends its stand-in.
    let stands = match (&self.asked, &info) {
      (Some((asked, _)), Some(read)) => read != asked && read.index == asked.index,
      _ => false,
    };
    if !stands {
      self.asked = None;
    }
    let sink = info.as_ref().map(Info::sink);
    self.shown = info;
    if self.sink.try_with(|shown| *shown != sink) == Some(true) {
      self.sink.set(sink);
    }
  }

  /// What the next command builds on: the last one's result while it
  /// stands in, or else the last read.
  fn base(&self) -> Option<Info> {
    match &self.asked {
      Some((asked, at)) if at.elapsed() < SETTLE => Some(asked.clone()),
      _ => self.shown.clone(),
    }
  }

  fn change_volume(&mut self, percent: i32) {
    let Some(mut next) = self.base() else {
      return tracing::debug!("no default sink: the volume stays");
    };
    next.volumes = changed(next.volumes, percent);
    let sent = self.when_ready(|intro| {
      intro.set_sink_volume_by_name(&next.name, &next.volumes, None);
    });
    if sent {
      self.asked = Some((next, Instant::now()));
    } else {
      tracing::debug!("no audio server: the volume stays");
    }
  }

  fn toggle_mute(&mut self) {
    let Some(mut next) = self.base() else {
      return tracing::debug!("no default sink: nothing to mute");
    };
    next.muted = !next.muted;
    let sent = self.when_ready(|intro| {
      intro.set_sink_mute_by_name(&next.name, next.muted, None);
    });
    if sent {
      self.asked = Some((next, Instant::now()));
    } else {
      tracing::debug!("no audio server: nothing to mute");
    }
  }

  /// Drops the context and shows no sink. `true` when this starts an
  /// outage.
  fn disconnect(&mut self) -> bool {
    let lock = Locked::new(&mut self.mainloop);
    peaks::close_all(&mut self.meters, &mut self.closing);
    self.sounds.clear();
    self.intro = None;
    if let Some(mut context) = self.context.take() {
      // Cleared first: disconnecting calls the state callback.
      context.set_state_callback(None);
      context.set_subscribe_callback(None);
      context.disconnect();
    }
    drop(lock);
    self.generation += 1;
    self.show(None);
    self.nodes = Nodes::default();
    self.show_lists();
    !std::mem::replace(&mut self.down, true)
  }
}

impl Drop for Pulse {
  fn drop(&mut self) {
    // Unlocked, as libpulse asks. Once its thread is gone, the context
    // and introspector drop with nothing running beside them.
    self.mainloop.stop();
  }
}

impl Info {
  /// Copies what we need out of libpulse's `info`, which lives only for
  /// the callback. A sink with no name or a bad volume is skipped.
  fn read(info: &SinkInfo) -> Option<Info> {
    let (Some(name), true) = (info.name.as_deref(), info.volume.is_valid()) else {
      tracing::warn!(
        index = info.index,
        "skipped a sink with no name or a bad volume"
      );
      return None;
    };
    Some(Info {
      index: info.index,
      name: name.to_owned(),
      volumes: info.volume,
      muted: info.mute,
    })
  }

  fn sink(&self) -> Sink {
    Sink {
      name: self.name.clone(),
      volume_percent: percent(self.volumes.avg()),
      muted: self.muted,
    }
  }
}

/// The mainloop's lock, held until this drops. libpulse's thread waits for
/// it, so it must be let go even on an early return or a panic.
struct Locked<'a>(&'a mut Mainloop);

impl<'a> Locked<'a> {
  fn new(mainloop: &'a mut Mainloop) -> Locked<'a> {
    mainloop.lock();
    Locked(mainloop)
  }
}

impl Drop for Locked<'_> {
  fn drop(&mut self) {
    self.0.unlock();
  }
}

/// libpulse runs its callbacks on its own thread but doesn't ask for
/// `Send`. Wrapping each one in this makes the compiler check it.
fn on_pa_thread<F: Send + 'static>(callback: F) -> F {
  callback
}

/// The percent `pactl get-sink-volume` shows for `volume`: 100 is
/// `PA_VOLUME_NORM`, and halves round up.
fn percent(volume: Volume) -> u32 {
  let norm = u64::from(Volume::NORMAL.0);
  // At most u32::MAX * 100 / norm, which fits.
  ((u64::from(volume.0) * 100 + norm / 2) / norm) as u32
}

/// `volumes` changed by `delta` percent. Every channel moves by the amount
/// that puts their average on the shown percent plus `delta`, kept within
/// 0-100%. A channel that would pass 0% or 100% stops there, and then the
/// average falls short of that.
fn changed(mut volumes: ChannelVolumes, delta: i32) -> ChannelVolumes {
  let norm = i64::from(Volume::NORMAL.0);
  let average = volumes.avg();
  let target = (i64::from(percent(average)) + i64::from(delta)).clamp(0, 100);
  let shift = (target * norm + 50) / 100 - i64::from(average.0);
  for volume in volumes.get_mut() {
    // Within 0..=norm, which fits.
    volume.0 = (i64::from(volume.0) + shift).clamp(0, norm) as u32;
  }
  volumes
}

/// A sink or source as libpulse reported it, unless it has no name or a
/// bad volume.
fn node(
  index: u32,
  name: &Option<std::borrow::Cow<'_, str>>,
  description: &Option<std::borrow::Cow<'_, str>>,
  volumes: ChannelVolumes,
  map: Map,
  muted: bool,
) -> Option<NodeInfo> {
  let name = name.as_deref().filter(|_| volumes.is_valid())?;
  Some(NodeInfo {
    index,
    name: name.to_owned(),
    description: description.as_deref().unwrap_or(name).to_owned(),
    volumes,
    map,
    muted,
    monitor: false,
    monitor_source: None,
    running: false,
  })
}

/// Each channel's percent and position, at most `PA_CHANNELS_MAX` of
/// them. A position the map lacks is unknown, so both are as long.
fn each_channel(volumes: &ChannelVolumes, map: &Map) -> (Vec<u32>, Vec<AudioChannel>) {
  let max = usize::from(Map::CHANNELS_MAX);
  let percents: Vec<u32> = volumes
    .get()
    .iter()
    .take(max)
    .map(|v| percent(*v))
    .collect();
  let positions = map.get().iter().map(|&position| channel(position));
  let positions = positions.chain(std::iter::repeat(AudioChannel::Unknown));
  let channels = positions.take(percents.len()).collect();
  (percents, channels)
}

/// Quickshell's name for a PulseAudio channel position.
fn channel(position: Position) -> AudioChannel {
  use AudioChannel as C;
  // Aux0 to Aux31 are numbered in a row.
  let aux = position as i32 - Position::Aux0 as i32;
  match position {
    Position::Mono => C::Mono,
    Position::FrontLeft => C::FrontLeft,
    Position::FrontRight => C::FrontRight,
    Position::FrontCenter => C::FrontCenter,
    Position::RearCenter => C::RearCenter,
    Position::RearLeft => C::RearLeft,
    Position::RearRight => C::RearRight,
    Position::Lfe => C::LowFrequencyEffects,
    Position::FrontLeftOfCenter => C::FrontLeftCenter,
    Position::FrontRightOfCenter => C::FrontRightCenter,
    Position::SideLeft => C::SideLeft,
    Position::SideRight => C::SideRight,
    Position::TopCenter => C::TopCenter,
    Position::TopFrontLeft => C::TopFrontLeft,
    Position::TopFrontRight => C::TopFrontRight,
    Position::TopFrontCenter => C::TopFrontCenter,
    Position::TopRearLeft => C::TopRearLeft,
    Position::TopRearRight => C::TopRearRight,
    Position::TopRearCenter => C::TopRearCenter,
    _ => match u8::try_from(aux) {
      Ok(aux) if aux < 32 => C::Aux(aux),
      _ => C::Unknown,
    },
  }
}

/// `volumes` with each channel at its own percent, at most 100, or `None`
/// when `percents` doesn't have one per channel.
fn each_at(mut volumes: ChannelVolumes, percents: &[u32]) -> Option<ChannelVolumes> {
  if percents.len() != usize::from(volumes.len()) {
    return None;
  }
  let norm = Volume::NORMAL.0;
  for (volume, percent) in volumes.get_mut().iter_mut().zip(percents) {
    // Rounded as pactl sets a percent; at most 100 * norm + 50, which fits.
    *volume = Volume(((*percent).min(100) * norm + 50) / 100);
  }
  Some(volumes)
}

/// `volumes` moved so their average is `percent`, at most 100, keeping
/// the channels' balance as [`changed`] does.
fn at(volumes: ChannelVolumes, percent: u32) -> ChannelVolumes {
  let shown = self::percent(volumes.avg());
  // Both at most a few thousand, which fit.
  changed(volumes, percent.min(100) as i32 - shown.min(10_000) as i32)
}
