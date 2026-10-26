/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![allow(dead_code)]
//! SCNetworkReachability

use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::{export_c_func, FunctionExports};
use crate::frameworks::core_foundation::cf_allocator::CFAllocatorRef;
use crate::frameworks::core_foundation::{CFRelease, CFRetain, CFTypeRef};
use crate::frameworks::foundation::ns_run_loop;
use crate::mem::{guest_size_of, ConstPtr, GuestISize, Mem, MutPtr, MutVoidPtr, SafeRead};
use crate::objc::{id, msg_class, nil, objc_classes, ClassExports, HostObject};
use crate::Environment;

type SCNetworkReachabilityFlags = u32;
const kSCNetworkReachabilityFlagsTransientConnection: SCNetworkReachabilityFlags = 1 << 0;
const kSCNetworkReachabilityFlagsReachable: SCNetworkReachabilityFlags = 1 << 1;
const kSCNetworkReachabilityFlagsConnectionRequired: SCNetworkReachabilityFlags = 1 << 2;
const kSCNetworkReachabilityFlagsConnectionOnTraffic: SCNetworkReachabilityFlags = 1 << 3;
const kSCNetworkReachabilityFlagsInterventionRequired: SCNetworkReachabilityFlags = 1 << 4;
const kSCNetworkReachabilityFlagsConnectionOnDemand: SCNetworkReachabilityFlags = 1 << 5;
const kSCNetworkReachabilityFlagsIsLocalAddress: SCNetworkReachabilityFlags = 1 << 16;
const kSCNetworkReachabilityFlagsIsDirect: SCNetworkReachabilityFlags = 1 << 17;
const kSCNetworkReachabilityFlagsIsWWAN: SCNetworkReachabilityFlags = 1 << 18;

pub const CLASSES: ClassExports = objc_classes! {
    (env, this, _cmd);
    @implementation _touchHLE_SCNetworkReachability: NSObject
    - (())dealloc {
        let (context, retired) = {
            let host = env.objc.borrow_mut::<SCNetworkReachabilityHostObject>(this);
            host.callout = None;
            (host.context.take(), std::mem::take(&mut host.retired_contexts))
        };
        env.objc.dealloc_object(this, &mut env.mem);
        release_context(env, context);
        for context in retired {
            release_context(env, Some(context));
        }
    }

    // Queued on the run loop by SCNetworkReachabilityScheduleWithRunLoop.
    - (())_touchHLE_deliverScheduledCallback {
        deliver_scheduled_callback(env, this);
    }
    @end
};

/// iOS/ARM32 ABI: CFIndex followed by info and three function pointers.
/// The caller may put this on its stack. Store a COPY, never that stack address.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct SCNetworkReachabilityContext {
    version: GuestISize,
    info: MutVoidPtr,
    retain: GuestFunction,
    release: GuestFunction,
    copy_description: GuestFunction,
}
unsafe impl SafeRead for SCNetworkReachabilityContext {}

impl SCNetworkReachabilityContext {
    fn retain_with(&mut self, mut call: impl FnMut(GuestFunction, MutVoidPtr) -> MutVoidPtr) {
        if self.retain.addr_with_thumb_bit() != 0 {
            self.info = call(self.retain, self.info);
        }
    }

    fn release_with(self, mut call: impl FnMut(GuestFunction, MutVoidPtr)) {
        if self.release.addr_with_thumb_bit() != 0 {
            call(self.release, self.info);
        }
    }
}

fn read_context(
    mem: &Mem,
    ptr: ConstPtr<SCNetworkReachabilityContext>,
) -> Result<Option<SCNetworkReachabilityContext>, &'static str> {
    if ptr.is_null() {
        return Ok(None);
    }
    let size = guest_size_of::<SCNetworkReachabilityContext>();
    let end = ptr.to_bits() as u64 + size as u64;
    // get_bytes_fallible can return a SHORT synthetic null-page slice.
    if ptr.to_bits() < mem.null_segment_size()
        || end > u32::MAX as u64 + 1
        || mem.get_bytes_fallible(ptr.cast(), size).map(|b| b.len()) != Some(size as usize)
    {
        return Err("unreadable context");
    }
    let context: SCNetworkReachabilityContext = mem.read(ptr);
    if context.version != 0 {
        return Err("unsupported context version");
    }
    Ok(Some(context))
}

fn retain_context(env: &mut Environment, context: &mut Option<SCNetworkReachabilityContext>) {
    if let Some(context) = context {
        // Retain may return a different pointer: use it for callout/release.
        context.retain_with(|callback, info| callback.call_from_host(env, (info,)));
    }
}

fn release_context(env: &mut Environment, context: Option<SCNetworkReachabilityContext>) {
    if let Some(context) = context {
        context.release_with(|callback, info| {
            let _: () = callback.call_from_host(env, (info,));
        });
    }
}

#[derive(Default)]
struct SCNetworkReachabilityHostObject {
    name: Option<String>,
    callout: Option<GuestFunction>,
    context: Option<SCNetworkReachabilityContext>,
    active_callbacks: usize,
    retired_contexts: Vec<SCNetworkReachabilityContext>,
    /// Scheduled on a run loop and not unscheduled since.
    scheduled: bool,
    /// The initial notification is queued on the run loop.
    notification_pending: bool,
}
impl HostObject for SCNetworkReachabilityHostObject {}

impl SCNetworkReachabilityHostObject {
    /// Defer release if a callback replaces/unregisters its own context.
    fn replace_callback(
        &mut self,
        callout: Option<GuestFunction>,
        context: Option<SCNetworkReachabilityContext>,
    ) -> Option<SCNetworkReachabilityContext> {
        self.callout = callout;
        let old = std::mem::replace(&mut self.context, context);
        if self.active_callbacks == 0 {
            return old;
        }
        if let Some(old) = old {
            self.retired_contexts.push(old);
        }
        None
    }

    /// Returns true when the caller must queue the initial notification.
    fn schedule(&mut self) -> bool {
        self.scheduled = true;
        !std::mem::replace(&mut self.notification_pending, true)
    }

    /// Runs on the run loop. The callback may have been unregistered or the
    /// target unscheduled since the notification was queued.
    fn begin_scheduled_callback(&mut self) -> Option<(GuestFunction, MutVoidPtr)> {
        self.notification_pending = false;
        if !self.scheduled {
            return None;
        }
        let callout = self.callout?;
        self.active_callbacks += 1;
        Some((callout, self.context.map_or(MutVoidPtr::null(), |c| c.info)))
    }

    fn finish_callback(&mut self) -> Vec<SCNetworkReachabilityContext> {
        self.active_callbacks -= 1;
        if self.active_callbacks == 0 {
            std::mem::take(&mut self.retired_contexts)
        } else {
            Vec::new()
        }
    }
}

fn is_reachability(env: &Environment, target: SCNetworkReachabilityRef) -> bool {
    env.objc
        .get_host_object(target)
        .is_some_and(|host| host.as_any().is::<SCNetworkReachabilityHostObject>())
}

type SCNetworkReachabilityRef = CFTypeRef;

pub fn SCNetworkReachabilityRetain(
    env: &mut Environment,
    target: SCNetworkReachabilityRef,
) -> SCNetworkReachabilityRef {
    if !target.is_null() {
        CFRetain(env, target)
    } else {
        target
    }
}

pub fn SCNetworkReachabilityRelease(env: &mut Environment, target: SCNetworkReachabilityRef) {
    if !target.is_null() {
        CFRelease(env, target);
    }
}

fn SCNetworkReachabilityCreateWithName(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    name: ConstPtr<u8>,
) -> SCNetworkReachabilityRef {
    let name_str = env.mem.cstr_at_utf8(name).unwrap_or("").to_string();
    let isa = env
        .objc
        .get_known_class("_touchHLE_SCNetworkReachability", &mut env.mem);
    env.objc.alloc_object(
        isa,
        Box::new(SCNetworkReachabilityHostObject {
            name: Some(name_str),
            ..Default::default()
        }),
        &mut env.mem,
    )
}

fn SCNetworkReachabilityCreateWithAddress(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    _address: ConstPtr<u8>,
) -> SCNetworkReachabilityRef {
    let isa = env
        .objc
        .get_known_class("_touchHLE_SCNetworkReachability", &mut env.mem);
    env.objc.alloc_object(
        isa,
        Box::new(SCNetworkReachabilityHostObject {
            name: None,
            ..Default::default()
        }),
        &mut env.mem,
    )
}

fn SCNetworkReachabilityCreateWithAddressPair(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    _local: ConstPtr<u8>,
    _remote: ConstPtr<u8>,
) -> SCNetworkReachabilityRef {
    let isa = env
        .objc
        .get_known_class("_touchHLE_SCNetworkReachability", &mut env.mem);
    env.objc.alloc_object(
        isa,
        Box::new(SCNetworkReachabilityHostObject {
            name: None,
            ..Default::default()
        }),
        &mut env.mem,
    )
}

/// Report a Wi-Fi-style route for real networking or local StoreKit emulation.
/// This is only a reachability hint: it must never enable sockets or HTTP.
/// Unity and common Reachability wrappers check Reachable and ConnectionRequired
/// before attempting StoreKit calls, even if the purchase itself is local.
fn reachability_flags(network_access: bool, iap_emulation: bool) -> SCNetworkReachabilityFlags {
    if network_access || iap_emulation {
        kSCNetworkReachabilityFlagsReachable
    } else {
        0
    }
}

fn current_flags(env: &Environment) -> SCNetworkReachabilityFlags {
    reachability_flags(
        env.options.network_access,
        crate::frameworks::store_kit::emulation_enabled(),
    )
}

fn SCNetworkReachabilityGetFlags(
    env: &mut Environment,
    _target: SCNetworkReachabilityRef,
    flags: MutPtr<SCNetworkReachabilityFlags>,
) -> bool {
    let flags_value = current_flags(env);
    if !flags.is_null() {
        env.mem.write(flags, flags_value);
    }
    true
}

fn SCNetworkReachabilitySetCallback(
    env: &mut Environment,
    target: SCNetworkReachabilityRef,
    callout: GuestFunction,
    context: ConstPtr<SCNetworkReachabilityContext>,
) -> bool {
    if !is_reachability(env, target) {
        return false;
    }
    let callout = (callout.addr_with_thumb_bit() != 0).then_some(callout);
    let mut context = if callout.is_none() {
        // NULL callout unregisters the callback; do not retain a new context.
        None
    } else {
        match read_context(&env.mem, context) {
            Ok(context) => context,
            Err(reason) => {
                log!(
                    "SCNetworkReachabilitySetCallback: {}; keeping previous callback",
                    reason
                );
                return false;
            }
        }
    };
    // Guest retain/release can re-enter the runtime. Keep the target alive
    // and never hold a host-object borrow across guest execution.
    CFRetain(env, target);
    retain_context(env, &mut context);
    let old = env
        .objc
        .borrow_mut::<SCNetworkReachabilityHostObject>(target)
        .replace_callback(callout, context);
    release_context(env, old);
    CFRelease(env, target);
    true
}

fn SCNetworkReachabilityScheduleWithRunLoop(
    env: &mut Environment,
    target: SCNetworkReachabilityRef,
    run_loop: CFTypeRef,
    run_loop_mode: CFTypeRef,
) -> bool {
    if !is_reachability(env, target) || run_loop.is_null() || run_loop_mode.is_null() {
        return false;
    }
    // The callout is a run-loop event, never called from inside this call:
    // https://developer.apple.com/documentation/systemconfiguration/scnetworkreachabilityschedulewithrunloop(_:_:_:)
    // Wrappers such as Disney's EasyReachability schedule a new target from
    // their callout, so calling it here recursed without bound.
    // There are no network-change events yet, so deliver one initial
    // notification, like iOS does once a host name target resolves.
    if env
        .objc
        .borrow_mut::<SCNetworkReachabilityHostObject>(target)
        .schedule()
    {
        let modes: id = msg_class![env; NSArray arrayWithObject:run_loop_mode];
        let selector = env
            .objc
            .lookup_selector("_touchHLE_deliverScheduledCallback")
            .unwrap();
        // The request retains the target until it is delivered.
        ns_run_loop::add_perform_request(env, run_loop, target, selector, nil, false, modes);
    }
    true
}

fn deliver_scheduled_callback(env: &mut Environment, target: SCNetworkReachabilityRef) {
    let Some((callback, info)) = env
        .objc
        .borrow_mut::<SCNetworkReachabilityHostObject>(target)
        .begin_scheduled_callback()
    else {
        return;
    };
    // Match GetFlags: do not invent a cellular/direct connection here.
    let flags = current_flags(env);
    let _: () = callback.call_from_host(env, (target, flags, info));
    let retired = env
        .objc
        .borrow_mut::<SCNetworkReachabilityHostObject>(target)
        .finish_callback();
    for context in retired {
        release_context(env, Some(context));
    }
}

fn SCNetworkReachabilityUnscheduleFromRunLoop(
    env: &mut Environment,
    target: SCNetworkReachabilityRef,
    run_loop: CFTypeRef,
    run_loop_mode: CFTypeRef,
) -> bool {
    if !is_reachability(env, target) || run_loop.is_null() || run_loop_mode.is_null() {
        return false;
    }
    // A queued initial notification is dropped when delivered.
    env.objc
        .borrow_mut::<SCNetworkReachabilityHostObject>(target)
        .scheduled = false;
    true
}
fn SCNetworkReachabilitySetDispatchQueue(
    _env: &mut Environment,
    _target: SCNetworkReachabilityRef,
    _queue: MutVoidPtr,
) -> bool {
    false
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(SCNetworkReachabilityRetain(_)),
    export_c_func!(SCNetworkReachabilityRelease(_)),
    export_c_func!(SCNetworkReachabilityCreateWithName(_, _)),
    export_c_func!(SCNetworkReachabilityCreateWithAddress(_, _)),
    export_c_func!(SCNetworkReachabilityCreateWithAddressPair(_, _, _)),
    export_c_func!(SCNetworkReachabilityGetFlags(_, _)),
    export_c_func!(SCNetworkReachabilitySetCallback(_, _, _)),
    export_c_func!(SCNetworkReachabilityScheduleWithRunLoop(_, _, _)),
    export_c_func!(SCNetworkReachabilityUnscheduleFromRunLoop(_, _, _)),
    export_c_func!(SCNetworkReachabilitySetDispatchQueue(_, _)),
];

#[cfg(test)]
mod tests;
