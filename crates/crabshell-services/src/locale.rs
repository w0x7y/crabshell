//! Month and day names for the user's locale, from glibc's own data:
//! `locale -k LC_TIME`, run once on a thread of its own.

use std::future::poll_fn;
use std::io::Read;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Poll, Waker};

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use jiff::Zoned;
use jiff::civil::{Date, Weekday};

use crate::Shared;

#[cfg(test)]
mod tests;

/// The most `locale` may print. glibc's `LC_TIME` is about 2 KiB.
const MAX_OUTPUT: u64 = 64 * 1024;

/// The names dates are written with in a locale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale {
  /// January first, as `%B` writes them.
  pub months: [String; 12],
  /// January first, as `%b` writes them.
  pub months_short: [String; 12],
  /// Monday first, as `%A` writes them.
  pub weekdays: [String; 7],
  /// Monday first, as `%a` writes them.
  pub weekdays_short: [String; 7],
  /// Before and after noon, as `%p` writes them. Empty in a locale that
  /// writes 24-hour time.
  pub am_pm: [String; 2],
  /// The day a calendar's week starts on.
  pub first_weekday: Weekday,
}

impl Default for Locale {
  /// English, as the C locale writes it.
  fn default() -> Locale {
    let names = |list: &str| list.split(';').map(str::to_owned).collect::<Vec<_>>();
    Locale {
      months: array(names(
        "January;February;March;April;May;June;July;August;September;October;November;December",
      )),
      months_short: array(names("Jan;Feb;Mar;Apr;May;Jun;Jul;Aug;Sep;Oct;Nov;Dec")),
      weekdays: array(names(
        "Monday;Tuesday;Wednesday;Thursday;Friday;Saturday;Sunday",
      )),
      weekdays_short: array(names("Mon;Tue;Wed;Thu;Fri;Sat;Sun")),
      am_pm: array(names("AM;PM")),
      first_weekday: Weekday::Sunday,
    }
  }
}

impl Locale {
  /// `time` written as `format` says, as jiff's `strftime` does, with
  /// `%a`, `%A`, `%b`, `%h`, `%B` and `%p` in this locale. A format jiff
  /// can't write is logged and gives an empty string.
  pub fn strftime(&self, time: &Zoned, format: &str) -> String {
    let mut filled = String::with_capacity(format.len());
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
      if c != '%' {
        filled.push(c);
        continue;
      }
      let weekday = time.weekday().to_monday_zero_offset() as usize;
      let month = (time.month() - 1) as usize;
      let name = match chars.next() {
        Some('a') => &self.weekdays_short[weekday],
        Some('A') => &self.weekdays[weekday],
        Some('b' | 'h') => &self.months_short[month],
        Some('B') => &self.months[month],
        Some('p') => &self.am_pm[usize::from(time.hour() >= 12)],
        other => {
          filled.push('%');
          filled.extend(other);
          continue;
        }
      };
      // Names go to jiff as literal text.
      filled.push_str(&name.replace('%', "%%"));
    }
    jiff::fmt::strtime::format(filled, time).unwrap_or_else(|error| {
      tracing::warn!(format, %error, "couldn't write a time");
      String::new()
    })
  }

  /// The names in what `locale -k LC_TIME` printed, English for any it
  /// left out or that don't parse.
  pub(crate) fn parse(output: &str) -> Locale {
    let mut locale = Locale::default();
    let (mut start, mut first) = (Weekday::Sunday, 1_i64);
    for line in output.lines() {
      let Some((key, value)) = line.split_once('=') else {
        continue;
      };
      let value = value.trim_matches('"');
      let names = || value.split(';').map(str::to_owned).collect::<Vec<_>>();
      let sunday_first = |names: Vec<String>| (names.len() == 7).then(|| monday_first(names));
      match key {
        "mon" => locale.months = array_or(names(), locale.months),
        "abmon" => locale.months_short = array_or(names(), locale.months_short),
        "day" => locale.weekdays = sunday_first(names()).unwrap_or(locale.weekdays),
        "abday" => locale.weekdays_short = sunday_first(names()).unwrap_or(locale.weekdays_short),
        "am_pm" => locale.am_pm = array_or(names(), locale.am_pm),
        // glibc counts the first weekday from 1, the weekday of the date
        // `week-1stday` names.
        "week-1stday" => {
          start = value
            .parse::<i64>()
            .ok()
            .and_then(|day| {
              let part = |divisor: i64| i8::try_from(day / divisor % 100).ok();
              Date::new(i16::try_from(day / 10_000).ok()?, part(100)?, part(1)?).ok()
            })
            .map_or(Weekday::Sunday, |date| date.weekday());
        }
        "first_weekday" => first = value.parse().unwrap_or(1).clamp(1, 7),
        _ => {}
      }
    }
    locale.first_weekday = start.wrapping_add(first - 1);
    locale
  }
}

/// Seven names from Sunday, from Monday.
fn monday_first(mut names: Vec<String>) -> [String; 7] {
  names.rotate_left(1);
  array(names)
}

/// `names` as an array, or `or` if there are more or fewer.
fn array_or<const N: usize>(names: Vec<String>, or: [String; N]) -> [String; N] {
  names.try_into().unwrap_or(or)
}

fn array<const N: usize>(names: Vec<String>) -> [String; N] {
  names.try_into().expect("a built-in list has its length")
}

/// The user's locale for dates, shared by every caller.
struct Shown(Signal<Locale>);

/// `cx.locale()`.
pub(crate) fn locale(cx: &Cx) -> ReadSignal<Locale> {
  if let Some(shown) = cx.service::<Shown>() {
    return shown.0.read_only();
  }
  let var = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
  let name = var("LC_ALL")
    .or_else(|| var("LC_TIME"))
    .or_else(|| var("LANG"));
  let shared = cx
    .service::<crate::process::Processes>()
    .map(|processes| Rc::clone(&processes.shared));
  let signal = match &shared {
    Some(shared) => shared.root.signal(Locale::default()),
    None => cx.signal(Locale::default()),
  };
  cx.provide(Shown(signal));
  if let (Some(shared), Some(name)) = (shared, name) {
    read(&shared, name, signal);
  }
  signal.read_only()
}

/// What `locale` printed, once its thread is done, and who waits for it.
type Answer = (Option<Result<String, String>>, Option<Waker>);

/// Sets `signal` to locale `name`'s names, read on a thread of its own.
/// C and POSIX stay English, and so does a locale `locale` can't read,
/// which is logged.
pub(crate) fn read(shared: &Shared, name: String, signal: Signal<Locale>) {
  let base = name.split(['.', '@']).next().unwrap_or_default();
  if base == "C" || base == "POSIX" {
    return;
  }
  let answer: Arc<Mutex<Answer>> = Arc::default();
  let filled = Arc::clone(&answer);
  let spawned = std::thread::Builder::new()
    .name(String::from("crabshell-locale"))
    .spawn(move || {
      let output = run_locale(&name);
      let mut answer = filled.lock().unwrap_or_else(PoisonError::into_inner);
      answer.0 = Some(output);
      if let Some(waker) = answer.1.take() {
        waker.wake();
      }
    });
  if let Err(error) = spawned {
    return tracing::warn!(%error, "couldn't read the locale's names, so dates are in English");
  }
  shared.spawn(async move {
    let output = poll_fn(|context| {
      let mut answer = answer.lock().unwrap_or_else(PoisonError::into_inner);
      match answer.0.take() {
        Some(output) => Poll::Ready(output),
        None => {
          answer.1 = Some(context.waker().clone());
          Poll::Pending
        }
      }
    })
    .await;
    match output {
      Ok(output) => {
        signal.try_set(Locale::parse(&output));
      }
      Err(why) => tracing::warn!(
        why,
        "couldn't read the locale's names, so dates are in English"
      ),
    }
  });
}

/// What `locale -k LC_TIME` prints with `LC_ALL` set to `name`.
fn run_locale(name: &str) -> Result<String, String> {
  let mut child = Command::new("locale")
    .args(["-k", "LC_TIME"])
    .env("LC_ALL", name)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|error| format!("locale didn't start: {error}"))?;
  let mut output = String::new();
  let read = child
    .stdout
    .take()
    .expect("stdout is piped")
    .take(MAX_OUTPUT + 1)
    .read_to_string(&mut output);
  if read.is_err() || output.len() as u64 > MAX_OUTPUT {
    let _ = child.kill();
  }
  // glibc warns here when the locale isn't installed, and prints C's.
  let mut warning = String::new();
  if let Some(stderr) = child.stderr.take() {
    let _ = stderr.take(4096).read_to_string(&mut warning);
  }
  let status = child.wait().map_err(|error| error.to_string())?;
  if let Some(line) = warning.lines().next() {
    return Err(line.to_owned());
  }
  read.map_err(|error| format!("locale printed something unreadable: {error}"))?;
  if output.len() as u64 > MAX_OUTPUT {
    return Err("locale printed over 64 KiB".into());
  }
  if !status.success() {
    return Err(format!("locale failed: {status}"));
  }
  Ok(output)
}
