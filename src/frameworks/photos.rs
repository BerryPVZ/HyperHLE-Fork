/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Photos.framework dependency registration.
//!
//! Photo-library access is not available in the emulator, but apps can still
//! link against the documented maximum-size sentinel used by PhotoKit APIs.

use crate::dyld::{ConstantExports, HostConstant, HostDylib};
use crate::frameworks::core_graphics::CGSize;
use crate::mem::ConstVoidPtr;
use crate::Environment;

fn ph_image_manager_maximum_size(env: &mut Environment) -> ConstVoidPtr {
    env.mem
        .alloc_and_write(CGSize {
            width: f32::MAX,
            height: f32::MAX,
        })
        .cast()
        .cast_const()
}

pub const CONSTANTS: ConstantExports = &[(
    "_PHImageManagerMaximumSize",
    HostConstant::Custom(ph_image_manager_maximum_size),
)];

pub const DYLIB: HostDylib = HostDylib {
    path: "/System/Library/Frameworks/Photos.framework/Photos",
    aliases: &[],
    class_exports: &[],
    constant_exports: &[CONSTANTS],
    function_exports: &[],
};
