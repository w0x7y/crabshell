//! The service against a real PipeWire of its own. Ignored, since they
//! need `pipewire` and `pw-link`; run them by hand:
//!
//! ```sh
//! cargo test -p crabshell-services --lib pipewire::tests::real -- --ignored --test-threads=1
//! ```

use std::time::{Duration, Instant};

use crate::testing::{Harness, offline};
use crate::{Config, PwLinkState, ServicesExt};

#[path = "../../../tests/pipewire/mod.rs"]
#[allow(dead_code, reason = "the audio tests use the rest")]
#[allow(
  clippy::duplicate_mod,
  reason = "the audio tests include it too, privately"
)]
mod server;

use server::PrivateServer;

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_link_made_with_pw_link_shows_goes_and_comes_back() {
  let server = PrivateServer::start("graph");
  let mut shell = Harness::new(Config {
    pipewire: Some(server.pipewire_socket()),
    ..offline()
  });
  let pipewire = shell.cx.pipewire();
  let (nodes, links, groups) = (pipewire.nodes(), pipewire.links(), pipewire.link_groups());
  let named = move |name: &str| {
    let nodes = nodes.get_untracked();
    nodes.into_iter().find(|node| node.name == name)
  };
  shell.retry_until("the sinks", || named("other-sink").is_some(), |_| {});
  let (from, to) = (
    named("test-sink").unwrap().id,
    named("other-sink").unwrap().id,
  );
  let source = named("test-source").expect("the source");
  assert_eq!(source.description, "Test Source");
  assert_eq!(source.media_class, "Audio/Source/Virtual");
  assert!(!source.is_stream);

  let ports = ["test-sink:monitor_FL", "other-sink:playback_FL"];
  let linked = move || {
    let links = links.get_untracked();
    links
      .iter()
      .any(|link| (link.output_node, link.input_node) == (from, to))
  };
  // Twice, so the second bind takes the proxy id PipeWire gave back.
  for round in 0..2 {
    server.pipewire_tool("pw-link", &ports);
    shell.retry_until("the link", linked, |_| {});
    let groups = groups.get_untracked();
    let group = groups.iter().find(|group| group.output_node == from);
    let group = group.expect("its group");
    assert_eq!(group.input_node, to);
    assert!(group.state >= PwLinkState::Init, "{:?}", group.state);
    if round == 0 {
      // Let the link settle, then nothing wakes the shell.
      shell.settle();
      let wakeups = shell.run_until(Instant::now() + Duration::from_secs(2));
      assert_eq!(wakeups, 1, "only the run's own deadline");
    }
    server.pipewire_tool("pw-link", &["-d", ports[0], ports[1]]);
    shell.retry_until("the link gone", || !linked(), |_| {});
  }
}
