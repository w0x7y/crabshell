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
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::threaded::Mainloop;

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
}

impl Audio {
    /// The default sink. `None` while there is no audio server or the
    /// server has no sink. Every caller gets the same signal.
    pub fn default_sink(&self) -> ReadSignal<Option<Sink>> {
        self.sink
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
            context: None,
            mainloop,
            sender,
            generation: 0,
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
    // Declared before `mainloop`, so it drops before it.
    context: Option<Context>,
    mainloop: Mainloop,
    sender: Sender<Message>,
    /// Bumped when a context goes, so the messages it left behind are
    /// dropped.
    generation: u32,
    sink: Signal<Option<Sink>>,
    /// Whether the server is gone and that was logged.
    down: bool,
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

    /// Acts on the context's new state. `Err` when the connection failed
    /// or ended.
    fn on_state(&mut self) -> Result<(), String> {
        let lock = Locked::new(&mut self.mainloop);
        let Some(context) = self.context.as_mut() else {
            return Ok(());
        };
        match context.get_state() {
            State::Ready => {
                drop(lock);
                if std::mem::take(&mut self.down) {
                    tracing::info!("the audio server is back");
                }
                Ok(())
            }
            State::Failed | State::Terminated => Err(format!("{}", context.errno())),
            _ => Ok(()),
        }
    }

    /// Drops the context. `true` when this starts an outage.
    fn disconnect(&mut self) -> bool {
        let lock = Locked::new(&mut self.mainloop);
        if let Some(mut context) = self.context.take() {
            // Cleared first: disconnecting calls the state callback.
            context.set_state_callback(None);
            context.disconnect();
        }
        drop(lock);
        self.generation += 1;
        !std::mem::replace(&mut self.down, true)
    }
}

impl Drop for Pulse {
    fn drop(&mut self) {
        // Unlocked, as libpulse asks. Once its thread is gone, the context
        // drops with nothing running beside it.
        self.mainloop.stop();
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
