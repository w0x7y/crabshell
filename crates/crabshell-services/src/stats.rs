//! CPU, memory and temperature stats from procfs and sysfs, whose files
//! change without an inotify event, so they are sampled on a timer that
//! lives only while a config shows them.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fmt::Display;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;
use crabshell_reactive::ReadSignal;
use crabshell_ui::Cx;

use crate::Shared;
use crate::file::{open_file, set};

/// The shortest period, so stats can't keep the shell busy.
const MIN_PERIOD: Duration = Duration::from_millis(250);
const MAX_CPUS: usize = 256;
const MAX_TEMPERATURES: usize = 64;
/// The most read from one file.
const MAX_READ: u64 = 64 * 1024;
/// The most entries looked at in one sysfs directory.
const MAX_LISTED: usize = 1024;

/// What `cx.system_stats` needs from the shell: the loop and the roots.
pub(crate) struct Service(pub(crate) Rc<Shared>);

/// The system's load, from `cx.system_stats(period)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStats {
  /// The share of time every CPU was busy over the last period, from 0
  /// to 1; 0 until the second sample.
  pub cpu: f32,
  /// Each CPU's share, as `cpu`, up to 256 CPUs.
  pub cpus: Vec<f32>,
  /// Memory in bytes.
  pub memory_total: u64,
  /// Memory a program could take without swapping, in bytes.
  pub memory_available: u64,
  /// Swap in bytes.
  pub swap_total: u64,
  /// Swap free, in bytes.
  pub swap_free: u64,
  /// Each hwmon sensor's temperature, up to 64.
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

/// `cx.system_stats(period)`.
pub(crate) fn system_stats(cx: &Cx, period: Duration) -> ReadSignal<SystemStats> {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!(
        "cx.system_stats() needs the services the shell provides; these stats stand still"
      );
    });
    return cx.signal(SystemStats::default()).read_only();
  };
  let shared = Rc::clone(&service.0);
  let mut sampler = Sampler::new(&shared.config.proc, &shared.config.sys);
  let stats = cx.signal(sampler.sample());
  let period = period.max(MIN_PERIOD);
  let tick = Box::new(move || {
    if stats.try_with(|_| ()).is_none() {
      return TimeoutAction::Drop;
    }
    set(stats, sampler.sample());
    TimeoutAction::ToDuration(period)
  });
  let token = shared
    .event_loop()
    .and_then(|event_loop| event_loop.insert_timer(Instant::now() + period, tick));
  if let Some(token) = token {
    cx.owner().signal(Unschedule(shared, token));
  }
  stats.read_only()
}

/// Takes the stats' timer out of the loop when its context goes.
struct Unschedule(Rc<Shared>, RegistrationToken);

impl Drop for Unschedule {
  fn drop(&mut self) {
    if let Some(event_loop) = self.0.event_loop() {
      event_loop.remove(self.1);
    }
  }
}

/// A CPU's time counters, in clock ticks since boot.
#[derive(Clone, Copy, Default)]
struct Times {
  busy: u64,
  total: u64,
}

/// Reads the stats, keeping the last CPU counters for the next sample's
/// share.
struct Sampler {
  proc: PathBuf,
  sys: PathBuf,
  /// Every CPU's counters at the last sample, then each CPU's.
  last: Option<(Times, Vec<Times>)>,
  /// The kinds of problem logged already.
  logged: HashSet<&'static str>,
}

impl Sampler {
  fn new(proc: &Path, sys: &Path) -> Sampler {
    Sampler {
      proc: proc.to_owned(),
      sys: sys.to_owned(),
      last: None,
      logged: HashSet::new(),
    }
  }

  fn sample(&mut self) -> SystemStats {
    let mut stats = SystemStats::default();
    self.cpu(&mut stats);
    self.memory(&mut stats);
    self.temperatures(&mut stats);
    stats
  }

  /// Logs a kind of problem the first time it comes.
  fn once(&mut self, kind: &'static str, detail: impl Display) {
    if self.logged.insert(kind) {
      tracing::info!("system stats: {detail}; it is skipped");
    }
  }

  fn read(&mut self, path: &Path) -> Option<String> {
    match read_capped(path) {
      Ok(text) => Some(text),
      Err(error) => {
        self.once(
          "read",
          format_args!("can't read {}: {error}", path.display()),
        );
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
      let Some(times) = parse_times(words) else {
        self.once(
          "cpu line",
          format_args!("a CPU line doesn't parse: {line:.80}"),
        );
        continue;
      };
      if label == "cpu" {
        all = Some(times);
      } else if label[3..].parse::<u32>().is_err() {
        self.once(
          "cpu line",
          format_args!("a CPU line doesn't parse: {line:.80}"),
        );
      } else if cpus.len() == MAX_CPUS {
        self.once("cpus", "there are over 256 CPUs; the rest are left out");
      } else {
        cpus.push(times);
      }
    }
    let all = all.unwrap_or_default();
    if let Some((last_all, last_cpus)) = &self.last {
      stats.cpu = share(*last_all, all);
      // A CPU that came or went since starts again from 0.
      if last_cpus.len() == cpus.len() {
        stats.cpus = last_cpus
          .iter()
          .zip(&cpus)
          .map(|(a, b)| share(*a, *b))
          .collect();
      }
    }
    if stats.cpus.is_empty() {
      stats.cpus = vec![0.0; cpus.len()];
    }
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

  fn temperatures(&mut self, stats: &mut SystemStats) {
    let class = self.sys.join("class/hwmon");
    let Some(mut chips) = self.numbered(&class, "hwmon", "") else {
      return;
    };
    chips.sort_unstable();
    for (chip, dir) in chips {
      let name = read_capped(&dir.join("name"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("hwmon{chip}"));
      let Some(mut sensors) = self.numbered(&dir, "temp", "_input") else {
        continue;
      };
      sensors.sort_unstable();
      for (sensor, input) in sensors {
        let Some(text) = self.read(&input) else {
          continue;
        };
        let Ok(millidegrees) = text.trim().parse::<i64>() else {
          let path = input.display();
          self.once(
            "temperature",
            format_args!("{path} doesn't hold a temperature"),
          );
          continue;
        };
        if stats.temperatures.len() == MAX_TEMPERATURES {
          self.once(
            "temperatures",
            "there are over 64 temperatures; the rest are left out",
          );
          return;
        }
        let label = read_capped(&dir.join(format!("temp{sensor}_label")))
          .ok()
          .map(|label| label.trim().to_owned())
          .filter(|label| !label.is_empty())
          .unwrap_or_else(|| format!("temp{sensor}"));
        stats.temperatures.push(Temperature {
          name: format!("{name} {label}"),
          celsius: millidegrees as f32 / 1000.0,
        });
      }
    }
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
fn read_capped(path: &Path) -> Result<String, String> {
  let file = open_file(path).map_err(|error| error.message)?;
  let mut bytes = Vec::new();
  file
    .take(MAX_READ)
    .read_to_end(&mut bytes)
    .map_err(|error| error.to_string())?;
  Ok(String::from_utf8_lossy(&bytes).into_owned())
}
