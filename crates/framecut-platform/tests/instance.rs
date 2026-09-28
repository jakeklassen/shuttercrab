//! A second Framecut reaches the running one. In its own test binary so no
//! other test's platform window can receive the signal.
#![cfg(windows)]

use framecut_platform::{Platform, PlatformEvent, signal_running_instance};
use futures::{StreamExt as _, executor::block_on};

#[test]
fn a_second_instance_signals_the_first() {
    let (_platform, mut events, _) = Platform::start(&[], None).unwrap();
    assert!(signal_running_instance());
    assert_eq!(
        block_on(events.next()),
        Some(PlatformEvent::AnotherInstance)
    );
}
