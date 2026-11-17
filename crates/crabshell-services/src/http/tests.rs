use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{Http, HttpErrorKind};
use crate::testing::{Harness, TempDir, capture_logs, offline};
use crate::{Config, ServicesExt};

/// The first bytes of a PNG, all `cx.cover_art` looks at.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really the rest";

/// A local HTTP server, one thread per request, counting hits per path.
struct Server {
  addr: SocketAddr,
  hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Server {
  fn start() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let addr = listener.local_addr().expect("its address");
    let hits = Arc::new(Mutex::new(HashMap::new()));
    let counted = Arc::clone(&hits);
    std::thread::spawn(move || {
      for stream in listener.incoming().flatten() {
        let hits = Arc::clone(&counted);
        std::thread::spawn(move || serve(stream, &hits));
      }
    });
    Server { addr, hits }
  }

  fn url(&self, path: &str) -> String {
    format!("http://{}{path}", self.addr)
  }

  fn hits(&self, path: &str) -> usize {
    self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
  }
}

fn serve(mut stream: TcpStream, hits: &Mutex<HashMap<String, usize>>) {
  let mut reader = BufReader::new(stream.try_clone().expect("the stream"));
  let mut line = String::new();
  let _ = reader.read_line(&mut line);
  let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
  loop {
    let mut header = String::new();
    if reader.read_line(&mut header).unwrap_or(0) <= 2 {
      break;
    }
  }
  *hits.lock().unwrap().entry(path.clone()).or_default() += 1;
  let (status, extra, body): (&str, &str, Vec<u8>) = match path.as_str() {
    "/body" => ("200 OK", "", b"hello".to_vec()),
    "/404" => ("404 Not Found", "", b"missing".to_vec()),
    "/redirect" => ("302 Found", "Location: /body\r\n", Vec::new()),
    "/to-file" => (
      "302 Found",
      "Location: file:///etc/hostname\r\n",
      Vec::new(),
    ),
    "/bytes" => ("200 OK", "", vec![0xff, 0xfe, 0xfd]),
    "/art.png" | "/other.png" => ("200 OK", "", PNG.to_vec()),
    "/text-art" => ("200 OK", "", b"not a picture".to_vec()),
    "/slow" | "/slow.png" => {
      std::thread::sleep(Duration::from_secs(3));
      ("200 OK", "", PNG.to_vec())
    }
    "/big" => {
      // No length, so only the bytes counted can stop it.
      let head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
      let _ = stream.write_all(head.as_bytes());
      let chunk = vec![b'a'; 64 * 1024];
      for _ in 0..64 {
        if stream.write_all(&chunk).is_err() {
          return;
        }
      }
      return;
    }
    _ => ("404 Not Found", "", Vec::new()),
  };
  let length = body.len();
  let head =
    format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {length}\r\n{extra}\r\n");
  let _ = stream.write_all(head.as_bytes());
  let _ = stream.write_all(&body);
}

fn curl_installed() -> bool {
  let found = std::process::Command::new("curl")
    .arg("--version")
    .output()
    .is_ok();
  if !found {
    eprintln!("curl isn't installed; skipping");
  }
  found
}

/// Runs the loop until `done`, for at most 5 s.
fn run_until(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  let give_up = Instant::now() + Duration::from_secs(5);
  Runtime::current().flush();
  while !done() {
    assert!(Instant::now() < give_up, "no {what} after 5 s");
    shell.run_until(Instant::now() + Duration::from_millis(10));
  }
}

fn finish(shell: &mut Harness, http: Http) {
  run_until(shell, "end", move || !http.loading().get_untracked());
}

fn kind(http: Http) -> Option<HttpErrorKind> {
  http.error().get_untracked().map(|error| error.kind)
}

/// A `curl` that is a script of the test's own.
fn fake_curl(dir: &TempDir, script: &str) -> Config {
  use std::os::unix::fs::PermissionsExt;
  let path = dir.path().join("curl");
  std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("the script");
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("executable");
  Config {
    curl: path,
    ..offline()
  }
}

#[test]
fn a_body_arrives_and_loading_ends() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let mut shell = Harness::new(offline());
  let http = shell.cx.http_get(server.url("/body"));
  Runtime::current().flush();
  assert!(http.loading().get_untracked(), "loading once it starts");
  finish(&mut shell, http);
  assert_eq!(http.body().get_untracked().as_deref(), Some("hello"));
  assert_eq!(http.error().get_untracked(), None);

  // Finished, it wakes nothing.
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(wakeups, 1, "asleep: only the run's own deadline");
}

#[test]
fn a_redirect_is_followed() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let mut shell = Harness::new(offline());
  let http = shell.cx.http_get(server.url("/redirect"));
  run_until(&mut shell, "body", move || {
    http.body().get_untracked().is_some()
  });
  assert_eq!(http.body().get_untracked().as_deref(), Some("hello"));
}

#[test]
fn a_new_url_fetches_again_and_a_failure_keeps_the_body() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let mut shell = Harness::new(offline());
  let path = shell.cx.signal("/body");
  let url = server.url("");
  let http = shell.cx.http_get(move || format!("{url}{}", path.get()));
  run_until(&mut shell, "body", move || {
    http.body().get_untracked().is_some()
  });

  path.set("/404");
  run_until(&mut shell, "error", move || {
    http.error().get_untracked().is_some()
  });
  let error = http.error().get_untracked().unwrap();
  assert_eq!(error.kind, HttpErrorKind::Status(404));
  assert!(error.message.contains("404"), "{}", error.message);
  assert_eq!(http.body().get_untracked().as_deref(), Some("hello"));

  path.set("/body");
  run_until(&mut shell, "a fetch that works", move || {
    http.error().get_untracked().is_none()
  });
  assert_eq!(server.hits("/body"), 2);
}

#[test]
fn each_failure_has_its_kind() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let mut shell = Harness::new(offline());
  let slow = shell
    .cx
    .http_get(server.url("/slow"))
    .timeout(Duration::from_millis(300));
  let big = shell.cx.http_get(server.url("/big")).max_size(1024 * 1024);
  let bytes = shell.cx.http_get(server.url("/bytes"));
  let to_file = shell.cx.http_get(server.url("/to-file"));
  let closed = shell.cx.http_get("http://127.0.0.1:1/");
  for http in [slow, big, bytes, to_file, closed] {
    run_until(&mut shell, "error", move || {
      http.error().get_untracked().is_some()
    });
  }
  assert_eq!(kind(slow), Some(HttpErrorKind::TimedOut));
  assert_eq!(kind(big), Some(HttpErrorKind::TooLarge));
  assert_eq!(big.body().get_untracked(), None);
  assert_eq!(kind(bytes), Some(HttpErrorKind::NotUtf8));
  assert_eq!(kind(to_file), Some(HttpErrorKind::Refused));
  assert_eq!(kind(closed), Some(HttpErrorKind::Network));
}

#[test]
fn a_file_url_is_refused_without_starting_curl() {
  let dir = TempDir::new();
  let ran = dir.path().join("ran");
  let config = fake_curl(&dir, &format!("touch '{}'", ran.display()));
  let mut shell = Harness::new(config);
  let http = shell.cx.http_get("file:///etc/passwd");
  Runtime::current().flush();
  assert_eq!(kind(http), Some(HttpErrorKind::Refused));
  assert!(!http.loading().get_untracked());
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert!(!ran.exists(), "curl never ran");
}

#[test]
fn without_curl_it_says_so() {
  let config = Config {
    curl: PathBuf::from("/nonexistent/crabshell-curl"),
    ..offline()
  };
  let shell = Harness::new(config);
  let http = shell.cx.http_get("https://example.com/");
  Runtime::current().flush();
  assert_eq!(kind(http), Some(HttpErrorKind::NoCurl));
  assert!(!http.loading().get_untracked());
}

#[test]
fn a_body_over_the_cap_stops_curl_even_if_curl_wont() {
  let dir = TempDir::new();
  let pid = dir.path().join("pid");
  // A curl that ignores --max-filesize.
  let config = fake_curl(&dir, &format!("echo $$ > '{}'; exec yes", pid.display()));
  let mut shell = Harness::new(config);
  let http = shell.cx.http_get("https://example.com/").max_size(1000);
  finish(&mut shell, http);
  assert_eq!(kind(http), Some(HttpErrorKind::TooLarge));
  let pid = std::fs::read_to_string(&pid).expect("its pid");
  let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
  run_until(&mut shell, "curl reaped", move || !proc.exists());
}

#[test]
fn the_context_going_stops_curl() {
  let dir = TempDir::new();
  let pid = dir.path().join("pid");
  let config = fake_curl(
    &dir,
    &format!("echo $$ > '{}'; exec sleep 30", pid.display()),
  );
  let mut shell = Harness::new(config);
  let child = shell.cx.child();
  let http = child.http_get("https://example.com/");
  run_until(&mut shell, "curl", || pid.exists());
  assert!(http.loading().get_untracked());
  let pid = std::fs::read_to_string(&pid).expect("its pid");
  let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
  child.owner().dispose();
  run_until(&mut shell, "curl reaped", move || !proc.exists());
}

#[test]
fn refresh_fetches_again_and_stopped_waits_for_it() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let mut shell = Harness::new(offline());
  let http = shell.cx.http_get(server.url("/body")).stopped();
  Runtime::current().flush();
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(server.hits("/body"), 0, "nothing until a refresh");
  assert!(!http.loading().get_untracked());

  http.refresh();
  run_until(&mut shell, "body", move || {
    http.body().get_untracked().is_some()
  });
  http.refresh();
  Runtime::current().flush();
  finish(&mut shell, http);
  assert_eq!(server.hits("/body"), 2);
}

#[test]
fn a_refresh_stops_the_fetch_still_running() {
  let dir = TempDir::new();
  let pids = dir.path().join("pids");
  let script = format!("echo $$ >> '{}'; exec sleep 30", pids.display());
  let mut shell = Harness::new(fake_curl(&dir, &script));
  let http = shell.cx.http_get("https://example.com/");
  run_until(&mut shell, "curl", || pids.exists());
  http.refresh();
  let read = || std::fs::read_to_string(&pids).unwrap_or_default();
  run_until(&mut shell, "another curl", || read().lines().count() == 2);
  let first = read().lines().next().unwrap().to_owned();
  let proc = PathBuf::from(format!("/proc/{first}"));
  run_until(&mut shell, "the first curl reaped", move || !proc.exists());
  assert!(http.loading().get_untracked(), "the second still runs");
}

#[test]
fn a_panicking_url_is_logged_and_fetches_nothing() {
  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let http = shell.cx.http_get(|| -> String { panic!("no city") });
    Runtime::current().flush();
    assert!(!http.loading().get_untracked());
  });
  assert!(logs.contains("cx.http_get (called at"), "{logs}");
  assert!(logs.contains("http/tests.rs"), "{logs}");
  assert!(logs.contains("no city"), "{logs}");
  shell.run_until(Instant::now() + Duration::from_millis(50));
}

/// A shell whose art cache is in `dir`.
fn art_shell(dir: &TempDir) -> Harness {
  Harness::new(Config {
    art_cache: Some(dir.path().join("cache/art")),
    ..offline()
  })
}

fn art_files(dir: &Path) -> Vec<String> {
  let mut names: Vec<String> = std::fs::read_dir(dir)
    .map(|entries| {
      entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
    })
    .unwrap_or_default();
  names.sort();
  names
}

#[test]
fn remote_art_is_downloaded_once_and_reused() {
  use std::os::unix::fs::PermissionsExt;
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let dir = TempDir::new();
  let mut shell = art_shell(&dir);
  let art = shell.cx.cover_art(server.url("/art.png"));
  Runtime::current().flush();
  assert_eq!(art.get_untracked(), None, "nothing while it downloads");
  run_until(&mut shell, "art", move || art.get_untracked().is_some());
  let path = art.get_untracked().unwrap();
  let cache = dir.path().join("cache/art");
  assert_eq!(path.parent(), Some(cache.as_path()));
  assert_eq!(path.extension().unwrap(), "png");
  assert_eq!(std::fs::read(&path).unwrap(), PNG);
  let mode = std::fs::metadata(&cache).unwrap().permissions().mode();
  assert_eq!(mode & 0o777, 0o700);
  assert_eq!(art_files(&cache).len(), 1, "no part left");

  let again = shell.cx.cover_art(server.url("/art.png"));
  Runtime::current().flush();
  assert_eq!(again.get_untracked(), Some(path));
  assert_eq!(server.hits("/art.png"), 1);
}

#[test]
fn art_that_isnt_a_picture_is_refused_and_not_kept() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let dir = TempDir::new();
  let mut shell = art_shell(&dir);
  let url = server.url("/text-art");
  let logs = capture_logs(|| {
    shell.cx.cover_art(url.as_str());
    run_until(&mut shell, "the download", || server.hits("/text-art") == 1);
    shell.run_until(Instant::now() + Duration::from_millis(300));
  });
  assert!(logs.contains("not a PNG"), "{logs}");
  assert!(art_files(&dir.path().join("cache/art")).is_empty());
}

#[test]
fn a_new_url_stops_the_download_before() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let dir = TempDir::new();
  let mut shell = art_shell(&dir);
  let path = shell.cx.signal("/slow.png");
  let base = server.url("");
  let art = shell.cx.cover_art(move || format!("{base}{}", path.get()));
  let cache = dir.path().join("cache/art");
  run_until(&mut shell, "the slow download", || {
    server.hits("/slow.png") == 1
  });
  assert_eq!(art_files(&cache).len(), 1, "its part");
  path.set("/other.png");
  run_until(&mut shell, "art", move || art.get_untracked().is_some());
  let names = art_files(&cache);
  assert_eq!(names.len(), 1, "{names:?}");
  assert!(names[0].ends_with(".png") && !names[0].starts_with('.'));
}

#[test]
fn a_file_url_gives_its_decoded_path() {
  let dir = TempDir::new();
  let file = dir.path().join("cover art.png");
  std::fs::write(&file, PNG).unwrap();
  let shell = art_shell(&dir);
  let url = format!("file://{}", dir.path().join("cover%20art.png").display());
  let art = shell.cx.cover_art(url);
  let missing = shell.cx.cover_art("file:///nonexistent/crabshell.png");
  let other = shell.cx.cover_art("ftp://example.com/x.png");
  let empty = shell.cx.cover_art("");
  Runtime::current().flush();
  assert_eq!(art.get_untracked(), Some(file));
  for none in [missing, other, empty] {
    assert_eq!(none.get_untracked(), None);
  }

  use super::art::file_path;
  assert_eq!(
    file_path("file:///home/me/Music/Cover%20Art%2Fx.png"),
    Some(PathBuf::from("/home/me/Music/Cover Art/x.png"))
  );
  // A stray or short escape stays as it is.
  assert_eq!(
    file_path("file:///tmp/100%.png%4"),
    Some(PathBuf::from("/tmp/100%.png%4"))
  );
  assert_eq!(file_path("file://relative.png"), None);
  assert_eq!(file_path("https://example.com/a.png"), None);
}

#[test]
fn a_trim_keeps_the_newest_under_both_caps() {
  use super::art::{CACHE_BYTES, CACHE_FILES, trim};
  use std::time::SystemTime;
  let dir = TempDir::new();
  let at = |age: u64| SystemTime::now() - Duration::from_secs(age);
  let file = |name: &str, len: u64, age: u64| {
    let file = std::fs::File::create(dir.path().join(name)).unwrap();
    // Sparse, so the test writes nothing.
    file.set_len(len).unwrap();
    file.set_modified(at(age)).unwrap();
  };
  for n in 0..CACHE_FILES as u64 + 5 {
    file(&format!("{n:04}.png"), 1, n + 100);
  }
  trim(dir.path());
  let names = art_files(dir.path());
  assert_eq!(names.len(), CACHE_FILES);
  assert_eq!(
    names.last().unwrap(),
    &format!("{:04}.png", CACHE_FILES - 1)
  );

  for name in names {
    std::fs::remove_file(dir.path().join(name)).unwrap();
  }
  let third = CACHE_BYTES / 3;
  file("new.png", third, 1);
  file("mid.png", third, 2);
  file("old.png", third + 2, 3);
  file(".old.part", 1, 3600);
  file(".new.part", 1, 1);
  trim(dir.path());
  assert_eq!(art_files(dir.path()), [".new.part", "mid.png", "new.png"]);
}

#[test]
fn a_panicking_art_url_is_logged_and_shows_nothing() {
  let dir = TempDir::new();
  let mut shell = art_shell(&dir);
  let logs = capture_logs(|| {
    let art = shell.cx.cover_art(|| -> String { panic!("no player") });
    Runtime::current().flush();
    assert_eq!(art.get_untracked(), None);
  });
  assert!(logs.contains("cx.cover_art (called at"), "{logs}");
  assert!(logs.contains("http/tests.rs"), "{logs}");
  assert!(logs.contains("no player"), "{logs}");
  shell.run_until(Instant::now() + Duration::from_millis(50));
}

#[test]
fn no_cap_and_no_timeout_still_fetch() {
  if !curl_installed() {
    return;
  }
  let server = Server::start();
  let mut shell = Harness::new(offline());
  let http = shell
    .cx
    .http_get(server.url("/body"))
    .max_size(u64::MAX)
    .timeout(Duration::MAX);
  finish(&mut shell, http);
  assert_eq!(http.error().get_untracked(), None);
  assert_eq!(http.body().get_untracked().as_deref(), Some("hello"));
}

#[test]
fn stop_programs_stops_curl_before_the_shell_exits() {
  let dir = TempDir::new();
  let pid = dir.path().join("pid");
  let config = fake_curl(
    &dir,
    &format!("echo $$ > '{}'; exec sleep 30", pid.display()),
  );
  let mut shell = Harness::new(config);
  let _http = shell.cx.http_get("https://example.com/");
  run_until(&mut shell, "curl", || pid.exists());
  let pid = std::fs::read_to_string(&pid).expect("its pid");
  let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
  crate::stop_programs(&shell.cx);
  run_until(&mut shell, "curl reaped", move || !proc.exists());
}
