use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
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
