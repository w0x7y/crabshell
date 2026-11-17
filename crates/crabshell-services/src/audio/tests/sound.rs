//! Short sounds: the WAV parser on built files, and what the service asks
//! of the fake server. The private-server tests play one.

use libpulse_binding::sample::Format;

use super::fake::{CREATE_PLAYBACK_STREAM, FakePulse};
use super::{Harness, connected, logs_of, one_at_a_time};
use crate::ServicesExt;
use crate::audio::sound::parse;
use crate::testing::TempDir;

/// A WAV file of `tag`, `channels`, `rate` and `bits`, with `before` and
/// `after` chunks around its `fmt ` and `data` chunks.
pub(super) fn wav(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
  wav_with(tag, channels, rate, bits, &[], data, &[])
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
  let mut chunk = id.to_vec();
  chunk.extend_from_slice(&(body.len() as u32).to_le_bytes());
  chunk.extend_from_slice(body);
  if body.len() % 2 == 1 {
    chunk.push(0);
  }
  chunk
}

fn format_chunk(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
  let align = channels * bits / 8;
  let mut fmt = Vec::new();
  fmt.extend_from_slice(&tag.to_le_bytes());
  fmt.extend_from_slice(&channels.to_le_bytes());
  fmt.extend_from_slice(&rate.to_le_bytes());
  fmt.extend_from_slice(&rate.wrapping_mul(u32::from(align)).to_le_bytes());
  fmt.extend_from_slice(&align.to_le_bytes());
  fmt.extend_from_slice(&bits.to_le_bytes());
  fmt
}

fn wav_with(
  tag: u16,
  channels: u16,
  rate: u32,
  bits: u16,
  before: &[u8],
  data: &[u8],
  after: &[u8],
) -> Vec<u8> {
  riff(&[
    before,
    &chunk(b"fmt ", &format_chunk(tag, channels, rate, bits)),
    &chunk(b"data", data),
    after,
  ])
}

fn riff(chunks: &[&[u8]]) -> Vec<u8> {
  let body: Vec<u8> = chunks.concat();
  let mut file = b"RIFF".to_vec();
  file.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
  file.extend_from_slice(b"WAVE");
  file.extend_from_slice(&body);
  file
}

#[test]
fn each_pcm_and_float_format_parses() {
  for (tag, bits, format) in [
    (1, 8, Format::U8),
    (1, 16, Format::S16le),
    (1, 24, Format::S24le),
    (1, 32, Format::S32le),
    (3, 32, Format::F32le),
  ] {
    let file = wav(tag, 2, 44_100, bits, &[0; 48]);
    let parsed = parse(&file).expect("a WAV file");
    assert_eq!(parsed.spec.format, format, "{tag} {bits}");
    assert_eq!((parsed.spec.channels, parsed.spec.rate), (2, 44_100));
    assert_eq!(&file[parsed.data], &[0; 48][..]);
  }
}

#[test]
fn an_extensible_format_takes_its_sub_format() {
  for (sub, bits, format) in [(1_u16, 24, Format::S24le), (3, 32, Format::F32le)] {
    let mut fmt = format_chunk(0xfffe, 6, 48_000, bits);
    // The extension's size, valid bits and channel mask, then the GUID.
    fmt.extend_from_slice(&22_u16.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    fmt.extend_from_slice(&0x3f_u32.to_le_bytes());
    fmt.extend_from_slice(&sub.to_le_bytes());
    fmt.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
    let file = riff(&[&chunk(b"fmt ", &fmt), &chunk(b"data", &[0; 36])]);
    let parsed = parse(&file).expect("an extensible WAV file");
    assert_eq!(parsed.spec.format, format);
    assert_eq!(parsed.spec.channels, 6);
  }
}

#[test]
fn odd_chunks_are_skipped_and_a_partial_frame_dropped() {
  let samples: Vec<u8> = (1..=9).collect();
  // An odd-length chunk before, padded, and one after.
  let file = wav_with(
    1,
    2,
    8_000,
    16,
    &chunk(b"LIST", b"odd"),
    &samples,
    &chunk(b"cue ", &[1; 4]),
  );
  let parsed = parse(&file).expect("a WAV file");
  // Two frames of 4 bytes; the ninth byte isn't a whole frame.
  assert_eq!(&file[parsed.data], &samples[..8]);
}

#[test]
fn a_truncated_or_unplayable_file_is_refused() {
  let good = wav(1, 2, 48_000, 16, &[0; 16]);
  // The data chunk says 16 bytes; cut 2 off.
  let truncated = &good[..good.len() - 2];
  assert_eq!(
    parse(truncated),
    Err("its data runs past the end of the file")
  );
  let no_data = riff(&[&chunk(b"fmt ", &format_chunk(1, 2, 48_000, 16))]);
  let no_format = riff(&[&chunk(b"data", &[0; 4])]);
  for (file, why) in [
    (b"RIFX\0\0\0\0WAVE".to_vec(), "it isn't a WAV file"),
    (Vec::new(), "it isn't a WAV file"),
    (no_data, "it has no data chunk"),
    (no_format, "it has no format chunk"),
    (
      wav(2, 2, 48_000, 4, &[0; 4]),
      "it isn't 8, 16, 24 or 32-bit PCM or 32-bit float",
    ),
    (
      wav(1, 2, 48_000, 12, &[0; 4]),
      "it isn't 8, 16, 24 or 32-bit PCM or 32-bit float",
    ),
    (
      wav(3, 2, 48_000, 64, &[0; 32]),
      "it isn't 8, 16, 24 or 32-bit PCM or 32-bit float",
    ),
    (
      wav(1, 0, 48_000, 16, &[]),
      "it doesn't have 1 to 8 channels",
    ),
    (
      wav(1, 9, 48_000, 16, &[0; 18]),
      "it doesn't have 1 to 8 channels",
    ),
    (wav(1, 2, 0, 16, &[0; 4]), "libpulse doesn't take its rate"),
    (
      wav(1, 2, u32::MAX, 16, &[0; 4]),
      "libpulse doesn't take its rate",
    ),
  ] {
    assert_eq!(parse(&file).map(|_| ()), Err(why));
  }
  // A chunk size near u32::MAX doesn't wrap.
  let mut huge = riff(&[&chunk(b"fmt ", &format_chunk(1, 2, 48_000, 16))]);
  huge.extend_from_slice(b"data");
  huge.extend_from_slice(&u32::MAX.to_le_bytes());
  assert_eq!(parse(&huge), Err("its data runs past the end of the file"));
}

#[test]
fn a_bad_file_is_logged_once_per_path_and_nothing_plays() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("bad-sound");
  let (shell, mut client) = connected(&fake);
  let dir = TempDir::new();
  let big = dir.path().join("big.wav");
  let file = std::fs::File::create(&big).expect("a test file");
  file.set_len(16 * 1024 * 1024 + 1).expect("a sparse file");
  let not_wav = dir.path().join("not.wav");
  std::fs::write(&not_wav, b"hello").expect("a test file");
  let audio = shell.cx.audio();
  let logs = logs_of(|| {
    for _ in 0..2 {
      audio.play_sound(&big);
      audio.play_sound(&not_wav);
      audio.play_sound(dir.path());
      audio.play_sound(dir.path().join("missing.wav"));
    }
  });
  assert_eq!(logs.matches("can't play a sound").count(), 4, "{logs}");
  assert!(logs.contains("it is over 16 MiB"), "{logs}");
  assert!(logs.contains("it isn't a WAV file"), "{logs}");
  assert!(logs.contains("it isn't a file"), "{logs}");
  assert!(logs.contains("No such file"), "{logs}");
  assert!(!client.command_numbers().contains(&CREATE_PLAYBACK_STREAM));
}

#[test]
fn a_sound_opens_a_playback_stream_in_its_format() {
  let _one = one_at_a_time();
  let fake = FakePulse::new("sound");
  let (shell, mut client) = connected(&fake);
  let dir = TempDir::new();
  let path = dir.path().join("beep.wav");
  std::fs::write(&path, wav(1, 2, 48_000, 16, &[0; 64])).expect("a test file");
  shell.cx.audio().play_sound(&path);
  let made: Vec<_> = client
    .commands()
    .into_iter()
    .filter(|(command, _)| *command == CREATE_PLAYBACK_STREAM)
    .collect();
  assert_eq!(made.len(), 1);
  // The sample spec: S16LE (3), 2 channels, 48000 Hz big-endian.
  let spec = [b'a', 3, 2, 0, 0, 0xbb, 0x80];
  assert!(made[0].1.windows(7).any(|w| w == spec), "{:?}", made[0].1);
  let sounds = |shell: &Harness| {
    let service = shell.service();
    let pulse = service.pulse.borrow();
    pulse.as_ref().map_or(0, |pulse| pulse.sounds.len())
  };
  assert_eq!(sounds(&shell), 1);
  // A dropped connection ends it.
  let (mut fake, mut shell) = (fake, shell);
  drop(client);
  fake.close();
  shell.run_until(|shell| sounds(shell) == 0);
}

#[test]
fn without_a_server_a_sound_is_skipped() {
  let shell = Harness::new("unix:/nonexistent/crabshell-test");
  let dir = TempDir::new();
  let path = dir.path().join("beep.wav");
  std::fs::write(&path, wav(1, 1, 8_000, 8, &[128; 8])).expect("a test file");
  let logs = logs_of(|| shell.cx.audio().play_sound(&path));
  assert!(logs.contains("a sound didn't play"), "{logs}");
}
