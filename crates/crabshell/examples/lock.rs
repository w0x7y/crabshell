//! A lock screen: `crabshell ipc lock on` locks the session, as an idle
//! daemon would, and it locks before the machine sleeps. Every monitor
//! shows a password field; Enter checks the password with PAM and
//! unlocks, or says it was wrong.
//!
//! ```sh
//! cargo run --release -p crabshell --example lock
//! crabshell ipc lock on
//! ```

use std::path::PathBuf;

use crabshell::prelude::*;

/// The PAM service that checks the password, a file in `/etc/pam.d`.
const SERVICE: &str = "login";

const SCREEN: Style = Style::new()
  .background(Color::rgb(0x11, 0x11, 0x1b))
  .color(Color::rgb(0xcd, 0xd6, 0xf4));
const FIELD: Style = Style::new()
  .background(Color::rgb(0x31, 0x32, 0x44))
  .padding(8, 10)
  .radius(6.0)
  .width(280.0);
const NOTICE: Style = Style::new().color(Color::rgb(0xf3, 0x8b, 0xa8));

#[allow(dead_code, reason = "the tests run setup, not main")]
fn main() {
  crabshell::run(|cx| setup(cx, SERVICE));
}

/// The lock screen, locked by `crabshell ipc lock on` and checked with
/// the PAM `service`.
pub fn setup(cx: &Cx, service: &str) {
  // In the runtime directory, so the shell after a reload or a crash locks
  // again, and a reboot starts unlocked.
  let locked = match std::env::var_os("XDG_RUNTIME_DIR") {
    Some(dir) => cx.json_file(PathBuf::from(dir).join("crabshell/lock.json"), false),
    None => cx.signal(false),
  };
  cx.ipc("lock", "on", move |_| {
    locked.set(true);
    Ok(String::new())
  });
  lock_screen(cx, locked, service);
}

/// Locks the session while `locked` is true, checking the password with
/// the PAM `service`; a right one sets `locked` false.
pub fn lock_screen(cx: &Cx, locked: Signal<bool>, service: &str) {
  let pam = cx.pam(service);
  let state = pam.state();
  // One field's text, shared by every output's surface.
  let entry = cx.signal(String::new());
  // Enter was pressed before PAM asked for the password.
  let waiting = cx.signal(false);
  let notice = cx.signal(String::new());
  {
    let pam = pam.clone();
    cx.effect(move || match state.get() {
      PamState::Asking { .. } if waiting.get_untracked() => {
        waiting.set(false);
        pam.respond(entry.get_untracked());
        entry.set(String::new());
      }
      PamState::Done(result) => {
        waiting.set(false);
        entry.set(String::new());
        match result {
          PamResult::Success => {
            notice.set(String::new());
            locked.set(false);
          }
          PamResult::Failure => notice.set(String::from("Wrong password")),
          PamResult::Error { message, .. } => notice.set(message),
        }
      }
      _ => {}
    });
  }
  let accept = move |_: &Cx, text: &str| match state.get_untracked() {
    PamState::Asking { .. } => {
      pam.respond(text);
      entry.set(String::new());
    }
    PamState::Busy => {}
    _ => {
      waiting.set(true);
      pam.start(None);
    }
  };
  let accept = std::rc::Rc::new(accept);
  // Lock as the machine goes to sleep, which waits until the compositor
  // shows the lock.
  let logind = cx.logind();
  let sleeping = logind.preparing_for_sleep();
  cx.effect(move || {
    if sleeping.get() {
      locked.set(true);
    }
  });
  let shown = cx.session_lock(
    SessionLock::new()
      .locked(move || locked.get())
      .style(SCREEN)
      .surface(move |_, _| {
        let accept = std::rc::Rc::clone(&accept);
        Column::new()
          .gap(12)
          .align(Align::Center)
          .justify(Justify::Center)
          .child(Text::new("Locked"))
          .child(
            TextInput::new(entry)
              .password(true)
              .placeholder("password")
              .focus(true)
              .on_accept(move |cx, text| accept(cx, text))
              .style(FIELD),
          )
          .child(Text::new(move || notice.get()).style(NOTICE))
      }),
  );
  logind.delay_sleep_until(move || shown.get());
}
