//! Window descriptions and contexts.

use crabshell_reactive::Runtime;

use crate::{Anchors, Cx, Edge};

#[test]
fn anchors_name_the_edge_a_bar_sits_on() {
    assert_eq!(Anchors::from(Edge::Top).edge(), Some(Edge::Top));
    assert_eq!(
        (Edge::Bottom | Edge::Left | Edge::Right).edge(),
        Some(Edge::Bottom)
    );
    assert_eq!(
        (Edge::Top | Edge::Bottom | Edge::Left).edge(),
        Some(Edge::Left)
    );
    assert_eq!(
        (Edge::Top | Edge::Bottom | Edge::Right).edge(),
        Some(Edge::Right)
    );
    assert_eq!((Edge::Top | Edge::Bottom).edge(), None);
    assert_eq!(
        (Edge::Top | Edge::Bottom | Edge::Left | Edge::Right).edge(),
        None
    );
    assert_eq!(Anchors::NONE.edge(), None);
    assert!((Edge::Top | Edge::Left).contains(Edge::Left));
    assert!(!(Edge::Top | Edge::Left).contains(Edge::Right));
}

#[test]
fn a_child_context_belongs_to_its_parent() {
    let root = Cx::new(Runtime::current().new_owner());
    let parent = root.child();
    let child = parent.child().named("child");
    let value = child.signal(1);
    parent.owner().dispose();
    assert_eq!(value.try_get(), None);
}
