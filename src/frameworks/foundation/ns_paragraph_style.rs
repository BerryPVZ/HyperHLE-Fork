/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Basic paragraph layout attributes for UIKit's attributed labels.

use super::NSInteger;
use crate::objc::{id, msg, msg_class, objc_classes, ClassExports, NSZonePtr};

#[derive(Clone)]
struct ParagraphStyle {
    alignment: NSInteger,
    line_break_mode: NSInteger,
}
impl Default for ParagraphStyle {
    fn default() -> Self {
        Self { alignment: 4, line_break_mode: 0 } // natural, word wrapping
    }
}
impl crate::objc::HostObject for ParagraphStyle {}

pub const CLASSES: ClassExports = objc_classes! {
(env, this, _cmd);

@implementation NSParagraphStyle: NSObject
+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(this, Box::<ParagraphStyle>::default(), &mut env.mem)
}
+ (id)defaultParagraphStyle {
    let style: id = msg_class![env; NSParagraphStyle new];
    msg![env; style autorelease]
}
- (NSInteger)alignment { env.objc.borrow::<ParagraphStyle>(this).alignment }
- (NSInteger)lineBreakMode { env.objc.borrow::<ParagraphStyle>(this).line_break_mode }
- (id)copyWithZone:(NSZonePtr)_zone {
    let style = env.objc.borrow::<ParagraphStyle>(this).clone();
    let copy: id = msg_class![env; NSParagraphStyle new];
    *env.objc.borrow_mut::<ParagraphStyle>(copy) = style;
    copy
}
- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    let style = env.objc.borrow::<ParagraphStyle>(this).clone();
    let copy: id = msg_class![env; NSMutableParagraphStyle new];
    *env.objc.borrow_mut::<ParagraphStyle>(copy) = style;
    copy
}
@end

@implementation NSMutableParagraphStyle: NSParagraphStyle
- (())setAlignment:(NSInteger)value {
    env.objc.borrow_mut::<ParagraphStyle>(this).alignment = value;
}
- (())setLineBreakMode:(NSInteger)value {
    env.objc.borrow_mut::<ParagraphStyle>(this).line_break_mode = value;
}
@end
};
