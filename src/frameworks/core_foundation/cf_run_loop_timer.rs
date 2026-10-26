/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFRunLoopTimer`.
//!
//! It's toll-free bridged with `NSTimer`.

use crate::abi::CallFromHost;
use crate::abi::GuestFunction;
use crate::dyld::{export_c_func, FunctionExports};
use crate::frameworks::core_foundation::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use crate::frameworks::core_foundation::time::{CFAbsoluteTime, CFTimeInterval};
use crate::frameworks::core_foundation::CFIndex;
use crate::mem::{ConstVoidPtr, MutPtr, MutVoidPtr, SafeRead};
use crate::objc::{
    id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;

type CFRunLoopTimerRef = super::CFTypeRef;
type CFOptionFlags = u32;

// void (*void)(CFRunLoopTimerRef timer, void *info)
type CFRunLoopTimerCallBack = GuestFunction;

#[repr(C, packed)]
pub struct CFRunLoopTimerContext {
    version: CFIndex,
    info: MutVoidPtr,
    retain_callback: GuestFunction,
    release_callback: GuestFunction,
    copy_desc_callback: GuestFunction,
}
unsafe impl SafeRead for CFRunLoopTimerContext {}

/// `CFRunLoopTimerRef CFRunLoopTimerCreate(CFAllocatorRef allocator,
///     CFAbsoluteTime fireDate, CFTimeInterval interval, CFOptionFlags flags,
///     CFIndex order, CFRunLoopTimerCallBack callout,
///     CFRunLoopTimerContext *context)`
///
/// Creates an `NSTimer` (the toll-free bridged type) that calls `callout`
/// with `(timer, context->info)` when it fires. It first fires at `fireDate`
/// and then every `interval` seconds, or only once if `interval` is 0. As a
/// Create function it returns a +1 reference. See CFRunLoop.h and
/// https://developer.apple.com/documentation/corefoundation/1542501-cfrunlooptimercreate
fn CFRunLoopTimerCreate(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    fire_date: CFAbsoluteTime,
    interval: CFTimeInterval,
    _flags: CFOptionFlags,
    _order: CFIndex,
    callout: CFRunLoopTimerCallBack,
    context_ptr: MutPtr<CFRunLoopTimerContext>,
) -> CFRunLoopTimerRef {
    // Apple's documentation says flags and order are currently ignored.
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented

    let info: MutVoidPtr = if !context_ptr.is_null() {
        let context = env.mem.read(context_ptr);
        // CF retains the info pointer for the timer's lifetime. Timers are
        // rarely destroyed, so we never call the release callback.
        let retain_callback = context.retain_callback;
        if !retain_callback.to_ptr().is_null() {
            let _: ConstVoidPtr = retain_callback.call_from_host(env, (context.info.cast_const(),));
        }
        context.info
    } else {
        MutVoidPtr::null()
    };

    let target: id = msg_class![env; _touchHLE_CFTimerTarget alloc];
    let target: id = msg![env; target initWithCallout:callout info:info];

    let selector = env.objc.lookup_selector("timerFireMethod:").unwrap();

    let repeats = interval > 0.0;
    let timer: id = msg_class![env; NSTimer timerWithTimeInterval:interval
                                                       target:target
                                                     selector:selector
                                                     userInfo:nil
                                                      repeats:repeats];
    // The timer retains its target.
    release(env, target);

    let date: id = msg_class![env; NSDate dateWithTimeIntervalSinceReferenceDate:fire_date];
    let _: () = msg![env; timer setFireDate:date];

    retain(env, timer)
}

pub const FUNCTIONS: FunctionExports = &[export_c_func!(CFRunLoopTimerCreate(_, _, _, _, _, _, _))];

/// Belongs to _touchHLE_CFTimerTarget
#[derive(Default)]
struct CFTimerTargetHostObject {
    callout: GuestFunction,
    info: MutVoidPtr,
}
impl HostObject for CFTimerTargetHostObject {}

/// _touchHLE_CFTimerTarget serves as a convenience
/// object for performing a callout from a timer.
pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation _touchHLE_CFTimerTarget: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(CFTimerTargetHostObject {
        callout: GuestFunction::null_ptr(),
        info: MutVoidPtr::null()
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithCallout:(GuestFunction)callout info:(MutVoidPtr)info {
    let host_object: &mut CFTimerTargetHostObject = env.objc.borrow_mut(this);
    host_object.callout = callout;
    host_object.info = info;
    this
}

- (())timerFireMethod:(id)timer { // NSTimer *
    let &CFTimerTargetHostObject {
        callout,
        info
    } = env.objc.borrow(this);
    () = callout.call_from_host(env, (timer, info));
}

@end

};
