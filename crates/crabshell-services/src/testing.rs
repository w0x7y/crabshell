//! What the services' tests share: a shell with no compositor, a private
//! D-Bus daemon and temporary directories.

use std::cell::RefCell;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::futures::Scheduler;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;

use crate::{Config, PamMessage, PamResult};

/// What a shell gives the services, with no compositor: a loop and a root
/// context.
pub(crate) struct Harness {
  pub(crate) event_loop: EventLoop<'static, ()>,
  pub(crate) cx: Cx,
  /// Runs the tests' own futures, such as a fake logind's.
  scheduler: Scheduler<()>,
}

impl Harness {
  pub(crate) fn new(config: Config) -> Harness {
    let event_loop = EventLoop::try_new().expect("an event loop");
    let cx = Cx::new(Runtime::current().new_owner());
    crate::provide_with(&cx, &event_loop.handle(), config);
    let (executor, scheduler) = calloop::futures::executor().expect("an executor");
    event_loop
      .handle()
      .insert_source(executor, |(), _, _| {})
      .expect("the tests' executor in the loop");
    Harness {
      event_loop,
      cx,
      scheduler,
    }
  }

  /// Runs the loop as the shell does, until `deadline`. Returns how many
  /// times it woke.
  pub(crate) fn run_until(&mut self, deadline: Instant) -> usize {
    let mut wakeups = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
      if left.is_zero() {
        break;
      }
      self
        .event_loop
        .dispatch(Some(left), &mut ())
        .expect("the loop runs");
      Runtime::current().flush();
      wakeups += 1;
    }
    wakeups
  }

  /// Starts `future` on the loop, alongside the shell, and returns at once.
  pub(crate) fn spawn(&self, future: impl Future<Output = ()> + 'static) {
    self
      .scheduler
      .schedule(future)
      .expect("the tests' executor runs");
  }

  /// Runs `future` to its end on the loop, which runs everything else
  /// meanwhile. Gives up after 5 s.
  pub(crate) fn block_on<R: 'static>(&mut self, future: impl Future<Output = R> + 'static) -> R {
    let result = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&result);
    self.spawn(async move { *sink.borrow_mut() = Some(future.await) });
    let give_up = Instant::now() + Duration::from_secs(5);
    loop {
      if let Some(result) = result.borrow_mut().take() {
        return result;
      }
      assert!(Instant::now() < give_up, "a future still running after 5 s");
      self.run_until(Instant::now() + Duration::from_millis(10));
    }
  }

  /// Does `act`, then runs the loop for 100 ms, until `done`. Gives up
  /// after 5 s.
  pub(crate) fn retry_until(
    &mut self,
    what: &str,
    done: impl Fn() -> bool,
    mut act: impl FnMut(&mut Harness),
  ) {
    let give_up = Instant::now() + Duration::from_secs(5);
    while !done() {
      assert!(Instant::now() < give_up, "no {what} after 5 s");
      act(self);
      self.run_until(Instant::now() + Duration::from_millis(100));
    }
  }
}

/// A config with no system parts: the system zone through jiff, and no
/// bus, Sway, Hyprland or audio server.
pub(crate) fn offline() -> Config {
  Config {
    localtime: None,
    system_bus: None,
    session_bus: None,
    sway: None,
    hyprland: None,
    audio: None,
    popup_timeout: crate::notifications::POPUP_TIMEOUT,
    greetd: None,
    desktop_entries: Default::default(),
    // Never polkit's own, which would ask PAM.
    polkit_helper: PathBuf::from("/nonexistent/crabshell-polkit-helper"),
    polkit_socket: PathBuf::from("/nonexistent/crabshell-polkit-socket"),
    pam: Arc::new(FakePam::default()),
  }
}

/// A PAM backend that follows a script instead of reaching libpam, so no
/// test counts toward locking a real account. With no steps it succeeds
/// at once.
#[derive(Default)]
pub(crate) struct FakePam {
  pub(crate) script: Vec<Step>,
  /// What each attempt did, in order, shared with the test.
  pub(crate) log: Arc<Mutex<Vec<String>>>,
}

/// One thing a [`FakePam`] does.
pub(crate) enum Step {
  /// Asks a question, and fails unless the answer is the third field.
  Ask(String, bool, &'static str),
  Info(String),
  Error(String),
}

impl FakePam {
  fn note(&self, what: String) {
    self.log.lock().expect("the fake's log").push(what);
  }
}

impl crate::pam::Backend for FakePam {
  fn authenticate(&self, service: &str, user: &str, talk: &crate::pam::Talk) -> PamResult {
    self.note(format!("{service} for {user}"));
    let result = (|| {
      for step in &self.script {
        match step {
          Step::Ask(question, visible, wanted) => match talk.ask(question.as_ref(), *visible) {
            None => return PamResult::Error(String::from("refused")),
            Some(answer) if answer.bytes() != wanted.as_bytes() => return PamResult::Failure,
            Some(_) => {}
          },
          Step::Info(text) => talk.tell(PamMessage::Info, text.as_ref()),
          Step::Error(text) => talk.tell(PamMessage::Error, text.as_ref()),
        }
      }
      PamResult::Success
    })();
    self.note(format!("ended {result:?}"));
    result
  }
}

/// A directory of a test's own, empty at first. It goes on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
  pub(crate) fn new() -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-services-dir-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    TempDir(dir)
  }

  pub(crate) fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for TempDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// A private bus standing in for the system or session bus: a
/// `dbus-daemon` on an address of its own, which starts no services. It
/// stops on drop.
pub(crate) struct PrivateBus {
  daemon: Child,
  pub(crate) address: String,
  config: std::path::PathBuf,
}

impl PrivateBus {
  pub(crate) fn start() -> PrivateBus {
    let config = private_bus_config();
    let mut daemon = Command::new("dbus-daemon")
      .arg(format!("--config-file={}", config.display()))
      .args(["--nofork", "--print-address"])
      .stdout(Stdio::piped())
      .spawn()
      .expect("dbus-daemon starts (it comes with dbus)");
    let stdout = daemon.stdout.take().expect("dbus-daemon's stdout");
    let mut address = String::new();
    BufReader::new(stdout)
      .read_line(&mut address)
      .expect("dbus-daemon prints its address");
    let address = address.trim().to_owned();
    PrivateBus {
      daemon,
      address,
      config,
    }
  }

  /// A connection to it, owning `names`.
  pub(crate) fn connect(
    &self,
    names: &[&'static str],
  ) -> impl Future<Output = zbus::Connection> + 'static {
    let mut builder = zbus::connection::Builder::address(self.address.as_str());
    for &name in names {
      builder = builder.name(name);
    }
    async move {
      builder
        .build()
        .await
        .expect("a connection to the private bus")
    }
  }
}

impl Drop for PrivateBus {
  fn drop(&mut self) {
    let _ = self.daemon.kill();
    let _ = self.daemon.wait();
    let _ = std::fs::remove_file(&self.config);
  }
}

/// A session bus's config with no service directories, written to a file
/// of its own. `dbus-daemon --session` would read the system's, and start
/// real services, such as the desktop's notification daemon, on demand.
fn private_bus_config() -> std::path::PathBuf {
  use std::sync::atomic::{AtomicU32, Ordering};
  static BUSES: AtomicU32 = AtomicU32::new(0);
  let path = std::env::temp_dir().join(format!(
    "crabshell-bus-{}-{}.conf",
    std::process::id(),
    BUSES.fetch_add(1, Ordering::Relaxed)
  ));
  std::fs::write(
    &path,
    "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>\
     <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
     <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
  )
  .expect("the bus's config");
  path
}

/// Runs `f` and returns everything it logged through `tracing` on this
/// thread, from `info` up.
pub(crate) fn capture_logs(f: impl FnOnce()) -> String {
  capture_logs_from(tracing::Level::INFO, f)
}

/// Runs `f` and returns everything it logged through `tracing` on this
/// thread, from `level` up.
pub(crate) fn capture_logs_from(level: tracing::Level, f: impl FnOnce()) -> String {
  #[derive(Clone, Default)]
  struct Buffer(Arc<Mutex<Vec<u8>>>);

  impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self.0.lock().expect("log lock").extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  let buffer = Buffer::default();
  let writer = buffer.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_max_level(level)
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = buffer.0.lock().expect("log lock").clone();
  String::from_utf8(bytes).expect("UTF-8 logs")
}

/// What a fake answers a method with: a reply built for the call, or
/// `None` to ignore it.
type Handler = Box<dyn Fn(&zbus::Message) -> Option<zbus::Result<zbus::Message>>>;

/// A program on a private bus standing in for a system daemon, such as
/// UPower or BlueZ: it owns `name`, answers `Properties` calls and
/// `ObjectManager.GetManagedObjects` from a table of objects, emits
/// `PropertiesChanged` when a test sets one, and answers other methods
/// with the handlers a test gives it.
pub(crate) struct FakeService {
  pub(crate) connection: zbus::Connection,
  name: &'static str,
  /// Each object's properties, by path and interface.
  objects: Rc<RefCell<std::collections::BTreeMap<(String, String), crate::dbus::Props>>>,
  handlers: Rc<RefCell<std::collections::HashMap<String, Handler>>>,
  /// `Set` calls and those to handlers, as `member path`.
  pub(crate) calls: Rc<RefCell<Vec<String>>>,
  /// While `Some`, replies are built as calls come but sent only at
  /// [`release`](Self::release), as from a slow daemon.
  held: Rc<RefCell<Option<Vec<zbus::Message>>>>,
}

impl FakeService {
  /// Joins `bus` and starts answering. It takes `name` at
  /// [`own`](Self::own), once a test has given it its objects, as a
  /// daemon exports them before it takes its name.
  pub(crate) fn start(shell: &mut Harness, bus: &PrivateBus, name: &'static str) -> FakeService {
    use std::future::poll_fn;
    use std::pin::Pin;

    use zbus::export::futures_core::Stream;
    use zbus::message::Type as Kind;

    let connection = shell.block_on(bus.connect(&[]));
    let fake = FakeService {
      connection: connection.clone(),
      name,
      objects: Rc::default(),
      handlers: Rc::default(),
      calls: Rc::default(),
      held: Rc::default(),
    };
    let (objects, handlers, calls, held) = (
      Rc::clone(&fake.objects),
      Rc::clone(&fake.handlers),
      Rc::clone(&fake.calls),
      Rc::clone(&fake.held),
    );
    let mut messages = zbus::MessageStream::from(&connection);
    shell.spawn(async move {
      while let Some(Ok(call)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
        let header = call.header();
        if header.message_type() != Kind::MethodCall {
          continue;
        }
        let path = header.path().map_or(String::new(), |path| path.to_string());
        let member = header
          .member()
          .map_or(String::new(), |member| member.to_string());
        let interface = header.interface().map(|interface| interface.as_str());
        let mut announce = None;
        // Built before any await, so no borrow is held across one.
        let reply = if interface == Some(crate::dbus::OBJECT_MANAGER)
          && member == "GetManagedObjects"
        {
          // The objects below `path`, as an object manager there lists them.
          let below = format!("{}/", path.trim_end_matches('/'));
          let objects = objects.borrow();
          let mut managed = std::collections::HashMap::<_, std::collections::HashMap<_, _>>::new();
          for ((at, interface), props) in objects.iter() {
            if at.starts_with(&below) && *at != path {
              let at = zbus::ObjectPath::try_from(at.as_str()).expect("a path");
              managed.entry(at).or_default().insert(interface, props);
            }
          }
          Some(zbus::Message::method_return(&header).build(&managed))
        } else if interface == Some(crate::dbus::PROPERTIES) {
          let objects = &mut *objects.borrow_mut();
          match member.as_str() {
            "GetAll" => {
              let interface: String = call.body().deserialize().expect("s");
              objects
                .get(&(path.clone(), interface))
                .map(|props| zbus::Message::method_return(&header).build(props))
            }
            "Get" => {
              let (interface, name): (String, String) = call.body().deserialize().expect("ss");
              let value = objects
                .get(&(path.clone(), interface))
                .and_then(|props| props.get(&name));
              value.map(|value| zbus::Message::method_return(&header).build(value))
            }
            "Set" => {
              let (interface, name, value): (String, String, zbus::OwnedVariant) =
                call.body().deserialize().expect("ssv");
              calls.borrow_mut().push(format!("Set {path} {name}"));
              let changed = (name.clone(), value.try_clone().expect("a value"));
              objects
                .get_mut(&(path.clone(), interface.clone()))
                .map(|props| {
                  props.insert(name, value);
                  // As a daemon does, once the reply is out.
                  announce = Some((interface, changed));
                  zbus::Message::method_return(&header).build(&())
                })
            }
            _ => None,
          }
        } else {
          calls.borrow_mut().push(format!("{member} {path}"));
          handlers
            .borrow()
            .get(&member)
            .and_then(|handler| handler(&call))
        };
        let reply = reply.unwrap_or_else(|| no_object(&header));
        if let (Some(queue), Ok(reply)) = (held.borrow_mut().as_mut(), &reply) {
          queue.push(reply.clone());
          continue;
        }
        let sent = match reply {
          Ok(reply) => connection.send(&reply).await,
          Err(error) => Err(error),
        };
        sent.expect("the fake's reply");
        if let Some((interface, (name, value))) = announce {
          let changed = crate::dbus::Props::from([(name, value)]);
          let body = (interface, changed, Vec::<String>::new());
          let signal = "PropertiesChanged";
          let sent = connection.emit_signal(
            None::<&str>,
            path.as_str(),
            crate::dbus::PROPERTIES,
            signal,
            &body,
          );
          sent.await.expect("the fake's signal");
        }
      }
    });
    fake
  }

  /// Holds every reply from now on until [`release`](Self::release),
  /// each with what was true when its call came.
  pub(crate) fn hold(&self) {
    self.held.replace(Some(Vec::new()));
  }

  /// Sends the replies held since [`hold`](Self::hold), and answers at
  /// once again.
  pub(crate) fn release(&self, shell: &mut Harness) {
    let replies = self.held.take().unwrap_or_default();
    let connection = self.connection.clone();
    shell
      .block_on(async move {
        for reply in replies {
          connection.send(&reply).await?;
        }
        zbus::Result::Ok(())
      })
      .expect("the held replies sent");
  }

  /// Sends the oldest reply held, and holds the rest.
  pub(crate) fn release_first(&self, shell: &mut Harness) {
    let reply = self.held.borrow_mut().as_mut().map(|held| held.remove(0));
    let (connection, reply) = (self.connection.clone(), reply.expect("a held reply"));
    shell
      .block_on(async move { connection.send(&reply).await })
      .expect("the held reply sent");
  }

  /// How many replies are held.
  pub(crate) fn held(&self) -> usize {
    self.held.borrow().as_ref().map_or(0, Vec::len)
  }

  /// Takes its name, as a daemon that is ready.
  pub(crate) fn own(&self, shell: &mut Harness) {
    let (connection, name) = (self.connection.clone(), self.name);
    shell
      .block_on(async move { connection.request_name(name).await })
      .expect("the name taken");
  }

  /// Gives up its name, as a daemon that stops. Its answering goes on, as
  /// a dropped fake's does, until the bus goes.
  pub(crate) fn leave(self, shell: &mut Harness) {
    let (connection, name) = (self.connection.clone(), self.name);
    shell
      .block_on(async move { connection.release_name(name).await })
      .expect("the name released");
  }

  /// Gives `path` the properties `props` on `interface`, without saying
  /// so on the bus, as an object that was there all along.
  pub(crate) fn add(&self, path: &str, interface: &str, props: &[(&str, zbus::Variant<'_>)]) {
    let props = props
      .iter()
      .map(|(name, value)| ((*name).to_owned(), owned(value)))
      .collect();
    self
      .objects
      .borrow_mut()
      .insert((path.to_owned(), interface.to_owned()), props);
  }

  /// What lists the paths that have `interface` when called, sorted, for a
  /// handler such as UPower's `EnumerateDevices`.
  pub(crate) fn paths(&self, interface: &str) -> impl Fn() -> Vec<String> + 'static {
    let (objects, interface) = (Rc::clone(&self.objects), interface.to_owned());
    move || {
      let objects = objects.borrow();
      let paths = objects.keys().filter(|(_, with)| *with == interface);
      paths.map(|(path, _)| path.clone()).collect()
    }
  }

  /// Takes every interface of `path` away.
  pub(crate) fn remove(&self, path: &str) {
    self.objects.borrow_mut().retain(|(at, _), _| at != path);
  }

  /// Sets `props` on `path`'s `interface` and says so with
  /// `PropertiesChanged`.
  pub(crate) fn set(
    &self,
    shell: &mut Harness,
    path: &'static str,
    interface: &'static str,
    props: &[(&str, zbus::Variant<'_>)],
  ) {
    let changed: crate::dbus::Props = props
      .iter()
      .map(|(name, value)| ((*name).to_owned(), owned(value)))
      .collect();
    if let Some(object) = self
      .objects
      .borrow_mut()
      .get_mut(&(path.to_owned(), interface.to_owned()))
    {
      for (name, value) in &changed {
        object.insert(name.clone(), value.try_clone().expect("a value"));
      }
    }
    let body = (interface, changed, Vec::<String>::new());
    self.emit(
      shell,
      path,
      crate::dbus::PROPERTIES,
      "PropertiesChanged",
      body,
    );
  }

  /// Emits `member` of `interface` from `path`, carrying `body`.
  pub(crate) fn emit<B>(
    &self,
    shell: &mut Harness,
    path: &'static str,
    interface: &'static str,
    member: &'static str,
    body: B,
  ) where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let connection = self.connection.clone();
    shell
      .block_on(async move {
        connection
          .emit_signal(None::<&str>, path, interface, member, &body)
          .await
      })
      .expect("a signal on the private bus");
  }

  /// Answers calls to `member` on any interface with `handler`'s reply.
  pub(crate) fn on(
    &self,
    member: &str,
    handler: impl Fn(&zbus::Message) -> Option<zbus::Result<zbus::Message>> + 'static,
  ) {
    self
      .handlers
      .borrow_mut()
      .insert(member.to_owned(), Box::new(handler));
  }
}

/// The reply to `call` carrying `body`.
pub(crate) fn reply<B>(call: &zbus::Message, body: &B) -> Option<zbus::Result<zbus::Message>>
where
  B: serde::Serialize + zbus::DynamicType,
{
  Some(zbus::Message::method_return(&call.header()).build(body))
}

fn owned(value: &zbus::Variant<'_>) -> zbus::OwnedVariant {
  value.try_to_owned().expect("an owned value")
}

/// The error a daemon gives for a path, interface or method it doesn't
/// have.
fn no_object(header: &zbus::message::Header<'_>) -> zbus::Result<zbus::Message> {
  use zbus::DBusError;

  zbus::fdo::Error::UnknownObject("no such object".into()).create_reply(header)
}
