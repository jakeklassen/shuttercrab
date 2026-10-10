//! A second Shuttercrab reaches the running one. In its own test binary so no
//! other test's platform window can receive the signal.
#![cfg(windows)]

use futures::{StreamExt as _, executor::block_on};
use shuttercrab_platform::{Platform, PlatformEvent, Request, signal_running_instance};

#[test]
fn a_second_instance_signals_the_first() {
    let (_platform, mut events, _) = Platform::start(&[], None).unwrap();
    // What the second start asked for arrives with it.
    assert!(signal_running_instance(Request::Screenshot));
    assert_eq!(
        block_on(events.next()),
        Some(PlatformEvent::AnotherInstance(Request::Screenshot))
    );
}
