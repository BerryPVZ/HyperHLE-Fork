/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSAttributedString` and `NSMutableAttributedString`.
//!
//! Chrome (and many other apps) use attributed strings for styled text.
//! We store the plain text plus an ordered attribute dictionary per range.
//! UIKit label drawing supports the first run's font, color and paragraph
//! layout. Mixed-style rendering and advanced typography remain unsupported.

use crate::abi::GuestArg;
use crate::dyld::{export_c_func, FunctionExports};
use crate::frameworks::core_foundation::CFRange;
use crate::frameworks::foundation::{ns_dictionary, ns_string, NSRange, NSInteger, NSUInteger};
use crate::mem::{ConstPtr, MutPtr, SafeRead};
use crate::frameworks::foundation::ns_string::NSUTF8StringEncoding;
use crate::objc::{id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, NSZonePtr};
use crate::Environment;
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::frameworks::core_graphics::cg_context::{CGContextSaveGState, CGContextRestoreGState};
use crate::frameworks::uikit::{ui_font, ui_graphics::UIGraphicsGetCurrentContext};

/// UIKit's uniform-style text path (used by Cocos2d to rasterize labels).
/// Mixed-style runs currently use the first run's style for the whole label.
fn label_style(env: &mut Environment, this: id) -> (id, id, id) {
    let range: MutPtr<NSRange> = MutPtr::null();
    let attrs: id = msg![env; this attributesAtIndex:0u32 effectiveRange:range];
    let key = ns_string::get_static_str(env, "NSFont");
    let font: id = msg![env; attrs objectForKey:key];
    let font: id = if font == nil {
        msg_class![env; UIFont systemFontOfSize:12.0f32]
    } else { font };
    let key = ns_string::get_static_str(env, "NSColor");
    let color: id = msg![env; attrs objectForKey:key];
    let color: id = if color == nil { msg_class![env; UIColor blackColor] } else { color };
    let key = ns_string::get_static_str(env, "NSParagraphStyle");
    let paragraph: id = msg![env; attrs objectForKey:key];
    (font, color, paragraph)
}

/// Host object: text plus `(range, attrs)` pairs, non-overlapping, sorted.
#[derive(Default)]
pub struct NSAttributedStringHostObject {
    text: id, // NSString*
    /// Sorted by range.location.
    runs: Vec<(NSRange, id /* NSDictionary* */)>,
}
impl crate::objc::HostObject for NSAttributedStringHostObject {}

/// Replace one attribute interval, preserving the attributes outside it.
/// Each stored run owns its dictionary; input dictionaries may be autoreleased.
fn replace_attributes(env: &mut Environment, this: id, range: NSRange, attrs: id) {
    if range.length == 0 { return; }
    let end = range.location.checked_add(range.length).unwrap();
    let old = std::mem::take(&mut env.objc.borrow_mut::<NSAttributedStringHostObject>(this).runs);
    let mut runs = Vec::new();
    for (r, a) in old {
        let r_end = r.location + r.length;
        if r_end <= range.location || r.location >= end {
            runs.push((r, a));
            continue;
        }
        if r.location < range.location {
            runs.push((NSRange { location: r.location, length: range.location - r.location }, retain(env, a)));
        }
        if r_end > end {
            runs.push((NSRange { location: end, length: r_end - end }, retain(env, a)));
        }
        release(env, a);
    }
    if attrs != nil {
        let snapshot: id = msg![env; attrs copy];
        runs.push((range, snapshot));
    }
    runs.sort_by_key(|(r, _)| r.location);
    env.objc.borrow_mut::<NSAttributedStringHostObject>(this).runs = runs;
}

/// Visit all existing run boundaries, including gaps, within a changed range.
fn attribute_ranges(env: &Environment, this: id, range: NSRange) -> Vec<NSRange> {
    let end = range.location.checked_add(range.length).unwrap();
    let mut bounds = vec![range.location, end];
    for (r, _) in &env.objc.borrow::<NSAttributedStringHostObject>(this).runs {
        for p in [r.location, r.location + r.length] {
            if p > range.location && p < end { bounds.push(p); }
        }
    }
    bounds.sort_unstable();
    bounds.dedup();
    bounds.windows(2).map(|w| NSRange { location: w[0], length: w[1] - w[0] }).collect()
}

fn copy_contents(env: &mut Environment, this: id, other: id) {
    let text: id = msg![env; other string];
    let text: id = msg![env; text copy];
    let runs = env.objc.borrow::<NSAttributedStringHostObject>(other).runs.clone();
    let runs = runs.into_iter().map(|(r, a)| {
        let a: id = msg![env; a copy];
        (r, a)
    }).collect();
    let old = std::mem::replace(env.objc.borrow_mut::<NSAttributedStringHostObject>(this),
        NSAttributedStringHostObject { text, runs });
    release(env, old.text);
    for (_, a) in old.runs { release(env, a); }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSAttributedString: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(this, Box::<NSAttributedStringHostObject>::default(), &mut env.mem)
}

+ (id)attributedStringWithString:(id)string { // NSString*
    let new: id = msg_class![env; NSAttributedString alloc];
    let new: id = msg![env; new initWithString:string];
    let _: () = msg![env; new autorelease];
    new
}

- (id)init {
    let text: id = msg_class![env; NSString new];
    msg![env; this initWithString:text]
}

- (id)initWithString:(id)string { // NSString*
    msg![env; this initWithString:string attributes:nil]
}

- (id)initWithString:(id)string attributes:(id)attributes { // (NSString*, NSDictionary*)
    if string == nil {
        let _: () = msg![env; this release];
        return nil;
    }
    let retained_string = retain(env, string);
    let retained_attrs = if attributes != nil {
        let len: NSUInteger = msg![env; string length];
        let attrs = retain(env, attributes);
        Some((NSRange { location: 0, length: len }, attrs))
    } else {
        None
    };
    let host = env.objc.borrow_mut::<NSAttributedStringHostObject>(this);
    host.text = retained_string;
    if let Some((range, attrs)) = retained_attrs {
        host.runs.push((range, attrs));
    }
    this
}

- (id)initWithAttributedString:(id)other {
    copy_contents(env, this, other);
    this
}

- (id)initWithData:(id)data options:(id)_options documentAttributes:(MutPtr<id>)_doc_attrs error:(MutPtr<id>)_error {
    // HTML/RTF import: fall back to interpreting the data as UTF-8 text.
    let string: id = msg_class![env; NSString alloc];
    let string: id = msg![env; string initWithData:data encoding:NSUTF8StringEncoding];
    if string == nil {
        let _: () = msg![env; this release];
        return nil;
    }
    let _: () = msg![env; string autorelease];
    msg![env; this initWithString:string]
}

- (NSUInteger)length {
    let text = env.objc.borrow::<NSAttributedStringHostObject>(this).text;
    msg![env; text length]
}

- (id)string {
    env.objc.borrow::<NSAttributedStringHostObject>(this).text
}

- (CGSize)size {
    let (font, _, _) = label_style(env, this);
    let text: id = msg![env; this string];
    msg![env; text sizeWithFont:font]
}

- (CGRect)boundingRectWithSize:(CGSize)size options:(NSUInteger)options context:(id)_context {
    let (font, _, paragraph) = label_style(env, this);
    let text: id = msg![env; this string];
    let text = ns_string::to_rust_string(env, text);
    let mode: NSInteger = msg![env; paragraph lineBreakMode];
    // NSStringDrawingUsesLineFragmentOrigin enables multiline layout.
    let constraint = if options & 1 != 0 && size.width > 0.0 {
        Some((size, mode))
    } else { None };
    CGRect { origin: CGPoint { x: 0.0, y: 0.0 },
        size: ui_font::size_with_font(env, font, &text, constraint) }
}

- (())drawInRect:(CGRect)rect {
    let (font, color, paragraph) = label_style(env, this);
    let text: id = msg![env; this string];
    let mode: NSInteger = msg![env; paragraph lineBreakMode];
    let alignment: NSInteger = msg![env; paragraph alignment];
    let context = UIGraphicsGetCurrentContext(env);
    if context.is_null() { return; }
    CGContextSaveGState(env, context);
    let _: () = msg![env; color setFill];
    let _: CGSize = msg![env; text drawInRect:rect withFont:font lineBreakMode:mode alignment:alignment];
    CGContextRestoreGState(env, context);
}

- (())drawAtPoint:(CGPoint)point {
    let size: CGSize = msg![env; this size];
    let rect = CGRect { origin: point, size };
    let _: () = msg![env; this drawInRect:rect];
}

- (())drawWithRect:(CGRect)rect options:(NSUInteger)_options context:(id)_context {
    let _: () = msg![env; this drawInRect:rect];
}

// ---- attribute access ----

- (id)attributesAtIndex:(NSUInteger)location effectiveRange:(MutPtr<NSRange>)range_ptr {
    let text: id = msg![env; this string];
    let length: NSUInteger = msg![env; text length];
    let host = env.objc.borrow::<NSAttributedStringHostObject>(this);
    for (range, attrs) in host.runs.iter() {
        if location >= range.location && location < range.location + range.length {
            if !range_ptr.is_null() {
                env.mem.write(range_ptr, *range);
            }
            return *attrs;
        }
    }
    if !range_ptr.is_null() {
        let start = host.runs.iter().map(|(r, _)| r.location + r.length)
            .filter(|&end| end <= location).max().unwrap_or(0);
        let end = host.runs.iter().map(|(r, _)| r.location)
            .filter(|&start| start > location).min().unwrap_or(length);
        env.mem.write(range_ptr, NSRange { location: start, length: end - start });
    }
    msg_class![env; NSDictionary dictionary]
}

- (id)attribute:(id)name atIndex:(NSUInteger)location effectiveRange:(MutPtr<NSRange>)range_ptr {
    let attrs: id = msg![env; this attributesAtIndex:location effectiveRange:range_ptr];
    if attrs == nil { return nil; }
    msg![env; attrs objectForKey:name]
}

- (id)attributedSubstringFromRange:(NSRange)range {
    let text: id = msg![env; this string];
    let sub: id = msg![env; text substringWithRange:range];
    let runs = env.objc.borrow::<NSAttributedStringHostObject>(this).runs.clone();
    let new: id = msg_class![env; NSAttributedString alloc];
    let new: id = msg![env; new initWithString:sub];
    let end = range.location.checked_add(range.length).unwrap();
    for (r, attrs) in runs {
        let start = r.location.max(range.location);
        let stop = (r.location + r.length).min(end);
        if start < stop {
            replace_attributes(env, new, NSRange {
                location: start - range.location, length: stop - start,
            }, attrs);
        }
    }
    msg![env; new autorelease]
}

- (bool)isEqualToAttributedString:(id)other {
    if other == nil { return false; }
    let a: id = msg![env; this string];
    let b: id = msg![env; other string];
    let eq: bool = msg![env; a isEqualToString:b];
    eq
}

- (id)copyWithZone:(NSZonePtr)_zone {
    let new: id = msg_class![env; NSAttributedString alloc];
    msg![env; new initWithAttributedString:this]
}
- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    let new: id = msg_class![env; NSMutableAttributedString alloc];
    msg![env; new initWithAttributedString:this]
}

- (())dealloc {
    let old_text;
    let old_runs;
    {
        let host = env.objc.borrow_mut::<NSAttributedStringHostObject>(this);
        old_text = std::mem::replace(&mut host.text, nil);
        old_runs = std::mem::take(&mut host.runs);
    }
    release(env, old_text);
    for (_, attrs) in old_runs {
        release(env, attrs);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

@implementation NSMutableAttributedString: NSAttributedString

// ---- mutable editing ----

- (())setAttributedString:(id)other {
    copy_contents(env, this, other);
}

- (())addAttribute:(id)name value:(id)value range:(NSRange)range {
    for part in attribute_ranges(env, this, range) {
        let range_ptr: MutPtr<NSRange> = MutPtr::null();
        let existing: id = msg![env; this attributesAtIndex:(part.location) effectiveRange:range_ptr];
        let dict: id = if existing != nil {
            msg![env; existing mutableCopy]
        } else {
            msg_class![env; NSMutableDictionary new]
        };
        let _: () = msg![env; dict setObject:value forKey:name];
        replace_attributes(env, this, part, dict);
        release(env, dict);
    }
}

- (())removeAttribute:(id)name range:(NSRange)range {
    for part in attribute_ranges(env, this, range) {
        let range_ptr: MutPtr<NSRange> = MutPtr::null();
        let existing: id = msg![env; this attributesAtIndex:(part.location) effectiveRange:range_ptr];
        if existing == nil { continue; }
        let dict: id = msg![env; existing mutableCopy];
        let _: () = msg![env; dict removeObjectForKey:name];
        replace_attributes(env, this, part, dict);
        release(env, dict);
    }
}

- (())replaceCharactersInRange:(NSRange)range withString:(id)string {
    let new_len: NSUInteger = msg![env; string length];
    let delta: NSInteger = new_len as NSInteger - range.length as NSInteger;
    let old_text: id = msg![env; this string];
    let new_text: id = msg![env; old_text stringByReplacingCharactersInRange:range withString:string];
    let stored_text = retain(env, new_text);
    let _ = new_text;
    {
        let host = env.objc.borrow_mut::<NSAttributedStringHostObject>(this);
        host.text = stored_text;
        for (r, _attrs) in host.runs.iter_mut() {
            if r.location >= range.location + range.length {
                r.location = (r.location as NSInteger + delta) as NSUInteger;
            } else if r.location + r.length > range.location {
                r.length = range.location.saturating_sub(r.location);
            }
        }
        host.runs.retain(|(r, _)| r.length > 0 || new_len == 0);
    }
    release(env, old_text);
}

- (())setAttributes:(id)attributes range:(NSRange)range {
    replace_attributes(env, this, range, attributes);
}

@end

};
