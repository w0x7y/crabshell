//! Audio: the default sink's volume and mute, from the PulseAudio server,
//! which pipewire-pulse serves on PipeWire systems.
//!
//! libpulse runs its mainloop on a thread of its own. Every libpulse call
//! from this side runs on the main thread under the mainloop's lock.
//! libpulse's callbacks run on its thread and only send a message over a
//! calloop channel, so the signal is set on the main thread like any other.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use calloop::channel::Sender;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::{Introspector, ServerInfo, SinkInfo};
use libpulse_binding::context::subscribe::{Facility, InterestMaskSet};
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::threaded::Mainloop;
use libpulse_binding::volume::{ChannelVolumes, Volume};

use crate::Shared;

#[cfg(test)]
mod tests;

/// How long the service waits before each reconnect.
pub(crate) const RETRY: Duration = Duration::from_secs(2);

/// Where the audio server is.
pub(crate) struct Server {
    /// In libpulse's server syntax. `None` for libpulse's default, which
    /// `PULSE_SERVER` and client.conf can set.
    pub(crate) address: Option<String>,
    /// How long to wait before each reconnect: [`RETRY`], less in tests.
    pub(crate) retry: Duration,
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

/// The audio service, from `cx.audio()`. Cloning it is cheap.
#[derive(Clone)]
pub struct Audio {
    sink: ReadSignal<Option<Sink>>,
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
    /// positive, and keeps it within 0-100%. Every channel moves by the
    /// same amount, so their balance stays. With no sink or no server it
    /// does nothing.
    pub fn change_volume(&self, percent: i32) {
        self.command(|pulse| pulse.change_volume(percent));
    }

    /// Mutes the default sink, or unmutes it. With no sink or no server it
    /// does nothing.
    pub fn toggle_mute(&self) {
        self.command(Pulse::toggle_mute);
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
        return service.audio();
    }
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
        tracing::warn!("cx.audio() needs the services the shell provides; there is no sink");
    });
    Audio {
        sink: cx.signal(None).read_only(),
        service: None,
    }
}

/// The audio service of one shell. It connects on first use.
pub(crate) struct AudioService {
    shared: Rc<Shared>,
    /// The default sink, under the root owner. Made on first use.
    sink: OnceCell<Signal<Option<Sink>>>,
    /// libpulse, from first use on. `None` before that, and if libpulse
    /// couldn't start its thread.
    pulse: RefCell<Option<Pulse>>,
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
    /// The sink asked for by name, or `None` for no such sink.
    Info(Option<Info>),
}

impl AudioService {
    pub(crate) fn new(shared: Rc<Shared>) -> AudioService {
        AudioService {
            shared,
            sink: OnceCell::new(),
            pulse: RefCell::new(None),
        }
    }

    fn audio(self: &Rc<Self>) -> Audio {
        let sink = *self.sink.get_or_init(|| {
            // On the loop's next turn, after the windows' first commit.
            self.after(Duration::ZERO, AudioService::start);
            self.shared.root.signal(None)
        });
        Audio {
            sink: sink.read_only(),
            service: Some(Rc::clone(self)),
        }
    }

    /// Starts libpulse's thread and the first connection. Neither waits
    /// for the server.
    fn start(self: &Rc<Self>) {
        if self.shared.config.audio.is_none() {
            return tracing::info!("no audio server is set, so there is no sink");
        }
        let sink = *self
            .sink
            .get()
            .expect("the sink is made before libpulse starts");
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
            intro: None,
            context: None,
            mainloop,
            sender,
            generation: 0,
            shown: None,
            sink,
            down: false,
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
            Event::Server => pulse.read_default(),
            Event::Sink(index) if pulse.shown.as_ref().is_some_and(|s| s.index == index) => {
                pulse.read_default();
            }
            Event::Sink(_) => {}
            Event::Default(name) => pulse.read_sink(name),
            Event::Info(info) => pulse.show(info),
        }
    }

    /// Drops the connection, shows no sink and connects again later. Logs
    /// once per outage, not on every try.
    fn lost(self: &Rc<Self>, pulse: &mut Pulse, why: &str) {
        let Some(Server { retry, .. }) = self.shared.config.audio else {
            return;
        };
        if pulse.disconnect() {
            tracing::warn!(
                reason = why,
                "no audio server, trying again every {retry:?}"
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
    // Declared before `mainloop`, so they drop before it.
    intro: Option<Introspector>,
    context: Option<Context>,
    mainloop: Mainloop,
    sender: Sender<Message>,
    /// Bumped when a context goes, so the messages it left behind are
    /// dropped.
    generation: u32,
    /// The default sink as last read.
    shown: Option<Info>,
    sink: Signal<Option<Sink>>,
    /// Whether the server is gone and that was logged.
    down: bool,
}

/// The default sink as libpulse reported it.
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
        let (sender, generation) = (self.sender.clone(), self.generation);
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
                let (sender, generation) = (self.sender.clone(), self.generation);
                let on_event = move |facility, _, index| {
                    let event = match facility {
                        Some(Facility::Server) => Event::Server,
                        Some(Facility::Sink) => Event::Sink(index),
                        _ => return,
                    };
                    let _ = sender.send((generation, event));
                };
                context.set_subscribe_callback(Some(Box::new(on_pa_thread(on_event))));
                let interest = InterestMaskSet::SINK | InterestMaskSet::SERVER;
                context.subscribe(interest, on_pa_thread(|_| {}));
                self.intro = Some(context.introspect());
                drop(lock);
                if std::mem::take(&mut self.down) {
                    tracing::info!("the audio server is back");
                } else {
                    tracing::debug!("connected to the audio server");
                }
                self.read_default();
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
        let _lock = Locked::new(&mut self.mainloop);
        match (&self.context, &mut self.intro) {
            (Some(context), Some(intro)) if context.get_state() == State::Ready => {
                call(intro);
                true
            }
            _ => false,
        }
    }

    /// Asks the server for its default sink's name.
    fn read_default(&mut self) {
        let (sender, generation) = (self.sender.clone(), self.generation);
        self.when_ready(|intro| {
            intro.get_server_info(on_pa_thread(move |info: &ServerInfo| {
                let name = info
                    .default_sink_name
                    .as_deref()
                    .filter(|name| !name.is_empty());
                let _ = sender.send((generation, Event::Default(name.map(str::to_owned))));
            }));
        });
    }

    /// Asks for the sink called `name`, or shows none.
    fn read_sink(&mut self, name: Option<String>) {
        let Some(name) = name else {
            return self.show(None);
        };
        let (sender, generation) = (self.sender.clone(), self.generation);
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

    /// Shows `info`, or no sink. The signal is set only when that changes
    /// what the bar shows, since every set repaints.
    fn show(&mut self, info: Option<Info>) {
        let sink = info.as_ref().map(Info::sink);
        self.shown = info;
        if self.sink.try_with(|shown| *shown != sink) == Some(true) {
            self.sink.set(sink);
        }
    }

    fn change_volume(&mut self, percent: i32) {
        let Some(shown) = &self.shown else {
            return tracing::debug!("no default sink: the volume stays");
        };
        let (name, volumes) = (shown.name.clone(), changed(shown.volumes, percent));
        let sent = self.when_ready(|intro| {
            intro.set_sink_volume_by_name(&name, &volumes, None);
        });
        match (sent, &mut self.shown) {
            // So a second step before the server's reply builds on this one.
            (true, Some(shown)) => shown.volumes = volumes,
            _ => tracing::debug!("no audio server: the volume stays"),
        }
    }

    fn toggle_mute(&mut self) {
        let Some(shown) = &self.shown else {
            return tracing::debug!("no default sink: nothing to mute");
        };
        let (name, muted) = (shown.name.clone(), !shown.muted);
        let sent = self.when_ready(|intro| {
            intro.set_sink_mute_by_name(&name, muted, None);
        });
        match (sent, &mut self.shown) {
            (true, Some(shown)) => shown.muted = muted,
            _ => tracing::debug!("no audio server: nothing to mute"),
        }
    }

    /// Drops the context and shows no sink. `true` when this starts an
    /// outage.
    fn disconnect(&mut self) -> bool {
        let lock = Locked::new(&mut self.mainloop);
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

/// `volumes` changed by `delta` percent. Every channel moves by the same
/// amount, so their balance stays, and the average lands on the whole
/// percent the bar shows next. Every channel stays within 0-100%.
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
