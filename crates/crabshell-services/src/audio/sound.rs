//! Short sounds: a WAV file played once on the default sink, through a
//! playback stream that goes once the server has played it.

use std::collections::HashSet;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libpulse_binding::channelmap::{Map, MapDef};
use libpulse_binding::context;
use libpulse_binding::operation::{self, Operation};
use libpulse_binding::proplist::{Proplist, properties};
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{FlagSet, SeekMode, State, Stream};

use super::{Audio, AudioService, Event, Locked, Pulse, on_pa_thread};
use crate::file::{MAX_FILE, open_file};

/// The most channels a sound has.
const MAX_CHANNELS: u16 = 8;

/// A WAV file's samples, as libpulse plays them.
#[derive(Debug, PartialEq)]
pub(super) struct Wav {
  pub(super) spec: Spec,
  /// Where the samples are in the file, whole frames only.
  pub(super) data: Range<usize>,
}

/// The samples in the WAV file `bytes`: RIFF chunks, of which `fmt ` and
/// `data` are read and the rest skipped. PCM unsigned 8, signed 16, 24 and
/// 32 bit, and 32-bit float, also in `WAVE_FORMAT_EXTENSIBLE`, with 1 to 8
/// channels at any rate libpulse takes.
pub(super) fn parse(bytes: &[u8]) -> Result<Wav, &'static str> {
  if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
    return Err("it isn't a WAV file");
  }
  let (mut fmt, mut data) = (None, None);
  let mut at = 12;
  while let Some(header) = bytes.get(at..at + 8) {
    let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let start = at + 8;
    let end = start.checked_add(size).filter(|&end| end <= bytes.len());
    match (&header[..4], end) {
      (b"fmt ", Some(end)) => fmt = Some(&bytes[start..end]),
      (b"data", Some(end)) => data = Some(start..end),
      (b"data", None) => return Err("its data runs past the end of the file"),
      (_, None) => break,
      _ => {}
    }
    // Chunks are padded to an even length.
    at = start + size + size % 2;
  }
  let fmt = fmt.ok_or("it has no format chunk")?;
  let data = data.ok_or("it has no data chunk")?;
  let field = |at: usize| {
    fmt
      .get(at..at + 2)
      .map(|b| u16::from_le_bytes([b[0], b[1]]))
  };
  let (Some(mut tag), Some(channels), Some(bits)) = (field(0), field(2), field(14)) else {
    return Err("its format chunk is too short");
  };
  let rate = fmt
    .get(4..8)
    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    .ok_or("its format chunk is too short")?;
  if tag == 0xfffe {
    // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the tag.
    tag = field(24).ok_or("its extensible format chunk is too short")?;
  }
  // A 24-bit sample in a 32-bit container fills its high bits, so it
  // plays as 32-bit.
  let format = match (tag, bits) {
    (1, 8) => Format::U8,
    (1, 16) => Format::S16le,
    (1, 24) => Format::S24le,
    (1, 32) => Format::S32le,
    (3, 32) => Format::F32le,
    _ => return Err("it isn't 8, 16, 24 or 32-bit PCM or 32-bit float"),
  };
  if !(1..=MAX_CHANNELS).contains(&channels) {
    return Err("it doesn't have 1 to 8 channels");
  }
  let spec = Spec {
    format,
    rate,
    channels: channels as u8,
  };
  if !spec.is_valid() {
    return Err("libpulse doesn't take its rate");
  }
  let frame = spec.frame_size();
  let whole = data.len() - data.len() % frame;
  Ok(Wav {
    spec,
    data: data.start..data.start + whole,
  })
}

/// The bytes of the file at `path`, read at once, if it is a regular file
/// of at most 16 MiB.
fn read(path: &Path) -> Result<Vec<u8>, String> {
  let file = open_file(path).map_err(|e| e.message)?;
  let mut bytes = Vec::new();
  file
    .take(MAX_FILE + 1)
    .read_to_end(&mut bytes)
    .map_err(|e| e.to_string())?;
  if bytes.len() as u64 > MAX_FILE {
    return Err("it is over 16 MiB".into());
  }
  Ok(bytes)
}

impl Audio {
  /// Plays the WAV file at `path` once on the default sink, as Qt's
  /// `SoundEffect`. It is read at once, on the loop's thread, so keep
  /// sounds short. A file that can't be read or played is logged once and
  /// skipped, and with no audio server nothing plays. The stream goes
  /// once the server has played it, and nothing wakes the shell after.
  pub fn play_sound(&self, path: impl AsRef<Path>) {
    let path = path.as_ref();
    let Some(service) = &self.service else {
      return tracing::debug!("no audio service: no sound");
    };
    let sound = read(path).and_then(|bytes| {
      let wav = parse(&bytes)?;
      Ok((bytes, wav))
    });
    let (bytes, wav) = match sound {
      Ok(sound) => sound,
      Err(why) => return service.refused(path, &why),
    };
    let mut pulse = service.pulse.borrow_mut();
    let played = match pulse.as_mut() {
      Some(pulse) if pulse.intro.is_some() => pulse.play(bytes, wav),
      Some(pulse) if pulse.down => Err("there is no audio server".to_owned()),
      // libpulse hasn't started or connected yet, as for a sound played
      // from the root closure.
      _ if service.shared.config.audio.is_some() => {
        let mut waiting = service.waiting_sounds.borrow_mut();
        if waiting.len() < MAX_WAITING {
          waiting.push((bytes, wav));
          return;
        }
        Err(format!(
          "{MAX_WAITING} sounds already wait for the audio server"
        ))
      }
      _ => Err("there is no audio server".to_owned()),
    };
    if let Err(why) = played {
      tracing::info!(path = %path.display(), why, "a sound didn't play");
    }
  }
}

impl AudioService {
  /// Plays the sounds that waited for the connection, now ready.
  pub(super) fn play_waiting(&self, pulse: &mut Pulse) {
    for (bytes, wav) in self.waiting_sounds.take() {
      if let Err(why) = pulse.play(bytes, wav) {
        tracing::info!(why, "a sound didn't play");
      }
    }
  }

  /// Drops the sounds that waited for a connection that failed.
  pub(super) fn drop_waiting(&self) {
    let dropped = self.waiting_sounds.take().len();
    if dropped > 0 {
      tracing::info!(dropped, "sounds waiting for the audio server didn't play");
    }
  }

  /// Logs that the sound at `path` can't be played, once per path.
  fn refused(&self, path: &Path, why: &str) {
    if self.bad_sounds.borrow_mut().insert(path.to_owned()) {
      tracing::warn!(path = %path.display(), why, "can't play a sound");
    }
  }
}

/// The sounds already logged as unplayable.
pub(super) type BadSounds = HashSet<PathBuf>;

/// The most sounds that wait for the audio server to connect.
const MAX_WAITING: usize = 4;

/// A sound waiting for the audio server to connect.
pub(super) type Waiting = Vec<(Vec<u8>, Wav)>;

/// A sound playing.
pub(super) struct Sound {
  id: u64,
  stream: Playing,
  bytes: Vec<u8>,
  /// Bytes a frame, so only whole frames are written.
  frame: usize,
  /// What is left to write.
  left: Range<usize>,
  draining: bool,
}

/// A playback stream whose callbacks are cleared before it drops, as a
/// peak meter's are, and its drain, cancelled if it hasn't ended, which
/// frees its callback. Dropped under the mainloop's lock, or with
/// libpulse's thread stopped.
struct Playing {
  stream: Stream,
  drain: Option<Operation<dyn FnMut(bool)>>,
}

impl Drop for Playing {
  fn drop(&mut self) {
    // An ended drain's callback was freed when it ran.
    if let Some(drain) = self.drain.as_mut()
      && drain.get_state() == operation::State::Running
    {
      drain.cancel();
    }
    self.stream.set_write_callback(None);
    self.stream.set_state_callback(None);
  }
}

impl Pulse {
  /// Starts playing `wav`, from `bytes`, or says why it can't.
  fn play(&mut self, bytes: Vec<u8>, wav: Wav) -> Result<(), String> {
    let lock = Locked::new(&mut self.mainloop);
    let context = self
      .context
      .as_mut()
      .filter(|context| context.get_state() == context::State::Ready)
      .ok_or("the audio server isn't connected")?;
    let mut map = Map::default();
    map
      .init_auto(wav.spec.channels, MapDef::WAVEEx)
      .ok_or("libpulse has no channel map for it")?;
    let mut properties = Proplist::new().ok_or("libpulse made no property list")?;
    let _ = properties.set_str(properties::MEDIA_ROLE, "event");
    let mut stream =
      Stream::new_with_proplist(context, "Sound", &wav.spec, Some(&map), &mut properties)
        .ok_or("libpulse made no stream")?;
    stream
      .connect_playback(None, None, FlagSet::NOFLAGS, None, None)
      .map_err(|error| format!("the audio server refused it: {error}"))?;
    let id = self.next_sound;
    self.next_sound += 1;
    let (sender, generation) = (Arc::clone(&self.sender), self.generation);
    let on_write = on_pa_thread(move |_| {
      let _ = sender.send((generation, Event::Sound(id)));
    });
    stream.set_write_callback(Some(Box::new(on_write)));
    let sender = Arc::clone(&self.sender);
    let on_state = on_pa_thread(move || {
      let _ = sender.send((generation, Event::Sound(id)));
    });
    stream.set_state_callback(Some(Box::new(on_state)));
    drop(lock);
    self.sounds.push(Sound {
      id,
      stream: Playing {
        stream,
        drain: None,
      },
      frame: wav.spec.frame_size(),
      left: wav.data,
      bytes,
      draining: false,
    });
    Ok(())
  }

  /// Writes what sound `id`'s stream asks for, drains it once it is all
  /// written, and ends it once the server ended it.
  pub(super) fn feed_sound(&mut self, id: u64) {
    let _lock = Locked::new(&mut self.mainloop);
    let Some(at) = self.sounds.iter().position(|sound| sound.id == id) else {
      return;
    };
    let sound = &mut self.sounds[at];
    let stream = &mut sound.stream.stream;
    match stream.get_state() {
      State::Ready => {}
      State::Failed | State::Terminated => {
        tracing::info!("the audio server ended a sound");
        self.sounds.swap_remove(at);
        return;
      }
      _ => return,
    }
    let room = stream.writable_size().unwrap_or(0);
    let n = sound.left.len().min(room - room % sound.frame);
    if n > 0 {
      let chunk = &sound.bytes[sound.left.start..sound.left.start + n];
      if stream.write(chunk, None, 0, SeekMode::Relative).is_err() {
        tracing::info!("a sound's stream took no more samples");
        self.sounds.swap_remove(at);
        return;
      }
      sound.left.start += n;
    }
    if sound.left.is_empty() && !sound.draining {
      sound.draining = true;
      let (sender, generation) = (Arc::clone(&self.sender), self.generation);
      let on_drained = on_pa_thread(move |_| {
        let _ = sender.send((generation, Event::SoundDone(id)));
      });
      sound.stream.drain = Some(stream.drain(Some(Box::new(on_drained))));
    }
  }

  /// Ends sound `id`, which the server has played.
  pub(super) fn sound_done(&mut self, id: u64) {
    let _lock = Locked::new(&mut self.mainloop);
    self.sounds.retain(|sound| sound.id != id);
  }
}
