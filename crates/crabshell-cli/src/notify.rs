//! Desktop notifications, over `org.freedesktop.Notifications` on the
//! session bus.

use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use calloop::LoopHandle;
use calloop::futures::Scheduler;
use calloop::timer::TimeoutFuture;

/// How long a notification server gets to answer, as D-Bus's own default.
const LIMIT: Duration = Duration::from_secs(25);

/// Shows `summary` and `body` as a notification, on the loop. A failure,
/// such as no notification server, is logged.
pub(crate) fn send<D>(
  handle: &LoopHandle<'static, D>,
  scheduler: &Scheduler<()>,
  summary: &str,
  body: &str,
) {
  let (summary, body) = (summary.to_owned(), escape(body));
  // A loop timer rather than zbus's method timeout, which leaves a
  // wakeup behind.
  let timeout = TimeoutFuture::from_duration(handle, LIMIT);
  let scheduled = scheduler.schedule(async move {
    let mut call = pin!(notify(&summary, &body));
    let mut timeout = pin!(timeout);
    let answer = std::future::poll_fn(|context| match call.as_mut().poll(context) {
      Poll::Ready(result) => Poll::Ready(Some(result)),
      Poll::Pending => timeout.as_mut().poll(context).map(|()| None),
    })
    .await;
    match answer {
      Some(Ok(())) => {}
      Some(Err(error)) => crate::log(&format!("can't show a notification: {error}")),
      None => crate::log("the notification server didn't answer"),
    }
  });
  if let Err(error) = scheduled {
    crate::log(&format!("can't show a notification: {error}"));
  }
}

async fn notify(summary: &str, body: &str) -> zbus::Result<()> {
  // A connection per notification: they are rare, and none stays open.
  let connection = zbus::Connection::session().await?;
  let actions: Vec<&str> = Vec::new();
  let hints: HashMap<&str, zbus::Variant<'_>> = HashMap::new();
  connection
    .call_method(
      Some("org.freedesktop.Notifications"),
      "/org/freedesktop/Notifications",
      Some("org.freedesktop.Notifications"),
      "Notify",
      // App name, replaces id, icon, summary, body, actions, hints and
      // the server's own timeout.
      &(
        "crabshell",
        0_u32,
        "",
        summary,
        body,
        actions,
        hints,
        -1_i32,
      ),
    )
    .await?;
  Ok(())
}

/// `text` with the characters a server's body markup reads escaped, as
/// in `expected Option<u32>`.
pub(crate) fn escape(text: &str) -> String {
  text
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}
