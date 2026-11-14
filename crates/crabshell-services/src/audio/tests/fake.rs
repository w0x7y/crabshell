//! A stand-in for the audio server. It speaks just enough of PulseAudio's
//! native protocol to bring libpulse to Ready, then only records what the
//! service asks. It answers nothing after the handshake, so a test feeds
//! the service its replies as synthetic messages.

use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use super::NoServer;

/// The native protocol's command numbers, from PulseAudio's
/// `native-common.h`.
pub(super) const REPLY: u32 = 2;
pub(super) const AUTH: u32 = 8;
pub(super) const SET_CLIENT_NAME: u32 = 9;
pub(super) const GET_SERVER_INFO: u32 = 20;
pub(super) const GET_SINK_INFO: u32 = 21;
pub(super) const GET_SINK_INFO_LIST: u32 = 22;
pub(super) const GET_SOURCE_INFO_LIST: u32 = 24;
pub(super) const GET_SINK_INPUT_INFO_LIST: u32 = 30;
pub(super) const SUBSCRIBE: u32 = 35;
pub(super) const SET_SINK_VOLUME: u32 = 36;
pub(super) const SET_SINK_MUTE: u32 = 39;
pub(super) const SET_DEFAULT_SINK: u32 = 44;
pub(super) const SET_DEFAULT_SOURCE: u32 = 45;
pub(super) const SET_SINK_INPUT_VOLUME: u32 = 37;
pub(super) const MOVE_SINK_INPUT: u32 = 67;
pub(super) const SET_SINK_INPUT_MUTE: u32 = 69;

/// A listening socket in a directory of its own.
pub(super) struct FakePulse {
  dir: NoServer,
  listener: Option<UnixListener>,
}

impl FakePulse {
  pub(super) fn new(name: &str) -> FakePulse {
    let mut fake = FakePulse {
      dir: NoServer::new(name),
      listener: None,
    };
    fake.listen();
    fake
  }

  /// Starts taking connections, as when the server starts.
  pub(super) fn listen(&mut self) {
    let listener = UnixListener::bind(self.dir.socket()).expect("a test socket");
    listener
      .set_nonblocking(true)
      .expect("a non-blocking socket");
    self.listener = Some(listener);
  }

  /// Stops taking connections, as when the server is gone.
  pub(super) fn close(&mut self) {
    self.listener = None;
    std::fs::remove_file(self.dir.socket()).expect("the socket file");
  }

  pub(super) fn address(&self) -> String {
    self.dir.address()
  }

  /// Takes the service's connection, waiting up to 2 s for it, and
  /// brings it to Ready.
  pub(super) fn accept(&self) -> Client {
    let listener = self.listener.as_ref().expect("listening");
    let deadline = Instant::now() + Duration::from_secs(2);
    let stream = loop {
      match listener.accept() {
        Ok((stream, _)) => break stream,
        Err(error) if error.kind() == ErrorKind::WouldBlock => {
          assert!(Instant::now() < deadline, "the service never connected");
          std::thread::sleep(Duration::from_millis(5));
        }
        Err(error) => panic!("accept failed: {error}"),
      }
    };
    stream.set_nonblocking(false).expect("a blocking stream");
    let timeout = Some(Duration::from_secs(1));
    stream.set_read_timeout(timeout).expect("a read timeout");
    let mut client = Client(stream);
    let (command, tag, _) = client.packet().expect("an AUTH");
    assert_eq!(command, AUTH);
    // Protocol 35, with no shared memory.
    client.reply(tag, 35);
    let (command, tag, _) = client.packet().expect("a SET_CLIENT_NAME");
    assert_eq!(command, SET_CLIENT_NAME);
    // The client's index on the server.
    client.reply(tag, 0);
    client
  }
}

/// The server's end of one connection.
pub(super) struct Client(UnixStream);

impl Client {
  /// Every command the service sent until 100 ms pass with none, each as
  /// its number and the rest of its tagstruct.
  pub(super) fn commands(&mut self) -> Vec<(u32, Vec<u8>)> {
    let timeout = Some(Duration::from_millis(100));
    self.0.set_read_timeout(timeout).expect("a read timeout");
    let mut commands = Vec::new();
    loop {
      match self.packet() {
        Ok((command, _, rest)) => commands.push((command, rest)),
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
          return commands;
        }
        Err(error) => panic!("a bad packet: {error}"),
      }
    }
  }

  /// Just the numbers of [`commands`](Self::commands).
  pub(super) fn command_numbers(&mut self) -> Vec<u32> {
    self
      .commands()
      .into_iter()
      .map(|(number, _)| number)
      .collect()
  }

  /// One packet: its command, its tag and the rest of its tagstruct.
  fn packet(&mut self) -> io::Result<(u32, u32, Vec<u8>)> {
    // Length, channel, two offset words and flags.
    let mut descriptor = [0; 20];
    self.0.read_exact(&mut descriptor)?;
    let length = u32::from_be_bytes(descriptor[..4].try_into().expect("4 bytes"));
    let mut payload = vec![0; length as usize];
    self.0.read_exact(&mut payload)?;
    let (command, tag) = (u32_at(&payload, 0), u32_at(&payload, 5));
    Ok((command, tag, payload[10..].to_vec()))
  }

  fn reply(&mut self, tag: u32, value: u32) {
    let mut payload = Vec::new();
    for word in [REPLY, tag, value] {
      payload.push(b'L');
      payload.extend_from_slice(&word.to_be_bytes());
    }
    let mut packet = Vec::new();
    let length = u32::try_from(payload.len()).expect("a short reply");
    for word in [length, u32::MAX, 0, 0, 0] {
      packet.extend_from_slice(&word.to_be_bytes());
    }
    packet.extend_from_slice(&payload);
    self.0.write_all(&packet).expect("a write to the service");
  }
}

/// The `u32` tagged `L` at `at` in a tagstruct.
fn u32_at(tagstruct: &[u8], at: usize) -> u32 {
  assert_eq!(tagstruct[at], b'L', "a u32 tag");
  u32::from_be_bytes(tagstruct[at + 1..at + 5].try_into().expect("4 bytes"))
}

/// The sink name and what follows it in a `SET_SINK_VOLUME` or
/// `SET_SINK_MUTE`, after the sink index.
fn after_index(rest: &[u8]) -> (&str, &[u8]) {
  assert_eq!(rest[5], b't', "a sink name");
  let end = 6 + rest[6..].iter().position(|&byte| byte == 0).expect("a NUL");
  let name = std::str::from_utf8(&rest[6..end]).expect("a UTF-8 name");
  (name, &rest[end + 1..])
}

/// The sink name and channel volumes a `SET_SINK_VOLUME` asks for.
pub(super) fn volumes_set(rest: &[u8]) -> (String, Vec<u32>) {
  let (name, volume) = after_index(rest);
  assert_eq!(volume[0], b'v', "a channel volume");
  let channels = usize::from(volume[1]);
  let words = volume[2..].chunks(4).take(channels);
  let volumes = words.map(|word| u32::from_be_bytes(word.try_into().expect("4 bytes")));
  (name.to_owned(), volumes.collect())
}

/// The sink name and mute a `SET_SINK_MUTE` asks for.
pub(super) fn mute_set(rest: &[u8]) -> (String, bool) {
  let (name, mute) = after_index(rest);
  (name.to_owned(), mute[0] == b'1')
}
