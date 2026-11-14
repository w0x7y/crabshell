use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use zbus::fdo::RequestNameFlags;
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

use super::{LoopStatus, PATH, PLAYER, PLAYERCTLD, PlaybackStatus, Player, ROOT};
use crate::dbus::{BUS, BUS_PATH, PROPERTIES};
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

const VLC: &str = "org.mpris.MediaPlayer2.vlc";
const FIREFOX: &str = "org.mpris.MediaPlayer2.firefox.instance_1_23";
const TRACK: &str = "/org/videolan/vlc/playlist/7";

fn online(bus: &PrivateBus) -> Config {
  Config {
    session_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

/// A track's metadata, as players send it.
fn metadata(title: &str) -> Variant<'static> {
  let track = ObjectPath::try_from(TRACK).expect("a path");
  Variant::from(HashMap::from([
    ("mpris:trackid", Variant::from(track)),
    ("mpris:length", Variant::from(180_000_000i64)),
    ("mpris:artUrl", Variant::from("file:///tmp/cover.png")),
    ("xesam:title", Variant::from(title.to_owned())),
    ("xesam:artist", Variant::from(vec!["Nina Simone", "Duke"])),
    ("xesam:album", Variant::from("Pastel Blues")),
  ]))
}

/// A player's `Player` properties: paused `seconds` into "Be My Husband".
fn playing_at(seconds: i64) -> Vec<(&'static str, Variant<'static>)> {
  vec![
    ("PlaybackStatus", Variant::from("Paused")),
    ("LoopStatus", Variant::from("Playlist")),
    ("Rate", Variant::from(1.0)),
    ("Shuffle", Variant::from(false)),
    ("Metadata", metadata("Be My Husband")),
    ("Volume", Variant::from(0.5)),
    ("Position", Variant::from(seconds * 1_000_000)),
    ("CanGoNext", Variant::from(true)),
    ("CanGoPrevious", Variant::from(false)),
    ("CanPlay", Variant::from(true)),
    ("CanPause", Variant::from(true)),
    ("CanSeek", Variant::from(true)),
    ("CanControl", Variant::from(true)),
  ]
}

/// A player that answers every call with nothing, on a connection of its
/// own, owning `name` once its objects are there.
fn fake_player(
  shell: &mut Harness,
  bus: &PrivateBus,
  name: &'static str,
  identity: &str,
) -> FakeService {
  let player = FakeService::start(shell, bus, name);
  player.add(
    PATH,
    ROOT,
    &[
      ("Identity", Variant::from(identity.to_owned())),
      ("DesktopEntry", Variant::from("vlc")),
    ],
  );
  player.add(PATH, PLAYER, &playing_at(10));
  player.own(shell);
  player
}

fn wait_for(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  shell.retry_until(what, done, |_| {});
}

fn ids(players: &[Player]) -> Vec<&str> {
  players.iter().map(|player| player.id.as_str()).collect()
}

#[test]
fn a_player_shows_with_its_track() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _vlc = fake_player(&mut shell, &bus, VLC, "VLC media player");
  let asked = Instant::now();
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  let player = players.get_untracked()[0].clone();
  assert_eq!(player.id, VLC);
  assert_eq!(player.identity, "VLC media player");
  assert_eq!(player.desktop_entry, "vlc");
  assert_eq!(player.status, PlaybackStatus::Paused);
  assert_eq!(player.title, "Be My Husband");
  assert_eq!(player.artists, ["Nina Simone", "Duke"]);
  assert_eq!(player.album, "Pastel Blues");
  assert_eq!(player.art_url, "file:///tmp/cover.png");
  assert_eq!(player.length, Some(Duration::from_secs(180)));
  assert!(player.can_play && player.can_pause && player.can_go_next);
  assert!(!player.can_go_previous);
  assert!(player.can_seek && player.can_control);
  assert_eq!(player.volume, Some(0.5));
  assert_eq!(player.shuffle, Some(false));
  assert_eq!(player.loop_status, Some(LoopStatus::Playlist));
  assert_eq!(player.rate, 1.0);
  assert_eq!(player.position, Duration::from_secs(10));
  assert!(player.position_known_at >= asked);
}

#[test]
fn players_show_in_the_order_they_appeared_and_go_when_they_leave() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "VLC", || players.get_untracked().len() == 1);

  let _firefox = fake_player(&mut shell, &bus, FIREFOX, "Firefox");
  wait_for(&mut shell, "Firefox", || players.get_untracked().len() == 2);
  assert_eq!(ids(&players.get_untracked()), [VLC, FIREFOX]);

  vlc.leave(&mut shell);
  wait_for(&mut shell, "VLC gone", || {
    players.get_untracked().len() == 1
  });
  assert_eq!(ids(&players.get_untracked()), [FIREFOX]);

  let _vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  wait_for(&mut shell, "VLC again", || {
    players.get_untracked().len() == 2
  });
  assert_eq!(ids(&players.get_untracked()), [FIREFOX, VLC]);
}

#[test]
fn a_name_passed_to_another_program_is_read_from_it() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let old = fake_player(&mut shell, &bus, VLC, "Old");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the old player", || {
    !players.get_untracked().is_empty()
  });

  let new = FakeService::start(&mut shell, &bus, VLC);
  new.add(PATH, ROOT, &[("Identity", Variant::from("New"))]);
  new.add(PATH, PLAYER, &playing_at(0));
  let queued = new.connection.clone();
  shell
    .block_on(async move {
      queued
        .request_name_with_flags(VLC, RequestNameFlags::empty())
        .await
    })
    .expect("in the queue");
  let old_connection = old.connection.clone();
  old.leave(&mut shell);
  wait_for(&mut shell, "the new player", || {
    players.get_untracked().first().map(|p| p.identity.as_str()) == Some("New")
  });

  // The old owner's word no longer counts.
  let changed = HashMap::from([("Identity", Variant::from("Stale"))]);
  shell
    .block_on(async move {
      let body = (ROOT, changed, Vec::<String>::new());
      let signal = "PropertiesChanged";
      old_connection
        .emit_signal(None::<&str>, PATH, PROPERTIES, signal, &body)
        .await
    })
    .expect("the old owner's signal");
  new.set(
    &mut shell,
    PATH,
    ROOT,
    &[("DesktopEntry", Variant::from("new"))],
  );
  wait_for(&mut shell, "the new owner's change", || {
    players.get_untracked()[0].desktop_entry == "new"
  });
  assert_eq!(players.get_untracked()[0].identity, "New");
}

#[test]
fn changes_show_and_a_new_status_rate_or_track_reads_the_position() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  let player = move || players.get_untracked()[0].clone();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });

  // The position moves without a word, as MPRIS has it.
  vlc.add(PATH, PLAYER, &playing_at(20));
  vlc.set(
    &mut shell,
    PATH,
    PLAYER,
    &[("PlaybackStatus", Variant::from("Playing"))],
  );
  wait_for(&mut shell, "playing at 20 s", || {
    player().position == Duration::from_secs(20)
  });
  assert_eq!(player().status, PlaybackStatus::Playing);

  vlc.add(PATH, PLAYER, &playing_at(30));
  vlc.set(&mut shell, PATH, PLAYER, &[("Rate", Variant::from(2.0))]);
  wait_for(&mut shell, "2× at 30 s", || {
    player().position == Duration::from_secs(30)
  });
  assert_eq!(player().rate, 2.0);

  vlc.add(PATH, PLAYER, &playing_at(0));
  vlc.set(
    &mut shell,
    PATH,
    PLAYER,
    &[("Metadata", metadata("Sinnerman"))],
  );
  wait_for(&mut shell, "the next track", || {
    player().title == "Sinnerman"
  });
  wait_for(&mut shell, "its start", || player().position.is_zero());

  vlc.add(PATH, PLAYER, &playing_at(40));
  vlc.set(
    &mut shell,
    PATH,
    PLAYER,
    &[
      ("Volume", Variant::from(0.25)),
      ("LoopStatus", Variant::from("Track")),
      ("Shuffle", Variant::from(true)),
    ],
  );
  vlc.set(
    &mut shell,
    PATH,
    ROOT,
    &[("Identity", Variant::from("VLC 4"))],
  );
  wait_for(&mut shell, "the new identity", || {
    player().identity == "VLC 4"
  });
  assert_eq!(player().volume, Some(0.25));
  assert_eq!(player().loop_status, Some(LoopStatus::Track));
  assert_eq!(player().shuffle, Some(true));
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert!(player().position.is_zero(), "a volume change reads nothing");

  // One that says only that Metadata changed gets it read again.
  let changed = HashMap::<String, OwnedVariant>::new();
  let body = (PLAYER, changed, vec!["Metadata".to_owned()]);
  vlc.emit(&mut shell, PATH, PROPERTIES, "PropertiesChanged", body);
  wait_for(&mut shell, "the track read again", || {
    player().title == "Be My Husband"
  });
}

#[test]
fn seeked_moves_the_position() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  let before = Instant::now();
  vlc.emit(&mut shell, PATH, PLAYER, "Seeked", 95_000_000i64);
  wait_for(&mut shell, "the seek", || {
    players.get_untracked()[0].position == Duration::from_secs(95)
  });
  assert!(players.get_untracked()[0].position_known_at >= before);
}

/// A player playing at 1× from 10 s into a 60 s track, known at `at`.
fn sample(at: Instant) -> Player {
  Player {
    id: VLC.into(),
    identity: String::new(),
    desktop_entry: String::new(),
    status: PlaybackStatus::Playing,
    title: String::new(),
    artists: Vec::new(),
    album: String::new(),
    art_url: String::new(),
    length: Some(Duration::from_secs(60)),
    can_play: true,
    can_pause: true,
    can_go_next: true,
    can_go_previous: true,
    can_seek: true,
    can_control: true,
    volume: None,
    shuffle: None,
    loop_status: None,
    rate: 1.0,
    position: Duration::from_secs(10),
    position_known_at: at,
  }
}

#[test]
fn the_position_moves_at_the_rate_while_playing_and_stops_at_the_end() {
  let at = Instant::now();
  let later = |seconds| at + Duration::from_secs(seconds);
  let player = sample(at);
  assert_eq!(player.position_at(later(5)), Duration::from_secs(15));
  assert_eq!(player.position_at(later(500)), Duration::from_secs(60));
  let earlier = at.checked_sub(Duration::from_secs(5)).expect("an instant");
  assert_eq!(player.position_at(earlier), Duration::from_secs(10));
  let fast = Player {
    rate: 2.0,
    ..sample(at)
  };
  assert_eq!(fast.position_at(later(5)), Duration::from_secs(20));
  let paused = Player {
    status: PlaybackStatus::Paused,
    ..sample(at)
  };
  assert_eq!(paused.position_at(later(5)), Duration::from_secs(10));
  let stream = Player {
    length: None,
    ..sample(at)
  };
  assert_eq!(stream.position_at(later(500)), Duration::from_secs(510));
}

#[test]
fn methods_reach_the_player_and_a_refusal_is_logged() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let seen = Rc::new(RefCell::new(Vec::new()));
  for member in [
    "PlayPause",
    "Play",
    "Pause",
    "Stop",
    "Previous",
    "Seek",
    "SetPosition",
    "Raise",
  ] {
    let seen = Rc::clone(&seen);
    vlc.on(member, move |call| {
      let header = call.header();
      let interface = header.interface().map(|i| i.to_string());
      let body = match member {
        "Seek" => format!("{:?}", call.body().deserialize::<i64>().ok()),
        "SetPosition" => format!(
          "{:?}",
          call
            .body()
            .deserialize::<(OwnedObjectPath, i64)>()
            .ok()
            .map(|(track, at)| (track.to_string(), at))
        ),
        _ => String::new(),
      };
      seen
        .borrow_mut()
        .push(format!("{} {member} {body}", interface.unwrap_or_default()));
      reply(call, &())
    });
  }
  let mpris = shell.cx.mpris();
  let players = mpris.players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });

  mpris.play_pause(VLC);
  mpris.play(VLC);
  mpris.pause(VLC);
  mpris.stop(VLC);
  mpris.previous(VLC);
  mpris.seek(VLC, -2.5);
  mpris.seek(VLC, f64::NAN);
  mpris.set_position(VLC, Duration::from_millis(30_500));
  mpris.raise(VLC);
  mpris.set_volume(VLC, 0.75);
  mpris.set_shuffle(VLC, true);
  mpris.set_loop(VLC, LoopStatus::None);
  wait_for(&mut shell, "every call", || {
    let player = &players.get_untracked()[0];
    seen.borrow().len() == 8
      && player.volume == Some(0.75)
      && player.shuffle == Some(true)
      && player.loop_status == Some(LoopStatus::None)
  });
  let mut seen = seen.borrow().clone();
  seen.sort();
  assert_eq!(
    seen,
    [
      format!("{ROOT} Raise "),
      format!("{PLAYER} Pause "),
      format!("{PLAYER} Play "),
      format!("{PLAYER} PlayPause "),
      format!("{PLAYER} Previous "),
      format!("{PLAYER} Seek Some(-2500000)"),
      format!("{PLAYER} SetPosition Some((\"{TRACK}\", 30500000))"),
      format!("{PLAYER} Stop "),
    ]
  );
  let mut sets = vlc.calls.borrow().clone();
  sets.retain(|call| call.starts_with("Set "));
  sets.sort();
  assert_eq!(
    sets,
    [
      format!("Set {PATH} LoopStatus"),
      format!("Set {PATH} Shuffle"),
      format!("Set {PATH} Volume"),
    ]
  );

  let logs = capture_logs(|| {
    // Next has no handler, so the fake says there's no such method.
    mpris.next(VLC);
    mpris.play("org.mpris.MediaPlayer2.nobody");
    let calls = Rc::clone(&vlc.calls);
    wait_for(&mut shell, "the call to Next", || {
      calls.borrow().iter().any(|call| call.starts_with("Next "))
    });
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  assert!(logs.contains("the media player refused"), "{logs}");
  assert!(logs.contains("no such media player"), "{logs}");
}

#[test]
fn playerctld_is_left_out() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _playerctld = fake_player(&mut shell, &bus, PLAYERCTLD, "playerctld");
  let players = shell.cx.mpris().players();
  let _vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  wait_for(&mut shell, "VLC", || !players.get_untracked().is_empty());
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(ids(&players.get_untracked()), [VLC]);
}

#[test]
fn signals_from_another_program_are_ignored() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  // Not the player, but saying it changed, to everyone and aimed at
  // every connection on the bus, the shell's too.
  let impostor = shell.block_on(bus.connect(&[]));
  shell
    .block_on(async move {
      let call = impostor.call_method(Some(BUS), BUS_PATH, Some(BUS), "ListNames", &());
      let names: Vec<String> = call.await?.body().deserialize()?;
      let unique = names.iter().filter(|name| name.starts_with(':'));
      for to in unique.map(Some).chain([None]) {
        let to = to.map(String::as_str);
        let changed = HashMap::from([("PlaybackStatus", Variant::from("Playing"))]);
        let body = (PLAYER, changed, Vec::<String>::new());
        let signal = "PropertiesChanged";
        impostor
          .emit_signal(to, PATH, PROPERTIES, signal, &body)
          .await?;
        impostor
          .emit_signal(to, PATH, PLAYER, "Seeked", &1_000_000i64)
          .await?;
      }
      zbus::Result::Ok(())
    })
    .expect("the impostor's signals");
  shell.run_until(Instant::now() + Duration::from_millis(200));
  let player = players.get_untracked()[0].clone();
  assert_eq!(player.status, PlaybackStatus::Paused);
  assert_eq!(player.position, Duration::from_secs(10));
}

#[test]
fn properties_of_the_wrong_type_read_as_defaults() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = FakeService::start(&mut shell, &bus, VLC);
  vlc.add(PATH, ROOT, &[("Identity", Variant::from(7u32))]);
  let track = HashMap::from([
    ("xesam:title", Variant::from(7u32)),
    ("xesam:artist", Variant::from("one artist, not a list")),
    ("mpris:length", Variant::from(90_000_000u64)),
    ("mpris:trackid", Variant::from("/not/a/path")),
  ]);
  vlc.add(
    PATH,
    PLAYER,
    &[
      ("PlaybackStatus", Variant::from(1u32)),
      ("LoopStatus", Variant::from("Forever")),
      ("Rate", Variant::from("fast")),
      ("Shuffle", Variant::from("yes")),
      ("Metadata", Variant::from(track)),
      ("Volume", Variant::from(f64::NAN)),
      ("Position", Variant::from(-5i64)),
      ("CanPlay", Variant::from(1u32)),
    ],
  );
  vlc.own(&mut shell);
  let mpris = shell.cx.mpris();
  let players = mpris.players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  let player = players.get_untracked()[0].clone();
  assert_eq!(player.identity, "");
  assert_eq!(player.status, PlaybackStatus::Stopped);
  assert_eq!(player.loop_status, None);
  assert_eq!(player.rate, 1.0);
  assert_eq!(player.shuffle, None);
  assert_eq!(player.title, "");
  assert_eq!(player.artists, Vec::<String>::new());
  assert_eq!(
    player.length,
    Some(Duration::from_secs(90)),
    "a u64 is fine"
  );
  assert_eq!(player.volume, None);
  assert_eq!(player.position, Duration::ZERO);
  assert!(!player.can_play);

  let logs = capture_logs(|| mpris.set_position(VLC, Duration::from_secs(1)));
  assert!(logs.contains("names no track"), "{logs}");

  vlc.set(
    &mut shell,
    PATH,
    PLAYER,
    &[("Metadata", Variant::from("not a dict"))],
  );
  wait_for(&mut shell, "no length", || {
    players.get_untracked()[0].length.is_none()
  });
}

#[test]
fn players_artists_and_text_are_capped() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let player = FakeService::start(&mut shell, &bus, "org.mpris.MediaPlayer2.cap0");
  player.add(PATH, ROOT, &[("Identity", Variant::from("x".repeat(5000)))]);
  // 4095 bytes, then a 2-byte é across the cap.
  let title = format!("{}é", "a".repeat(4095));
  let artists: Vec<String> = (0..40).map(|n| format!("artist {n}")).collect();
  let track = HashMap::from([
    ("xesam:title", Variant::from(title)),
    ("xesam:artist", Variant::from(artists)),
    (
      "mpris:artUrl",
      Variant::from(format!("data:{}", "A".repeat(5000))),
    ),
  ]);
  player.add(PATH, PLAYER, &[("Metadata", Variant::from(track))]);
  player.own(&mut shell);
  let connection = player.connection.clone();
  shell
    .block_on(async move {
      for n in 1..33 {
        let name = format!("org.mpris.MediaPlayer2.cap{n}");
        connection.request_name(name.as_str()).await?;
      }
      zbus::Result::Ok(())
    })
    .expect("33 names");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "32 players", || {
    players.get_untracked().len() == 32
  });
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(players.get_untracked().len(), 32);
  let first = players.get_untracked()[0].clone();
  assert_eq!(first.identity.len(), 4096);
  assert_eq!(first.title, "a".repeat(4095));
  assert_eq!(first.artists.len(), 32);
  assert_eq!(first.art_url, "", "a cut URL would point elsewhere");
}

#[test]
fn an_idle_player_wakes_nothing_and_no_session_bus_is_quiet() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let players = shell.cx.mpris().players();
    assert_eq!(
      shell.run_until(Instant::now() + Duration::from_millis(200)),
      1
    );
    assert_eq!(players.get_untracked(), []);
  });
  assert!(logs.contains("no session bus"), "{logs}");
}

#[test]
fn without_provide_the_players_are_empty_under_its_context() {
  let cx = Cx::new(Runtime::current().new_owner());
  let mpris = cx.mpris();
  assert_eq!(mpris.players().get_untracked(), []);
  mpris.play_pause(VLC);
  cx.owner().dispose();
  assert_eq!(mpris.players().try_get(), None);
}

#[test]
fn disposing_the_root_frees_the_players() {
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  shell.cx.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  // A signal after that finds the list gone, and nothing panics.
  vlc.emit(&mut shell, PATH, PLAYER, "Seeked", 1i64);
  shell.run_until(Instant::now() + Duration::from_millis(100));
}

#[test]
fn a_pause_shows_where_it_stopped_before_the_position_is_read() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let vlc = fake_player(&mut shell, &bus, VLC, "VLC");
  let players = shell.cx.mpris().players();
  wait_for(&mut shell, "the player", || {
    !players.get_untracked().is_empty()
  });
  vlc.set(
    &mut shell,
    PATH,
    PLAYER,
    &[("PlaybackStatus", Variant::from("Playing"))],
  );
  let playing = || players.get_untracked()[0].status == PlaybackStatus::Playing;
  wait_for(&mut shell, "playing", playing);
  // Its position reads stay unanswered for now.
  vlc.hold();
  shell.run_until(Instant::now() + Duration::from_millis(400));
  vlc.set(
    &mut shell,
    PATH,
    PLAYER,
    &[("PlaybackStatus", Variant::from("Paused"))],
  );
  wait_for(&mut shell, "paused", || !playing());
  let shown = players.get_untracked()[0].position_at(Instant::now());
  assert!(
    shown >= Duration::from_millis(10_300),
    "{shown:?}, not where it stopped"
  );
  vlc.release(&mut shell);
}
