/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use super::*;
use crate::mem::PAGE_SIZE;

#[test]
fn context_is_five_guest_words_not_five_host_words() {
    assert_eq!(guest_size_of::<SCNetworkReachabilityContext>(), 20);
    let mut mem = Mem::new();
    mem.set_null_segment_size(PAGE_SIZE);
    let ptr = mem.alloc(20).cast::<SCNetworkReachabilityContext>();
    let words = [0u32, 0x30123450, 0x1001, 0x2001, 0x3001];
    for (i, word) in words.iter().enumerate() {
        mem.write(ptr.cast::<u32>() + i as u32, *word);
    }
    let context = read_context(&mem, ptr.cast_const()).unwrap().unwrap();
    assert_eq!(context.version, 0);
    assert_eq!(context.info.to_bits(), 0x30123450);
    assert_ne!(context.info.to_bits(), ptr.to_bits());
    assert_eq!(context.retain.addr_with_thumb_bit(), 0x1001);
    assert_eq!(context.release.addr_with_thumb_bit(), 0x2001);
    assert_eq!(context.copy_description.addr_with_thumb_bit(), 0x3001);
    // Reusing the caller's stack/storage after registration must be harmless.
    mem.bytes_at_mut(ptr.cast(), 20).fill(0xAA);
    let host = SCNetworkReachabilityHostObject { context: Some(context), ..Default::default() };
    assert_eq!(host.context.unwrap().info.to_bits(), 0x30123450);
}

#[test]
fn null_invalid_and_unsupported_contexts_are_distinguished() {
    let mut mem = Mem::new();
    mem.set_null_segment_size(PAGE_SIZE);
    assert!(read_context(&mem, ConstPtr::null()).unwrap().is_none());
    assert!(read_context(&mem, ConstPtr::from_bits(4)).is_err());
    assert!(read_context(&mem, ConstPtr::from_bits(u32::MAX - 8)).is_err());
    let ptr = mem.alloc_and_write(SCNetworkReachabilityContext {
        version: 1, ..Default::default()
    });
    assert!(read_context(&mem, ptr.cast_const()).is_err());
}

#[test]
fn replace_unregister_and_nested_callbacks_release_each_old_context_once() {
    let context = |info| SCNetworkReachabilityContext {
        info: MutVoidPtr::from_bits(info),
        release: GuestFunction::from_addr_with_thumb_bit(0x2001),
        ..Default::default()
    };
    let callback = Some(GuestFunction::from_addr_with_thumb_bit(0x1001));
    let mut host = SCNetworkReachabilityHostObject::default();
    assert!(host.replace_callback(callback, Some(context(10))).is_none());
    let old = host.replace_callback(callback, Some(context(20))).unwrap();
    assert_eq!(old.info.to_bits(), 10);
    host.active_callbacks = 2;
    assert!(host.replace_callback(callback, Some(context(30))).is_none());
    assert!(host.replace_callback(None, None).is_none());
    assert!(host.callout.is_none());
    assert!(host.context.is_none());
    assert!(host.finish_callback().is_empty());
    let retired = host.finish_callback();
    assert_eq!(retired.iter().map(|c| c.info.to_bits()).collect::<Vec<_>>(), vec![20, 30]);
    assert!(host.retired_contexts.is_empty());
    assert!(host.replace_callback(None, None).is_none());
}

#[test]
fn retain_return_value_is_used_and_null_function_pointers_are_not_called() {
    let mut context = SCNetworkReachabilityContext {
        info: MutVoidPtr::from_bits(0x10000),
        retain: GuestFunction::from_addr_with_thumb_bit(0x1001),
        release: GuestFunction::from_addr_with_thumb_bit(0x2001),
        ..Default::default()
    };
    let mut calls = Vec::new();
    context.retain_with(|callback, info| {
        calls.push((callback.addr_with_thumb_bit(), info.to_bits()));
        MutVoidPtr::from_bits(0x20000)
    });
    assert_eq!(context.info.to_bits(), 0x20000);
    context.release_with(|callback, info| calls.push((callback.addr_with_thumb_bit(), info.to_bits())));
    assert_eq!(calls, vec![(0x1001, 0x10000), (0x2001, 0x20000)]);
    let mut empty = SCNetworkReachabilityContext::default();
    empty.retain_with(|_, _| panic!("must not branch to NULL"));
    empty.release_with(|_, _| panic!("must not branch to NULL"));
}

#[test]
fn local_iap_emulation_reports_reachable_without_real_network_access() {
    assert_eq!(reachability_flags(false, false), 0);
    assert_eq!(
        reachability_flags(false, true),
        kSCNetworkReachabilityFlagsReachable
    );
}

#[test]
fn real_network_access_does_not_depend_on_iap_emulation() {
    assert_eq!(
        reachability_flags(true, false),
        kSCNetworkReachabilityFlagsReachable
    );
    assert_eq!(reachability_flags(true, true), reachability_flags(true, false));
}

#[test]
fn reachable_route_is_wifi_without_connection_or_intervention_requirements() {
    for (network, iap) in [(true, false), (false, true), (true, true)] {
        let flags = reachability_flags(network, iap);
        assert_ne!(flags & kSCNetworkReachabilityFlagsReachable, 0);
        assert_eq!(
            flags & (kSCNetworkReachabilityFlagsConnectionRequired
                | kSCNetworkReachabilityFlagsInterventionRequired
                | kSCNetworkReachabilityFlagsIsWWAN
                | kSCNetworkReachabilityFlagsIsDirect),
            0
        );
    }
}

// SCNetworkReachabilityScheduleWithRunLoop only queues the callout; the run
// loop delivers it later. Calling it inline recursed in Where's My Perry,
// whose callout schedules another target.
// https://developer.apple.com/documentation/systemconfiguration/scnetworkreachabilityschedulewithrunloop(_:_:_:)
#[test]
fn scheduling_queues_one_initial_notification_for_the_run_loop() {
    let callback = GuestFunction::from_addr_with_thumb_bit(0x1001);
    let mut host = SCNetworkReachabilityHostObject::default();
    host.replace_callback(
        Some(callback),
        Some(SCNetworkReachabilityContext {
            info: MutVoidPtr::from_bits(0x30000),
            ..Default::default()
        }),
    );
    assert!(host.schedule());
    // Scheduling again (another mode) before delivery adds no second callout.
    assert!(!host.schedule());
    assert_eq!(host.active_callbacks, 0);
    let (delivered, info) = host.begin_scheduled_callback().unwrap();
    assert_eq!(delivered.addr_with_thumb_bit(), 0x1001);
    assert_eq!(info.to_bits(), 0x30000);
    // A callout that schedules again queues a new notification instead of
    // recursing.
    assert!(host.schedule());
    assert!(host.finish_callback().is_empty());
    assert_eq!(host.active_callbacks, 0);
}

#[test]
fn queued_notification_is_dropped_after_unschedule_or_unregister() {
    let callback = Some(GuestFunction::from_addr_with_thumb_bit(0x1001));
    let mut host = SCNetworkReachabilityHostObject::default();
    host.replace_callback(callback, None);
    assert!(host.schedule());
    host.scheduled = false;
    assert!(host.begin_scheduled_callback().is_none());
    assert!(!host.notification_pending);

    assert!(host.schedule());
    host.replace_callback(None, None);
    assert!(host.begin_scheduled_callback().is_none());
    assert_eq!(host.active_callbacks, 0);
}
