use std::time::{Duration, Instant};

use calloop::EventLoop;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use jiff::Zoned;
use jiff::civil::Weekday;

use super::Locale;
use crate::testing::capture_logs;

/// 2026-03-02, a Monday, at 14:05 UTC.
fn monday_afternoon() -> Zoned {
  "2026-03-02T14:05:00+00:00[UTC]"
    .parse()
    .expect("a valid time")
}

/// What glibc 2.43's `locale -k LC_TIME` prints for `de_DE.UTF-8`, in
/// part.
const GERMAN: &str = r#"abday="So;Mo;Di;Mi;Do;Fr;Sa"
day="Sonntag;Montag;Dienstag;Mittwoch;Donnerstag;Freitag;Samstag"
abmon="Jan;Feb;Mär;Apr;Mai;Jun;Jul;Aug;Sep;Okt;Nov;Dez"
mon="Januar;Februar;März;April;Mai;Juni;Juli;August;September;Oktober;November;Dezember"
am_pm=";"
d_t_fmt="%a %d %b %Y %T %Z"
week-ndays=7
week-1stday=19971130
week-1stweek=4
first_weekday=2
alt_mon="Januar;Februar;März;April;Mai;Juni;Juli;August;September;Oktober;November;Dezember"
"#;

/// What glibc 2.43's `locale -k LC_TIME` prints for `pl_PL.UTF-8`, in
/// part: a month inside a date is genitive.
const POLISH: &str = r#"mon="stycznia;lutego;marca;kwietnia;maja;czerwca;lipca;sierpnia;września;października;listopada;grudnia"
alt_mon="styczeń;luty;marzec;kwiecień;maj;czerwiec;lipiec;sierpień;wrzesień;październik;listopad;grudzień"
"#;

#[test]
fn english_is_the_default() {
  let locale = Locale::default();
  assert_eq!(locale.months[0], "January");
  assert_eq!(locale.months_short[11], "Dec");
  assert_eq!(locale.weekdays[0], "Monday");
  assert_eq!(locale.weekdays_short[6], "Sun");
  assert_eq!(locale.am_pm, ["AM".to_owned(), "PM".to_owned()]);
  assert_eq!(locale.first_weekday, Weekday::Sunday);
}

#[test]
fn german_has_its_own_names_and_starts_on_monday() {
  let locale = Locale::parse(GERMAN);
  assert_eq!(locale.months[2], "März");
  assert_eq!(locale.months_short[9], "Okt");
  assert_eq!(locale.weekdays[0], "Montag");
  assert_eq!(locale.weekdays[6], "Sonntag");
  assert_eq!(locale.weekdays_short[2], "Mi");
  assert_eq!(locale.am_pm, [String::new(), String::new()]);
  assert_eq!(locale.first_weekday, Weekday::Monday);
}

#[test]
fn standalone_months_come_from_alt_mon_or_else_mon() {
  let polish = Locale::parse(POLISH);
  assert_eq!(polish.months[2], "marca");
  assert_eq!(polish.months_standalone[2], "marzec");
  let without = Locale::parse("mon=\"a;b;c;d;e;f;g;h;i;j;k;l\"");
  assert_eq!(without.months_standalone, without.months);
  assert_eq!(Locale::default().months_standalone[0], "January");
}

#[test]
fn flags_and_a_width_before_a_name_keep_the_locale() {
  let time = monday_afternoon();
  let german = Locale::parse(GERMAN);
  assert_eq!(
    german.strftime(&time, "%^a %-B %10A %_b %^B"),
    "MO März Montag Mär MÄRZ"
  );
  let english = Locale::default();
  assert_eq!(english.strftime(&time, "%#p %^p %#A"), "pm PM MONDAY");
  // Other conversions keep their flags for jiff.
  assert_eq!(english.strftime(&time, "%-d %^a %5Y"), "2 MON 02026");
}

#[test]
fn the_first_weekday_counts_from_week_1stday() {
  let first = |output: &str| Locale::parse(output).first_weekday;
  // A week counted from a Monday, 1997-12-01, starting on its first day.
  assert_eq!(
    first("week-1stday=19971201\nfirst_weekday=1"),
    Weekday::Monday
  );
  assert_eq!(
    first("week-1stday=19971130\nfirst_weekday=7"),
    Weekday::Saturday
  );
  assert_eq!(first("first_weekday=2"), Weekday::Monday);
  // Out of range or not a date: from Sunday, clamped.
  assert_eq!(
    first("week-1stday=nope\nfirst_weekday=9"),
    Weekday::Saturday
  );
}

#[test]
fn a_list_of_the_wrong_length_keeps_english() {
  let locale = Locale::parse("mon=\"a;b\"\nday=\"x\"\nabmon\ngarbage");
  assert_eq!(locale, Locale::default());
}

#[test]
fn strftime_fills_the_names_from_the_locale() {
  let time = monday_afternoon();
  let format = "%a %A %b %h %B %p %%a %d %Y";
  assert_eq!(
    Locale::default().strftime(&time, format),
    "Mon Monday Mar Mar March PM %a 02 2026"
  );
  assert_eq!(
    Locale::parse(GERMAN).strftime(&time, format),
    "Mo Montag Mär Mär März  %a 02 2026"
  );
  let morning: Zoned = "2026-03-02T09:00:00+00:00[UTC]".parse().expect("a time");
  assert_eq!(Locale::default().strftime(&morning, "%I %p"), "09 AM");
}

#[test]
fn a_format_jiff_cant_write_is_logged_and_empty() {
  let mut written = None;
  let logs = capture_logs(|| written = Some(Locale::default().strftime(&monday_afternoon(), "%!")));
  assert_eq!(written.as_deref(), Some(""));
  assert!(logs.contains("couldn't write a time"), "{logs}");
}

/// Reads locale `name` as the service does, into a signal holding a blank
/// month, running the loop until the signal changes or `wait` passes.
/// The signal's value then, the logs, and how many times the loop woke in
/// the second after.
fn read(name: &str, wait: Duration) -> (Locale, String, usize) {
  let mut event_loop: EventLoop<()> = EventLoop::try_new().expect("an event loop");
  let cx = Cx::new(Runtime::current().new_owner());
  crate::provide_with(&cx, &event_loop.handle(), crate::testing::offline());
  let processes = cx.service::<crate::process::Processes>().expect("provided");
  let mut blank = Locale::default();
  blank.months[0].clear();
  let signal = cx.signal(blank.clone());
  let logs = capture_logs(|| {
    super::read(&processes.shared, name.to_owned(), signal);
    let deadline = Instant::now() + wait;
    while signal.get_untracked() == blank && Instant::now() < deadline {
      event_loop
        .dispatch(Some(Duration::from_millis(50)), &mut ())
        .expect("the loop runs");
    }
  });
  let deadline = Instant::now() + Duration::from_secs(1);
  let mut woke = 0;
  while let Some(left) = deadline.checked_duration_since(Instant::now()) {
    event_loop
      .dispatch(Some(left), &mut ())
      .expect("the loop runs");
    woke += 1;
  }
  (signal.get_untracked(), logs, woke)
}

/// Whether glibc has locale `name` here, as `locale -a` lists it.
fn installed(name: &str) -> bool {
  let output = std::process::Command::new("locale").arg("-a").output();
  output.is_ok_and(|output| {
    String::from_utf8_lossy(&output.stdout)
      .lines()
      .any(|l| l == name)
  })
}

#[test]
fn an_installed_locale_is_read_from_glibc() {
  if !installed("de_DE.utf8") {
    eprintln!("skipped: de_DE.UTF-8 isn't installed here");
    return;
  }
  let (locale, _, _) = read("de_DE.UTF-8", Duration::from_secs(5));
  assert_eq!(locale, Locale::parse(GERMAN));
}

#[test]
fn us_english_is_read_from_glibc() {
  if !installed("en_US.utf8") {
    eprintln!("skipped: en_US.UTF-8 isn't installed here");
    return;
  }
  let (locale, logs, woke) = read("en_US.UTF-8", Duration::from_secs(5));
  assert_eq!(locale, Locale::default(), "{logs}");
  // Once read, nothing wakes the loop: one wakeup, the one that ends it.
  assert_eq!(woke, 1);
}

#[test]
fn a_locale_that_isnt_installed_stays_english_and_is_logged() {
  let (locale, logs, woke) = read("xx_YY.UTF-8", Duration::from_secs(1));
  assert_eq!(woke, 1);
  assert_eq!(locale.months[0], "", "never set");
  assert!(logs.contains("dates are in English"), "{logs}");
}

#[test]
fn c_and_posix_start_nothing() {
  for name in ["C", "C.UTF-8", "POSIX"] {
    let (locale, logs, _) = read(name, Duration::from_millis(200));
    assert_eq!(locale.months[0], "", "never set");
    assert!(logs.is_empty(), "{logs}");
  }
}
