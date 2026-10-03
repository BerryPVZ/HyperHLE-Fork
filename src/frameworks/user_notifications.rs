/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Minimal UserNotifications.framework support.

use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::HostDylib;
use crate::frameworks::foundation::NSUInteger;
use crate::objc::{
    autorelease, id, msg_class, nil, objc_classes, ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;

#[derive(Default)]
pub struct State {
    current_center: id,
}

#[derive(Default)]
struct UNUserNotificationCenterHostObject {
    delegate: id,
}
impl HostObject for UNUserNotificationCenterHostObject {}

fn block_invoke(env: &mut Environment, block: id) -> Option<GuestFunction> {
    if block == nil {
        return None;
    }
    let invoke_ptr: u32 = env.mem.read(block.cast::<u32>() + 3u32);
    if invoke_ptr == 0 {
        return None;
    }
    Some(GuestFunction::from_addr_with_thumb_bit(invoke_ptr))
}

fn invoke_bool_error_block(env: &mut Environment, block: id, granted: bool, error: id) {
    if let Some(invoke) = block_invoke(env, block) {
        let _: () = invoke.call_from_host(env, (block, granted, error));
    }
}

fn invoke_object_block(env: &mut Environment, block: id, value: id) {
    if let Some(invoke) = block_invoke(env, block) {
        let _: () = invoke.call_from_host(env, (block, value));
    }
}

fn invoke_error_block(env: &mut Environment, block: id, error: id) {
    if let Some(invoke) = block_invoke(env, block) {
        let _: () = invoke.call_from_host(env, (block, error));
    }
}

fn invoke_empty_array_block(env: &mut Environment, block: id) {
    let Some(invoke) = block_invoke(env, block) else {
        return;
    };
    let empty: id = msg_class![env; NSArray new];
    let _: () = invoke.call_from_host(env, (block, empty));
    autorelease(env, empty);
}

fn invoke_empty_set_block(env: &mut Environment, block: id) {
    let Some(invoke) = block_invoke(env, block) else {
        return;
    };
    let empty: id = msg_class![env; NSSet new];
    let _: () = invoke.call_from_host(env, (block, empty));
    autorelease(env, empty);
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UNUserNotificationCenter: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(
        this,
        Box::<UNUserNotificationCenterHostObject>::default(),
        &mut env.mem,
    )
}

+ (id)currentNotificationCenter {
    let existing = env.framework_state.user_notifications.current_center;
    if existing != nil {
        return existing;
    }
    let center = env.objc.alloc_static_object(
        this,
        Box::<UNUserNotificationCenterHostObject>::default(),
        &mut env.mem,
    );
    env.framework_state.user_notifications.current_center = center;
    center
}

- (id)delegate {
    env.objc
        .borrow::<UNUserNotificationCenterHostObject>(this)
        .delegate
}

- (())setDelegate:(id)delegate {
    env.objc
        .borrow_mut::<UNUserNotificationCenterHostObject>(this)
        .delegate = delegate;
}

- (())requestAuthorizationWithOptions:(NSUInteger)_options completionHandler:(id)completion_handler {
    invoke_bool_error_block(env, completion_handler, false, nil);
}

- (())getNotificationSettingsWithCompletionHandler:(id)completion_handler {
    invoke_object_block(env, completion_handler, nil);
}

- (())addNotificationRequest:(id)_request withCompletionHandler:(id)completion_handler {
    invoke_error_block(env, completion_handler, nil);
}

- (())getPendingNotificationRequestsWithCompletionHandler:(id)completion_handler {
    invoke_empty_array_block(env, completion_handler);
}

- (())getDeliveredNotificationsWithCompletionHandler:(id)completion_handler {
    invoke_empty_array_block(env, completion_handler);
}

- (())removePendingNotificationRequestsWithIdentifiers:(id)_identifiers {}

- (())removeAllPendingNotificationRequests {}

- (())removeDeliveredNotificationsWithIdentifiers:(id)_identifiers {}

- (())removeAllDeliveredNotifications {}

- (())setNotificationCategories:(id)_categories {}

- (())getNotificationCategoriesWithCompletionHandler:(id)completion_handler {
    invoke_empty_set_block(env, completion_handler);
}

@end

};

pub const DYLIB: HostDylib = HostDylib {
    path: "/System/Library/Frameworks/UserNotifications.framework/UserNotifications",
    aliases: &[],
    class_exports: &[CLASSES],
    constant_exports: &[],
    function_exports: &[],
};
