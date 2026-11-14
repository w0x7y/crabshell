//! A greeter for greetd: one field on every monitor asks for a user name,
//! then for whatever greetd asks, such as the password. A refused password
//! asks again; once authenticated, the session starts and the greeter
//! exits so greetd can run it.
//!
//! greetd starts it inside a compositor, with `GREETD_SOCK` set, from a
//! line in `/etc/greetd/config.toml` such as
//! `command = "cage -s -- /usr/local/bin/greeter"`.

use crabshell::prelude::*;

/// What the session runs, once started.
const SESSION: &[&str] = &["sway"];

const SCREEN: Style = Style::new()
  .background(Color::rgb(0x11, 0x11, 0x1b))
  .color(Color::rgb(0xcd, 0xd6, 0xf4));
const FIELD: Style = Style::new()
  .background(Color::rgb(0x31, 0x32, 0x44))
  .padding(8, 10)
  .radius(6.0)
  .width(280.0);
const NOTICE: Style = Style::new().color(Color::rgb(0xf3, 0x8b, 0xa8));

#[allow(dead_code, reason = "the tests build the greeter, not main")]
fn main() {
  crabshell::run(greeter);
}

/// The greeter on every output, sharing one attempt.
pub fn greeter(cx: &Cx) {
  let greetd = cx.greetd();
  let state = greetd.state();
  let user = cx.signal(String::new());
  let notice = cx.signal(String::new());
  {
    let (greetd, shell) = (greetd.clone(), cx.clone());
    cx.effect(move || {
      let now = state.get();
      // For greetd's log; a state never holds an answer.
      eprintln!("greetd: {now:?}");
      match now {
        GreetState::Authenticated => {
          notice.set(String::new());
          let command = SESSION.iter().map(|part| part.to_string()).collect();
          greetd.start_session(command, Vec::new());
        }
        GreetState::Started => shell.exit(0),
        GreetState::Failed { message, auth } => {
          notice.set(message);
          // A refused password asks again for the same user.
          if auth {
            greetd.create_session(&user.get_untracked());
          }
        }
        _ => {}
      }
    });
  }
  cx.per_output(move |cx, output| {
    let entry = cx.signal(String::new());
    let greetd = greetd.clone();
    let accept = move |_: &Cx, text: &str| {
      match state.get_untracked() {
        GreetState::Idle | GreetState::Failed { .. } => {
          user.set(text.to_owned());
          greetd.create_session(text);
        }
        GreetState::Asking { .. } => greetd.respond(text),
        _ => return,
      }
      entry.set(String::new());
    };
    PanelWindow::new(output)
      .anchors(Edge::Top | Edge::Bottom | Edge::Left | Edge::Right)
      .layer(Layer::Overlay)
      .keyboard_focus(KeyboardFocus::Exclusive)
      .exclusive_zone(-1)
      .style(SCREEN)
      .child(
        Column::new()
          .gap(12)
          .align(Align::Center)
          .justify(Justify::Center)
          .child(Text::new(move || match state.get() {
            GreetState::Asking { message, .. } => message,
            GreetState::Busy | GreetState::Authenticated | GreetState::Started => String::from("…"),
            _ => String::from("User:"),
          }))
          .child(
            TextInput::new(entry)
              .password(move || matches!(state.get(), GreetState::Asking { visible: false, .. }))
              .focus(true)
              .on_accept(accept)
              .style(FIELD),
          )
          .child(Text::new(move || notice.get()).style(NOTICE)),
      )
  });
}
