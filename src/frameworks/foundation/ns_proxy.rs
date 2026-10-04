/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! NSProxy root-class allocation and NSObject-protocol lifetime operations.
//! Concrete proxies implement their own method signatures and forwarding.

use super::ns_object::{invoke_cxx_constructors, invoke_cxx_destructors};
use super::NSUInteger;
use crate::mem::MutVoidPtr;
use crate::objc::{
    id, msg, msg_class, objc_classes, Class, ClassExports, NSZonePtr, ObjC, TrivialHostObject, SEL,
};

pub const CLASSES: ClassExports = objc_classes! {
(env, this, _cmd);

// NSProxy is a separate root, not an NSObject subclass.
@implementation NSProxy

+ (id)alloc { msg![env; this allocWithZone:(MutVoidPtr::null())] }
+ (id)allocWithZone:(NSZonePtr)_zone {
    let object = env.objc.alloc_object(this, Box::new(TrivialHostObject), &mut env.mem);
    invoke_cxx_constructors(env, object);
    object
}
+ (Class)class { this }
+ (Class)superclass { env.objc.get_superclass(this) }
+ (())initialize {}
+ (bool)respondsToSelector:(SEL)selector {
    env.objc.object_has_method(&env.mem, this, selector)
}
- (Class)class { ObjC::read_isa(this, &env.mem) }
- (Class)superclass {
    let class = ObjC::read_isa(this, &env.mem);
    env.objc.get_superclass(class)
}
- (bool)isProxy { true }
- (id)retain {
    env.objc.increment_refcount(this);
    this
}
- (NSUInteger)retainCount { env.objc.get_refcount(this).into() }
- (())release {
    if env.objc.decrement_refcount(this) { () = msg![env; this dealloc]; }
}
- (id)autorelease {
    () = msg_class![env; NSAutoreleasePool addObject:this];
    this
}
- (())dealloc {
    invoke_cxx_destructors(env, this);
    env.objc.dealloc_object(this, &mut env.mem)
}
- (bool)respondsToSelector:(SEL)selector {
    env.objc.object_has_method(&env.mem, this, selector)
}
- (NSUInteger)hash { this.to_bits() }
- (bool)isEqual:(id)object { this == object }
@end
};
