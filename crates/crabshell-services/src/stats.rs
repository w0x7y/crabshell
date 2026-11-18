//! CPU, memory and temperature stats from procfs and sysfs, whose files
//! change without an inotify event, so they are sampled on a timer that
//! lives only while a config shows them. The reads run on a thread of
//! their own, as some sensors take 100 ms to answer.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::io::Read;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Once, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;

use crate::Shared;
use crate::file::{FileError, FileErrorKind, open_file, set};
use crate::lines::panic_message;

/// The shortest period, so stats can't keep the shell busy.
const MIN_PERIOD: Duration = Duration::from_millis(250);
/// The longest period, as `cx.interval`'s, so adding it to now can't
/// overflow.
const MAX_PERIOD: Duration = Duration::from_secs(u32::MAX as u64);
const MAX_CPUS: usize = 256;
const MAX_TEMPERATURES: usize = 64;
/// The most read from one file.
const MAX_READ: u64 = 64 * 1024;
/// The most entries looked at in one sysfs directory.
const MAX_LISTED: usize = 1024;

/// The system's load, from `cx.system_stats(period)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStats {
  /// The share of time every CPU was busy over the last period, from 0
  /// to 1; 0 until the second sample.
  pub cpu: f32,
  /// Each online CPU's share, as `cpu`, in CPU number order, up to 256
  /// CPUs. A CPU that came online since the last sample shows 0.
  pub cpus: Vec<f32>,
  /// Memory in bytes.
  pub memory_total: u64,
  /// Memory a program could take without swapping, in bytes.
  pub memory_available: u64,
  /// Swap in bytes.
  pub swap_total: u64,
  /// Swap free, in bytes.
  pub swap_free: u64,
  /// Each hwmon sensor's temperature, up to 64, except those of a chip
  /// whose device sleeps. The sensors are listed at the first sample and
  /// again only once one is gone.
  pub temperatures: Vec<Temperature>,
}

/// A temperature sensor's reading, from [`SystemStats::temperatures`].
#[derive(Clone, Debug, PartialEq)]
pub struct Temperature {
  /// The chip's name and the sensor's label, such as `coretemp Core 0`,
  /// or its number, such as `acpitz temp1`, when it has no label.
  pub name: String,
  /// The reading in degrees Celsius.
  pub celsius: f32,
}

/// What `cx.system_stats` needs from the shell: the loop and the roots,
/// and the one thread that samples while any context shows stats.
pub(crate) struct Service {
  shared: Rc<Shared>,
  /// Each live call's signal, with its sampler while no read runs.
  live: RefCell<HashMap<u64, Live>>,
  next: Cell<u64>,
  worker: RefCell<Option<Worker>>,
  /// Whether the worker couldn't start, so samples are read on the loop
  /// thread instead.
  inline: Cell<bool>,
  /// How many samples were asked for, for the tests.
  #[cfg(test)]
  requests: Cell<usize>,
  /// The last worker's thread, once stopped, for the tests to join.
  #[cfg(test)]
  ended: RefCell<Option<JoinHandle<()>>>,
}

struct Live {
  stats: Signal<SystemStats>,
  /// `None` while the worker reads with it.
  sampler: Option<Sampler>,
}

/// A finished sample: whose it is, its sampler back, and what it read.
pub(crate) type Answer = (u64, Sampler, SystemStats);

/// The thread that reads, and the channel it answers on.
struct Worker {
  requests: mpsc::Sender<(u64, Sampler)>,
  token: RegistrationToken,
  /// Detached outside tests, which join it.
  #[cfg_attr(not(test), allow(dead_code))]
  thread: JoinHandle<()>,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      live: RefCell::default(),
      next: Cell::new(0),
      worker: RefCell::new(None),
      inline: Cell::new(false),
      #[cfg(test)]
      requests: Cell::new(0),
      #[cfg(test)]
      ended: RefCell::new(None),
    }
  }

  /// Hands sampler `id` to the worker, starting it if none runs. A
  /// sampler still reading is skipped, not queued.
  fn request(self: &Rc<Self>, id: u64) {
    let taken = self
      .live
      .borrow_mut()
      .get_mut(&id)
      .and_then(|live| live.sampler.take());
    let Some(sampler) = taken else {
      return;
    };
    #[cfg(test)]
    self.requests.set(self.requests.get() + 1);
    if self.worker.borrow().is_none() && !self.inline.get() {
      let worker = self.start();
      self.inline.set(worker.is_none());
      self.worker.replace(worker);
    }
    if self.inline.get() {
      let mut sampler = sampler;
      let stats = sample_caught(&mut sampler);
      return self.answered(id, sampler, stats);
    }
    let unsent = match self.worker.borrow().as_ref() {
      Some(worker) => worker
        .requests
        .send((id, sampler))
        .err()
        .map(|error| error.0.1),
      None => Some(sampler),
    };
    if let Some(sampler) = unsent
      && let Some(live) = self.live.borrow_mut().get_mut(&id)
    {
      live.sampler = Some(sampler);
    }
  }

  /// The worker, answering on a channel in the loop. `None`, logged, if
  /// either couldn't start; samples are read on the loop thread then.
  fn start(self: &Rc<Self>) -> Option<Worker> {
    let (answers, channel) = calloop::channel::channel::<Answer>();
    let weak = Rc::downgrade(self);
    let answered = Box::new(move |(id, sampler, stats)| {
      if let Some(service) = weak.upgrade() {
        service.answered(id, sampler, stats);
      }
    });
    let event_loop = self.shared.event_loop()?;
    let token = event_loop.insert_stats_channel(channel, answered)?;
    let (requests, received) = mpsc::channel::<(u64, Sampler)>();
    let spawned = std::thread::Builder::new()
      .name(String::from("crabshell-stats"))
      .spawn(move || {
        // Ends once the requests' sender or the loop's channel is gone.
        for (id, mut sampler) in received {
          let stats = sample_caught(&mut sampler);
          if answers.send((id, sampler, stats)).is_err() {
            break;
          }
        }
      });
    match spawned {
      Ok(thread) => Some(Worker {
        requests,
        token,
        thread,
      }),
      Err(error) => {
        tracing::warn!(%error, "couldn't start the stats' thread, so they are read on the loop thread");
        event_loop.remove(token);
        None
      }
    }
  }

  fn answered(&self, id: u64, sampler: Sampler, stats: SystemStats) {
    let signal = {
      let mut live = self.live.borrow_mut();
      let Some(live) = live.get_mut(&id) else {
        return;
      };
      live.sampler = Some(sampler);
      live.stats
    };
    set(signal, stats);
  }

  /// Takes the worker out of the loop. Its thread ends once a read it is
  /// in finishes.
  fn stop(&self) {
    let Some(worker) = self.worker.take() else {
      return;
    };
    if let Some(event_loop) = self.shared.event_loop() {
      event_loop.remove(worker.token);
    }
    drop(worker.requests);
    #[cfg(test)]
    self.ended.replace(Some(worker.thread));
  }
}

/// `cx.system_stats(period)`.
pub(crate) fn system_stats(cx: &Cx, period: Duration) -> ReadSignal<SystemStats> {
  let stats = cx.signal(SystemStats::default());
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!(
        "cx.system_stats() needs the services the shell provides; these stats stand still"
      );
    });
    return stats.read_only();
  };
  let Some(event_loop) = service.shared.event_loop() else {
    return stats.read_only();
  };
  let id = service.next.get();
  service.next.set(id + 1);
  let config = &service.shared.config;
  #[cfg_attr(not(test), allow(unused_mut))]
  let mut sampler = Sampler::new(&config.proc, &config.sys);
  #[cfg(test)]
  {
    sampler.delay = config.stats_delay;
    sampler.panic_once = config.stats_panic;
  }
  let live = Live {
    stats,
    sampler: Some(sampler),
  };
  service.live.borrow_mut().insert(id, live);
  service.request(id);
  let period = period.clamp(MIN_PERIOD, MAX_PERIOD);
  let weak = Rc::downgrade(&service);
  let tick = Box::new(move || {
    let Some(service) = weak.upgrade() else {
      return TimeoutAction::Drop;
    };
    service.request(id);
    TimeoutAction::ToDuration(period)
  });
  let timer = event_loop.insert_timer(Instant::now() + period, tick);
  cx.owner().signal(Unschedule { service, id, timer });
  stats.read_only()
}

/// Reads a sample. A bug in the reading gives an empty one, logged once,
/// rather than taking the sampler and freezing the stats.
fn sample_caught(sampler: &mut Sampler) -> SystemStats {
  panic::catch_unwind(AssertUnwindSafe(|| sampler.sample())).unwrap_or_else(|payload| {
    static LOGGED: Once = Once::new();
    let message = panic_message(payload.as_ref());
    LOGGED.call_once(|| tracing::error!("reading the system stats panicked: {message}"));
    SystemStats::default()
  })
}

/// Takes a call's timer out of the loop when its context goes, and the
/// worker with the last one.
struct Unschedule {
  service: Rc<Service>,
  id: u64,
  timer: Option<RegistrationToken>,
}

impl Drop for Unschedule {
  fn drop(&mut self) {
    if let (Some(timer), Some(event_loop)) = (self.timer, self.service.shared.event_loop()) {
      event_loop.remove(timer);
    }
    let empty = {
      let mut live = self.service.live.borrow_mut();
      live.remove(&self.id);
      live.is_empty()
    };
    if empty {
      self.service.stop();
    }
  }
}

/// A CPU's time counters, in clock ticks since boot.
#[derive(Clone, Copy, Default)]
struct Times {
  busy: u64,
  total: u64,
}

/// A hwmon chip's temperature sensors, listed once.
struct Chip {
  /// The chip's device's power state, `suspended` while it sleeps.
  runtime_status: PathBuf,
  /// Each sensor's name and the file holding its reading.
  sensors: Vec<(String, PathBuf)>,
}

/// Reads the stats, keeping the last CPU counters for the next sample's
/// share. It moves to the worker for each read and back with the result.
pub(crate) struct Sampler {
  proc: PathBuf,
  sys: PathBuf,
  /// Every CPU's counters at the last sample, then each CPU's by number.
  last: Option<(Times, Vec<(u32, Times)>)>,
  /// The temperature sensors, listed at the first sample and again after
  /// one was gone.
  chips: Option<Vec<Chip>>,
  /// The problems logged already, by kind or by file.
  logged: HashSet<String>,
  /// A pause before each read, so a test's sensors can be slow.
  #[cfg(test)]
  delay: Duration,
  /// Whether the next read panics, as a bug would.
  #[cfg(test)]
  panic_once: bool,
}

impl Sampler {
  fn new(proc: &Path, sys: &Path) -> Sampler {
    Sampler {
      proc: proc.to_owned(),
      sys: sys.to_owned(),
      last: None,
      chips: None,
      logged: HashSet::new(),
      #[cfg(test)]
      delay: Duration::ZERO,
      #[cfg(test)]
      panic_once: false,
    }
  }

  fn sample(&mut self) -> SystemStats {
    #[cfg(test)]
    {
      std::thread::sleep(self.delay);
      assert!(
        !std::mem::take(&mut self.panic_once),
        "a test's sensor panicked"
      );
    }
    let mut stats = SystemStats::default();
    self.cpu(&mut stats);
    self.memory(&mut stats);
    self.temperatures(&mut stats);
    stats
  }

  /// Logs a problem the first time it comes.
  fn once(&mut self, key: impl Into<String>, detail: impl Display) {
    if self.logged.insert(key.into()) {
      tracing::info!("system stats: {detail}; it is skipped");
    }
  }

  fn read(&mut self, path: &Path) -> Option<String> {
    match read_capped(path) {
      Ok(text) => Some(text),
      Err(error) => {
        let path = path.display().to_string();
        let detail = format!("can't read {path}: {}", error.message);
        self.once(path, detail);
        None
      }
    }
  }

  fn cpu(&mut self, stats: &mut SystemStats) {
    let Some(text) = self.read(&self.proc.join("stat")) else {
      return;
    };
    let mut all = None;
    let mut cpus = Vec::new();
    for line in text.lines() {
      let mut words = line.split_ascii_whitespace();
      let Some(label) = words.next().filter(|label| label.starts_with("cpu")) else {
        continue;
      };
      let number = label[3..].parse::<u32>();
      let Some(times) = parse_times(words).filter(|_| label == "cpu" || number.is_ok()) else {
        let detail = format!("a CPU line doesn't parse: {line:.80}");
        self.once("cpu line", detail);
        continue;
      };
      match number {
        Err(_) => all = Some(times),
        Ok(_) if cpus.len() == MAX_CPUS => {
          self.once("cpus", "there are over 256 CPUs; the rest are left out");
        }
        Ok(number) => cpus.push((number, times)),
      }
    }
    cpus.sort_unstable_by_key(|(number, _)| *number);
    let all = all.unwrap_or_default();
    let last = self.last.take();
    stats.cpu = last.as_ref().map_or(0.0, |(then, _)| share(*then, all));
    // A CPU that came online since starts from 0.
    stats.cpus = cpus
      .iter()
      .map(|(number, now)| {
        let then = last.as_ref().and_then(|(_, then)| {
          let at = then.binary_search_by_key(number, |(number, _)| *number);
          at.ok().map(|at| then[at].1)
        });
        then.map_or(0.0, |then| share(then, *now))
      })
      .collect();
    self.last = Some((all, cpus));
  }

  fn memory(&mut self, stats: &mut SystemStats) {
    let Some(text) = self.read(&self.proc.join("meminfo")) else {
      return;
    };
    for line in text.lines() {
      let Some((key, value)) = line.split_once(':') else {
        continue;
      };
      let field = match key {
        "MemTotal" => &mut stats.memory_total,
        "MemAvailable" => &mut stats.memory_available,
        "SwapTotal" => &mut stats.swap_total,
        "SwapFree" => &mut stats.swap_free,
        _ => continue,
      };
      match parse_bytes(value) {
        Some(bytes) => *field = bytes,
        None => self.once(
          "meminfo line",
          format_args!("a meminfo line doesn't parse: {line:.80}"),
        ),
      }
    }
  }

  /// Reads each listed sensor, except a sleeping chip's, so a sampled
  /// dGPU isn't woken. A sensor gone has the sensors listed again at the
  /// next sample.
  fn temperatures(&mut self, stats: &mut SystemStats) {
    let chips = match self.chips.take() {
      Some(chips) => chips,
      None => self.list_sensors(),
    };
    let mut gone = false;
    for chip in &chips {
      let asleep =
        read_capped(&chip.runtime_status).is_ok_and(|status| status.trim() == "suspended");
      if asleep {
        continue;
      }
      for (name, input) in &chip.sensors {
        let text = match read_capped(input) {
          Ok(text) => text,
          Err(error) if error.kind == FileErrorKind::NotFound => {
            gone = true;
            continue;
          }
          Err(error) => {
            let path = input.display().to_string();
            let detail = format!("can't read {path}: {}", error.message);
            self.once(path, detail);
            continue;
          }
        };
        match text.trim().parse::<i64>() {
          Ok(millidegrees) => stats.temperatures.push(Temperature {
            name: name.clone(),
            celsius: millidegrees as f32 / 1000.0,
          }),
          Err(_) => {
            let path = input.display().to_string();
            let detail = format!("{path} doesn't hold a temperature");
            self.once(path, detail);
          }
        }
      }
    }
    self.chips = (!gone).then_some(chips);
  }

  /// The hwmon chips and their temperature sensors, named by the chip's
  /// name and each sensor's label, in number order, up to
  /// [`MAX_TEMPERATURES`] sensors.
  fn list_sensors(&mut self) -> Vec<Chip> {
    let class = self.sys.join("class/hwmon");
    let Some(mut numbered) = self.numbered(&class, "hwmon", "") else {
      return Vec::new();
    };
    numbered.sort_unstable();
    let mut chips = Vec::new();
    let mut count = 0;
    for (number, dir) in numbered {
      let name = read_capped(&dir.join("name"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("hwmon{number}"));
      let Some(mut inputs) = self.numbered(&dir, "temp", "_input") else {
        continue;
      };
      inputs.sort_unstable();
      let mut sensors = Vec::new();
      for (sensor, input) in inputs {
        if count == MAX_TEMPERATURES {
          self.once(
            "temperatures",
            "there are over 64 temperatures; the rest are left out",
          );
          break;
        }
        let label = read_capped(&dir.join(format!("temp{sensor}_label")))
          .ok()
          .map(|label| label.trim().to_owned())
          .filter(|label| !label.is_empty())
          .unwrap_or_else(|| format!("temp{sensor}"));
        sensors.push((format!("{name} {label}"), input));
        count += 1;
      }
      chips.push(Chip {
        runtime_status: dir.join("device/power/runtime_status"),
        sensors,
      });
    }
    chips
  }

  /// The entries in `dir` named `<prefix><number><suffix>`, with their
  /// numbers, among its first [`MAX_LISTED`].
  fn numbered(&mut self, dir: &Path, prefix: &str, suffix: &str) -> Option<Vec<(u32, PathBuf)>> {
    let entries = match std::fs::read_dir(dir) {
      Ok(entries) => entries,
      Err(error) => {
        self.once(
          "list",
          format_args!("can't list {}: {error}", dir.display()),
        );
        return None;
      }
    };
    let numbered = entries
      .take(MAX_LISTED)
      .filter_map(|entry| {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let number = name.to_str()?.strip_prefix(prefix)?.strip_suffix(suffix)?;
        Some((number.parse().ok()?, entry.path()))
      })
      .collect();
    Some(numbered)
  }
}

/// A `/proc/stat` CPU line's counters after its label: user, nice,
/// system, idle, then optionally iowait, irq, softirq and steal. Guest
/// time is in user's already, so it is left out.
fn parse_times<'a>(words: impl Iterator<Item = &'a str>) -> Option<Times> {
  let counters: Vec<u64> = words
    .take(8)
    .map(str::parse)
    .collect::<Result<_, _>>()
    .ok()?;
  let idle = counters.get(3)? + counters.get(4).unwrap_or(&0);
  let total = counters
    .iter()
    .try_fold(0_u64, |sum, n| sum.checked_add(*n))?;
  Some(Times {
    busy: total.checked_sub(idle)?,
    total,
  })
}

/// The share of `now`'s time since `then` that was busy, from 0 to 1.
fn share(then: Times, now: Times) -> f32 {
  let total = now.total.saturating_sub(then.total);
  if total == 0 {
    return 0.0;
  }
  // iowait may go back, so busy may move more than total.
  (now.busy.saturating_sub(then.busy) as f64 / total as f64).clamp(0.0, 1.0) as f32
}

/// A meminfo value, such as `  16318060 kB`, in bytes.
fn parse_bytes(value: &str) -> Option<u64> {
  let mut words = value.split_ascii_whitespace();
  let number: u64 = words.next()?.parse().ok()?;
  match (words.next(), words.next()) {
    (Some("kB"), None) => number.checked_mul(1024),
    (None, None) => Some(number),
    _ => None,
  }
}

/// Up to [`MAX_READ`] of the file at `path`, which must be a regular
/// file, as procfs's and sysfs's are.
fn read_capped(path: &Path) -> Result<String, FileError> {
  let file = open_file(path)?;
  let mut bytes = Vec::new();
  file.take(MAX_READ).read_to_end(&mut bytes)?;
  Ok(String::from_utf8_lossy(&bytes).into_owned())
}
