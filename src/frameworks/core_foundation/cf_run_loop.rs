/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFRunLoop`.
//!
//! This is not even toll-free bridged to `NSRunLoop` in Apple's implementation,
//! but here it is the same type.

use crate::abi::CallFromHost;
use crate::abi::GuestFunction;
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use crate::frameworks::core_foundation::time::{CFAbsoluteTime, CFTimeInterval};
use crate::frameworks::core_foundation::{CFIndex, CFRelease, CFRetain, CFTypeRef};
use crate::frameworks::foundation::ns_run_loop;
use crate::frameworks::foundation::ns_string;
use crate::mem::{MutPtr, MutVoidPtr, SafeRead};
use crate::objc::{id, msg, msg_class, nil, objc_classes, ClassExports, HostObject};
use crate::Environment;

pub type CFRunLoopRef = CFTypeRef;
pub type CFRunLoopMode = super::cf_string::CFStringRef;
pub type CFRunLoopSourceRef = CFTypeRef;
pub type CFRunLoopObserverRef = CFTypeRef;
pub type CFRunLoopTimerRef = CFTypeRef;

/// `CFRunLoopSourceContext` (version 0). Mirrors the public C declaration in
/// `<CoreFoundation/CFRunLoop.h>`.
#[repr(C, packed)]
pub struct CFRunLoopSourceContext {
    pub version: CFIndex,
    pub info: MutVoidPtr,
    pub retain: GuestFunction,
    pub release: GuestFunction,
    pub copyDescription: GuestFunction,
    pub equal: GuestFunction,
    pub hash: GuestFunction,
    pub schedule: GuestFunction,
    pub cancel: GuestFunction,
    pub perform: GuestFunction,
}
unsafe impl SafeRead for CFRunLoopSourceContext {}

#[derive(Default)]
pub struct CFRunLoopSourceHostObject {
    pub version: CFIndex,
    pub info: MutVoidPtr,
    pub retain: GuestFunction,
    pub release: GuestFunction,
    pub copy_description: GuestFunction,
    pub equal: GuestFunction,
    pub hash: GuestFunction,
    pub schedule: GuestFunction,
    pub cancel: GuestFunction,
    pub perform: GuestFunction,
    pub order: i32,
    pub valid: bool,
    pub signaled: bool,
}
impl HostObject for CFRunLoopSourceHostObject {}

// CFRunLoopActivity flags
type CFRunLoopActivity = u32;
const kCFRunLoopEntry: CFRunLoopActivity = 1 << 0;
const kCFRunLoopBeforeTimers: CFRunLoopActivity = 1 << 1;
const kCFRunLoopBeforeSources: CFRunLoopActivity = 1 << 2;
const kCFRunLoopBeforeWaiting: CFRunLoopActivity = 1 << 5;
const kCFRunLoopAfterWaiting: CFRunLoopActivity = 1 << 6;
const kCFRunLoopExit: CFRunLoopActivity = 1 << 7;
const kCFRunLoopAllActivities: CFRunLoopActivity = 0x0FFFFFFF;

pub const kCFRunLoopCommonModes: &str = "kCFRunLoopCommonModes";
pub const kCFRunLoopDefaultMode: &str = "kCFRunLoopDefaultMode";

pub const CONSTANTS: ConstantExports = &[
    (
        "_kCFRunLoopCommonModes",
        HostConstant::NSString(kCFRunLoopCommonModes),
    ),
    (
        "_kCFRunLoopDefaultMode",
        HostConstant::NSString(kCFRunLoopDefaultMode),
    ),
];

// MARK: - Retain / Release

pub fn CFRunLoopRetain(env: &mut Environment, rl: CFRunLoopRef) -> CFRunLoopRef {
    if !rl.is_null() {
        CFRetain(env, rl)
    } else {
        rl
    }
}

pub fn CFRunLoopRelease(env: &mut Environment, rl: CFRunLoopRef) {
    if !rl.is_null() {
        CFRelease(env, rl);
    }
}

// MARK: - Accessors

fn CFRunLoopGetCurrent(env: &mut Environment) -> CFRunLoopRef {
    msg_class![env; NSRunLoop currentRunLoop]
}

pub fn CFRunLoopGetMain(env: &mut Environment) -> CFRunLoopRef {
    msg_class![env; NSRunLoop mainRunLoop]
}

fn CFRunLoopCopyCurrentMode(env: &mut Environment, rl: CFRunLoopRef) -> CFRunLoopMode {
    match ns_run_loop::current_mode(env, rl) {
        Some(mode) => ns_string::from_rust_string(env, mode),
        None => nil,
    }
}

fn CFRunLoopCopyAllModes(env: &mut Environment, _rl: CFRunLoopRef) -> CFTypeRef {
    // Return an array containing only the default mode.
    let mode = ns_string::get_static_str(env, kCFRunLoopDefaultMode);
    let arr: id = msg_class![env; NSArray arrayWithObject:mode];
    arr
}

fn CFRunLoopContainsSource(
    env: &mut Environment,
    rl: CFRunLoopRef,
    source: CFRunLoopSourceRef,
    mode: CFRunLoopMode,
) -> bool {
    let mode = ns_string::to_rust_string(env, mode).to_string();
    ns_run_loop::contains_in_mode(env, rl, source, &mode)
}

fn CFRunLoopAddCommonMode(env: &mut Environment, rl: CFRunLoopRef, mode: CFRunLoopMode) {
    ns_run_loop::add_common_mode(env, rl, mode);
}

fn CFRunLoopContainsObserver(
    _env: &mut Environment,
    _rl: CFRunLoopRef,
    _observer: CFRunLoopObserverRef,
    _mode: CFRunLoopMode,
) -> bool {
    false
}

fn CFRunLoopContainsTimer(
    _env: &mut Environment,
    _rl: CFRunLoopRef,
    _timer: CFRunLoopTimerRef,
    _mode: CFRunLoopMode,
) -> bool {
    false
}

fn CFRunLoopIsWaiting(_env: &mut Environment, _rl: CFRunLoopRef) -> bool {
    false
}

// MARK: - Run

fn CFRunLoopRun(env: &mut Environment) {
    let rl = CFRunLoopGetCurrent(env);
    let _: () = msg![env; rl run];
}

fn CFRunLoopRunInMode(
    env: &mut Environment,
    mode: CFRunLoopMode,
    seconds: CFTimeInterval,
    return_after_source_handled: bool,
) -> i32 {
    let current = CFRunLoopGetCurrent(env);
    let now = env
        .guest_clock
        .system_time()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    ns_run_loop::run_in_mode(
        env,
        current,
        mode,
        Some(now + seconds.max(0.0)),
        return_after_source_handled,
    ) as i32
}

fn CFRunLoopStop(env: &mut Environment, rl: CFRunLoopRef) {
    if rl.is_null() {
        return;
    }
    ns_run_loop::stop(env, rl);
}

fn CFRunLoopWakeUp(_env: &mut Environment, _rl: CFRunLoopRef) {
    log_dbg!("CFRunLoopWakeUp: stubbed");
}

// MARK: - Sources

fn CFRunLoopAddSource(
    env: &mut Environment,
    rl: CFRunLoopRef,
    source: CFRunLoopSourceRef,
    mode: CFRunLoopMode,
) {
    if rl.is_null() || source.is_null() {
        return;
    }
    // Custom modes are valid registration names; nil is not a mode.
    if mode.is_null() {
        return;
    }
    if !ns_run_loop::register_mode(env, rl, source, mode) {
        return;
    }
    let already_contains = env
        .objc
        .borrow::<crate::frameworks::foundation::ns_run_loop::NSRunLoopHostObject>(rl)
        .sources
        .contains(&source);
    if !already_contains {
        CFRetain(env, source);
        env.objc
            .borrow_mut::<ns_run_loop::NSRunLoopHostObject>(rl)
            .sources
            .push(source);
    }
    let schedule = env
        .objc
        .borrow::<CFRunLoopSourceHostObject>(source)
        .schedule;
    let info = env.objc.borrow::<CFRunLoopSourceHostObject>(source).info;
    if !schedule.to_ptr().is_null() {
        let _: () = schedule.call_from_host(env, (info, rl, mode));
    }
}

fn CFRunLoopRemoveSource(
    env: &mut Environment,
    rl: CFRunLoopRef,
    source: CFRunLoopSourceRef,
    mode: CFRunLoopMode,
) {
    if rl.is_null() || source.is_null() {
        return;
    }
    let mode_name = ns_string::to_rust_string(env, mode).to_string();
    if !ns_run_loop::registered_in_mode(env, rl, source, &mode_name) {
        return;
    }
    let last = ns_run_loop::unregister_mode(env, rl, source, mode);
    let (cancel, info) = {
        let state = env.objc.borrow::<CFRunLoopSourceHostObject>(source);
        (state.cancel, state.info)
    };
    if last {
        env.objc
            .borrow_mut::<ns_run_loop::NSRunLoopHostObject>(rl)
            .sources
            .retain(|s| *s != source);
    }
    if !cancel.to_ptr().is_null() {
        let _: () = cancel.call_from_host(env, (info, rl, mode));
    }
    if last {
        CFRelease(env, source);
    }
}

fn CFRunLoopSourceCreate(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    order: i32,
    context_ptr: MutPtr<CFRunLoopSourceContext>,
) -> CFRunLoopSourceRef {
    if !(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()) {
        log!("CFRunLoopSourceCreate: non-default allocator unsupported, returning null");
        return nil;
    }
    if context_ptr.is_null() {
        log!("CFRunLoopSourceCreate: NULL context, returning null");
        return nil;
    }
    let context = env.mem.read(context_ptr);
    let version = context.version;
    if version != 0 {
        log!(
            "CFRunLoopSourceCreate: unsupported context version {}, returning null",
            version
        );
        return nil;
    }
    let host = Box::new(CFRunLoopSourceHostObject {
        version,
        info: context.info,
        retain: context.retain,
        release: context.release,
        copy_description: context.copyDescription,
        equal: context.equal,
        hash: context.hash,
        schedule: context.schedule,
        cancel: context.cancel,
        perform: context.perform,
        order,
        valid: true,
        signaled: false,
    });
    let class = env
        .objc
        .get_known_class("_touchHLE_CFRunLoopSource", &mut env.mem);
    let source = env.objc.alloc_object(class, host, &mut env.mem);
    // Retain `info` if the context provides a retain callback (Apple docs).
    let retain_cb = env.objc.borrow::<CFRunLoopSourceHostObject>(source).retain;
    let info = env.objc.borrow::<CFRunLoopSourceHostObject>(source).info;
    if !retain_cb.to_ptr().is_null() {
        let _: MutVoidPtr = retain_cb.call_from_host(env, (info,));
    }
    source
}

fn CFRunLoopSourceRetain(env: &mut Environment, source: CFRunLoopSourceRef) -> CFRunLoopSourceRef {
    if !source.is_null() {
        CFRetain(env, source)
    } else {
        source
    }
}

fn CFRunLoopSourceRelease(env: &mut Environment, source: CFRunLoopSourceRef) {
    if !source.is_null() {
        CFRelease(env, source);
    }
}

fn CFRunLoopSourceInvalidate(env: &mut Environment, source: CFRunLoopSourceRef) {
    if source.is_null() {
        return;
    }
    env.objc
        .borrow_mut::<CFRunLoopSourceHostObject>(source)
        .valid = false;
}

fn CFRunLoopSourceIsValid(env: &mut Environment, source: CFRunLoopSourceRef) -> bool {
    if source.is_null() {
        return false;
    }
    env.objc.borrow::<CFRunLoopSourceHostObject>(source).valid
}

fn CFRunLoopSourceSignal(env: &mut Environment, source: CFRunLoopSourceRef) {
    if source.is_null() {
        return;
    }
    env.objc
        .borrow_mut::<CFRunLoopSourceHostObject>(source)
        .signaled = true;
}

fn CFRunLoopSourceGetOrder(env: &mut Environment, source: CFRunLoopSourceRef) -> i32 {
    if source.is_null() {
        return 0;
    }
    env.objc.borrow::<CFRunLoopSourceHostObject>(source).order
}

fn CFRunLoopSourceGetContext(
    env: &mut Environment,
    source: CFRunLoopSourceRef,
    ctx_out: MutPtr<CFRunLoopSourceContext>,
) {
    if source.is_null() || ctx_out.is_null() {
        return;
    }
    let host = env.objc.borrow::<CFRunLoopSourceHostObject>(source);
    let ctx = CFRunLoopSourceContext {
        version: host.version,
        info: host.info,
        retain: host.retain,
        release: host.release,
        copyDescription: host.copy_description,
        equal: host.equal,
        hash: host.hash,
        schedule: host.schedule,
        cancel: host.cancel,
        perform: host.perform,
    };
    env.mem.write(ctx_out, ctx);
}

// MARK: - Observers

fn CFRunLoopAddObserver(
    _env: &mut Environment,
    _rl: CFRunLoopRef,
    _observer: CFRunLoopObserverRef,
    _mode: CFRunLoopMode,
) {
    log!("CFRunLoopAddObserver: stubbed");
}

fn CFRunLoopRemoveObserver(
    _env: &mut Environment,
    _rl: CFRunLoopRef,
    _observer: CFRunLoopObserverRef,
    _mode: CFRunLoopMode,
) {
    log!("CFRunLoopRemoveObserver: stubbed");
}

fn CFRunLoopObserverCreate(
    _env: &mut Environment,
    _allocator: CFTypeRef,
    _activities: CFRunLoopActivity,
    _repeats: bool,
    _order: i32,
    _callout: MutVoidPtr, // CFRunLoopObserverCallBack
    _context: MutVoidPtr, // CFRunLoopObserverContext*
) -> CFRunLoopObserverRef {
    log!("CFRunLoopObserverCreate: stubbed, returning null");
    nil
}

fn CFRunLoopObserverRetain(
    env: &mut Environment,
    observer: CFRunLoopObserverRef,
) -> CFRunLoopObserverRef {
    if !observer.is_null() {
        CFRetain(env, observer)
    } else {
        observer
    }
}

fn CFRunLoopObserverRelease(env: &mut Environment, observer: CFRunLoopObserverRef) {
    if !observer.is_null() {
        CFRelease(env, observer);
    }
}

fn CFRunLoopObserverIsValid(_env: &mut Environment, observer: CFRunLoopObserverRef) -> bool {
    !observer.is_null()
}

fn CFRunLoopObserverInvalidate(_env: &mut Environment, _observer: CFRunLoopObserverRef) {
    log_dbg!("CFRunLoopObserverInvalidate: stubbed");
}

fn CFRunLoopObserverGetActivities(
    _env: &mut Environment,
    _observer: CFRunLoopObserverRef,
) -> CFRunLoopActivity {
    kCFRunLoopAllActivities
}

fn CFRunLoopObserverDoesRepeat(_env: &mut Environment, _observer: CFRunLoopObserverRef) -> bool {
    false
}

fn CFRunLoopObserverGetOrder(_env: &mut Environment, _observer: CFRunLoopObserverRef) -> i32 {
    0
}

// MARK: - Timers

fn CFRunLoopAddTimer(
    env: &mut Environment,
    rl: CFRunLoopRef,
    timer: CFRunLoopTimerRef,
    mode: CFRunLoopMode,
) {
    if rl.is_null() || timer.is_null() {
        return;
    }

    // Как сказано в заголовке файла: в touchHLE CFRunLoop и NSRunLoop — это
    // один и тот же тип.
    // Поэтому мы честно пробрасываем вызов напрямую в NSRunLoop, который умеет
    // работать с таймерами.
    let _: () = msg![env; rl addTimer:timer forMode:mode];
}

fn CFRunLoopRemoveTimer(
    env: &mut Environment,
    rl: CFRunLoopRef,
    timer: CFRunLoopTimerRef,
    _mode: CFRunLoopMode,
) {
    // TODO: modes. Removing a timer from a run loop doesn't invalidate it.
    if rl.is_null() || timer.is_null() {
        return;
    }
    ns_run_loop::remove_timer(env, rl, timer);
}

fn CFRunLoopTimerRetain(env: &mut Environment, timer: CFRunLoopTimerRef) -> CFRunLoopTimerRef {
    if !timer.is_null() {
        CFRetain(env, timer)
    } else {
        timer
    }
}

fn CFRunLoopTimerRelease(env: &mut Environment, timer: CFRunLoopTimerRef) {
    if !timer.is_null() {
        CFRelease(env, timer);
    }
}

// CFRunLoopTimer is toll-free bridged to NSTimer, and CFRunLoopTimerCreate
// returns an NSTimer, so the accessors below just message the timer.

fn CFRunLoopTimerIsValid(env: &mut Environment, timer: CFRunLoopTimerRef) -> bool {
    msg![env; timer isValid]
}

fn CFRunLoopTimerInvalidate(env: &mut Environment, timer: CFRunLoopTimerRef) {
    let _: () = msg![env; timer invalidate];
}

/// `CFAbsoluteTime CFRunLoopTimerGetNextFireDate(CFRunLoopTimerRef timer)`
fn CFRunLoopTimerGetNextFireDate(
    env: &mut Environment,
    timer: CFRunLoopTimerRef,
) -> CFAbsoluteTime {
    let date: id = msg![env; timer fireDate];
    msg![env; date timeIntervalSinceReferenceDate]
}

/// `void CFRunLoopTimerSetNextFireDate(CFRunLoopTimerRef timer,
///     CFAbsoluteTime fireDate)`
///
/// Equivalent to `-[NSTimer setFireDate:]` for a date given as seconds since
/// the reference date (1 January 2001). See CFRunLoop.h and
/// https://developer.apple.com/documentation/corefoundation/1543570-cfrunlooptimersetnextfiredate
/// PAC-MAN Remix schedules every frame this way on an NSTimer whose interval
/// is effectively infinite.
fn CFRunLoopTimerSetNextFireDate(
    env: &mut Environment,
    timer: CFRunLoopTimerRef,
    fire_date: CFAbsoluteTime,
) {
    let date: id = msg_class![env; NSDate dateWithTimeIntervalSinceReferenceDate:fire_date];
    let _: () = msg![env; timer setFireDate:date];
}

fn CFRunLoopTimerGetInterval(env: &mut Environment, timer: CFRunLoopTimerRef) -> CFTimeInterval {
    msg![env; timer timeInterval]
}

fn CFRunLoopTimerDoesRepeat(env: &mut Environment, timer: CFRunLoopTimerRef) -> bool {
    // CF reports a timer as repeating when its interval is positive, and
    // -[NSTimer timeInterval] is 0 for non-repeating timers.
    CFRunLoopTimerGetInterval(env, timer) > 0.0
}

fn CFRunLoopTimerGetOrder(_env: &mut Environment, _timer: CFRunLoopTimerRef) -> i32 {
    0
}

pub const FUNCTIONS: FunctionExports = &[
    // Run loop lifecycle
    export_c_func!(CFRunLoopRetain(_)),
    export_c_func!(CFRunLoopRelease(_)),
    export_c_func!(CFRunLoopGetCurrent()),
    export_c_func!(CFRunLoopGetMain()),
    export_c_func!(CFRunLoopCopyCurrentMode(_)),
    export_c_func!(CFRunLoopCopyAllModes(_)),
    export_c_func!(CFRunLoopIsWaiting(_)),
    export_c_func!(CFRunLoopContainsSource(_, _, _)),
    export_c_func!(CFRunLoopContainsObserver(_, _, _)),
    export_c_func!(CFRunLoopContainsTimer(_, _, _)),
    // Running
    export_c_func!(CFRunLoopRun()),
    export_c_func!(CFRunLoopAddCommonMode(_, _)),
    export_c_func!(CFRunLoopRunInMode(_, _, _)),
    export_c_func!(CFRunLoopStop(_)),
    export_c_func!(CFRunLoopWakeUp(_)),
    // Sources
    export_c_func!(CFRunLoopAddSource(_, _, _)),
    export_c_func!(CFRunLoopRemoveSource(_, _, _)),
    export_c_func!(CFRunLoopSourceCreate(_, _, _)),
    export_c_func!(CFRunLoopSourceRetain(_)),
    export_c_func!(CFRunLoopSourceRelease(_)),
    export_c_func!(CFRunLoopSourceSignal(_)),
    export_c_func!(CFRunLoopSourceIsValid(_)),
    export_c_func!(CFRunLoopSourceInvalidate(_)),
    export_c_func!(CFRunLoopSourceGetOrder(_)),
    export_c_func!(CFRunLoopSourceGetContext(_, _)),
    // Observers
    export_c_func!(CFRunLoopAddObserver(_, _, _)),
    export_c_func!(CFRunLoopRemoveObserver(_, _, _)),
    export_c_func!(CFRunLoopObserverCreate(_, _, _, _, _, _)),
    export_c_func!(CFRunLoopObserverRetain(_)),
    export_c_func!(CFRunLoopObserverRelease(_)),
    export_c_func!(CFRunLoopObserverIsValid(_)),
    export_c_func!(CFRunLoopObserverInvalidate(_)),
    export_c_func!(CFRunLoopObserverGetActivities(_)),
    export_c_func!(CFRunLoopObserverDoesRepeat(_)),
    export_c_func!(CFRunLoopObserverGetOrder(_)),
    // Timers
    export_c_func!(CFRunLoopAddTimer(_, _, _)),
    export_c_func!(CFRunLoopRemoveTimer(_, _, _)),
    // CFRunLoopTimerCreate is exported from cf_run_loop_timer.
    export_c_func!(CFRunLoopTimerRetain(_)),
    export_c_func!(CFRunLoopTimerRelease(_)),
    export_c_func!(CFRunLoopTimerIsValid(_)),
    export_c_func!(CFRunLoopTimerInvalidate(_)),
    export_c_func!(CFRunLoopTimerGetNextFireDate(_)),
    export_c_func!(CFRunLoopTimerSetNextFireDate(_, _)),
    export_c_func!(CFRunLoopTimerGetInterval(_)),
    export_c_func!(CFRunLoopTimerDoesRepeat(_)),
    export_c_func!(CFRunLoopTimerGetOrder(_)),
];

// At the bottom of the file, append (still inside the same Rust module) a
// CLASSES const that declares `_touchHLE_CFRunLoopSource: NSObject` so the
// runtime can dispatch retain/release through env.objc.
pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation _touchHLE_CFRunLoopSource: NSObject

- (())dealloc {
    // If a release callback was supplied, call it on `info` per Apple docs.
    let release_cb = env
        .objc
        .borrow::<CFRunLoopSourceHostObject>(this)
        .release;
    let info = env.objc.borrow::<CFRunLoopSourceHostObject>(this).info;
    if !release_cb.to_ptr().is_null() {
        let _: () = release_cb.call_from_host(env, (info,));
    }
    env.objc.dealloc_object(this, &mut env.mem);
}

@end

};
