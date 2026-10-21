//! The source policy can be replaced while the transport runs (M4a, K07): readers see the new
//! policy from the next request on, and a snapshot taken earlier is unaffected.

use sip_core::transport::{Decision, PolicyCell, PolicyMode, SourcePolicy, TransportHandle};

#[test]
fn a_replaced_policy_applies_to_later_decisions_only() {
    let ip = "10.1.2.3".parse().unwrap();
    let cell = PolicyCell::new(SourcePolicy::parse(&["192.0.2.0/24"], PolicyMode::Drop).unwrap());
    let other = cell.clone();
    let before = cell.get();
    assert_ne!(before.decide(ip), Decision::Allow);
    other.set(SourcePolicy::parse(&["10.0.0.0/8"], PolicyMode::Drop).unwrap());
    assert_eq!(
        cell.get().decide(ip),
        Decision::Allow,
        "clones share the policy"
    );
    assert_ne!(
        before.decide(ip),
        Decision::Allow,
        "the snapshot taken earlier is unchanged"
    );
}

#[test]
fn the_transport_handle_exposes_the_policy() {
    let d = TransportHandle::detached("192.0.2.1:5060".parse().unwrap());
    let ip = "203.0.113.5".parse().unwrap();
    assert_eq!(d.handle.source_policy().decide(ip), Decision::Allow);
    d.handle
        .set_source_policy(SourcePolicy::parse(&["10.0.0.0/8"], PolicyMode::Reject).unwrap());
    assert_ne!(d.handle.source_policy().decide(ip), Decision::Allow);
}
