/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Compatibility for BTC Free input and classic content inside BTC Sports.
use super::{id, msg, msg_class, Class, SEL};
use crate::frameworks::core_graphics::{CGPoint, CGSize};
use crate::Environment;

fn slot(env: &Environment, object: id, name: &str) -> Option<crate::mem::MutPtr<u32>> {
    env.objc
        .object_lookup_ivar(&env.mem, object, &name.to_owned())
        .map(|p| p.cast())
}

fn node(env: &Environment, object: id, name: &str) -> id {
    slot(env, object, name)
        .map(|p| env.mem.read(p.cast()))
        .unwrap_or_default()
}

pub(super) fn intercept(env: &mut Environment, class: Class, sel: SEL) -> bool {
    if env.bundle.bundle_identifier() == "hu.BV.BreakTheCookieFree"
        && env.objc.get_class_name(class) == "KKInputTouch"
        && sel.as_str(&env.mem) == "anyTouchBeganThisFrame"
    {
        let saved = *env.cpu.regs();
        let object = id::from_bits(saved[0]);
        let touches = node(env, object, "touches");
        let array: id = msg![env; touches touches];
        let count: u32 = msg![env; array count];
        let director: id = msg_class![env; CCDirector sharedDirector];
        let frame: u32 = msg![env; director frameCount];
        let mut began_this_frame = false;
        for i in 0..count {
            let touch: id = msg![env; array objectAtIndex:i];
            let phase: u32 = msg![env; touch phase];
            if let Some(p) = slot(env, touch, "touchBeganFrame") {
                let began: u32 = env.mem.read(p);
                began_this_frame |= began_on_update(began, frame, phase);
            }
        }
        env.cpu.regs_mut().copy_from_slice(&saved);
        env.cpu.regs_mut()[0] = u32::from(began_this_frame);
        return true;
    }
    if env.bundle.bundle_identifier() != "hu.BV.BTC-Olympic"
        || env.objc.get_class_name(class) != "GameManager"
    {
        return false;
    }
    let value = match sel.as_str(&env.mem) {
        "isGummyBearAllowed" | "isLaboratoryAllowed" => 1,
        "appEdition" if env.objc.btc_classic_menu_depth > 0 => 0,
        _ => return false,
    };
    env.cpu.regs_mut()[0] = value;
    true
}

pub(super) fn before(env: &mut Environment, object: id, class: Class, sel: SEL) -> Option<(id, u32)> {
    if env.bundle.bundle_identifier() != "hu.BV.BTC-Olympic" {
        return None;
    }
    let class_name = env.objc.get_class_name(class).to_owned();
    let selector = sel.as_str(&env.mem).to_owned();
    if class_name == "GameLayer" && selector == "onEnter" {
        let saved = *env.cpu.regs();
        let size: CGSize = msg![env; object contentSize];
        // Sports creates both buttons but never attaches them. Reuse its
        // original sprites and update-time ragdoll switching handlers.
        for (name, x) in [("cookieButton", 0.45), ("gummyButton", 0.55)] {
            let button = node(env, object, name);
            if button.is_null() {
                continue;
            }
            let parent: id = msg![env; button parent];
            if parent.is_null() {
                let position = CGPoint {
                    x: size.width * x,
                    y: size.height - 24.0,
                };
                () = msg![env; button setPosition:position];
                () = msg![env; object addChild:button z:100i32];
            }
        }
        env.cpu.regs_mut().copy_from_slice(&saved);
    }
    if class_name == "BackgroundSubMenu" && matches!(selector.as_str(), "loadMenu" | "loadBackground") {
        let page_slot = slot(env, object, "pageNumber")?;
        let page: u32 = env.mem.read(page_slot);
        if page >= 2 {
            // Keep Sports on page 1; append both original background pages.
            // The classic edition is visible only during menu construction.
            env.mem.write(page_slot, (page - 1).min(2));
            env.objc.btc_classic_menu_depth += 1;
        }
        return Some((object, page));
    }
    None
}

pub(super) fn after(env: &mut Environment, token: Option<(id, u32)>) {
    let Some((object, page)) = token else {
        return;
    };
    if page >= 2 {
        env.objc.btc_classic_menu_depth -= 1;
        if let Some(p) = slot(env, object, "pageNumber") {
            env.mem.write(p, page);
        }
    }
    let saved = *env.cpu.regs();
    let next = node(env, object, "nextButton");
    if !next.is_null() {
        () = msg![env; next setVisible:(page < 3)];
    }
    env.cpu.regs_mut().copy_from_slice(&saved);
}

// UIKit delivers the event after the previous draw. Free polls on the next
// director update, after frameCount increments. Accept that one frame only;
// accepting every Began phase repeats shots while the finger is held down.
fn began_on_update(began: u32, frame: u32, phase: u32) -> bool {
    phase <= 2 && began.wrapping_add(1) == frame
}

#[cfg(test)]
mod tests {
    use super::began_on_update;

    #[test]
    fn touch_begin_is_visible_for_one_update_only() {
        assert!(!began_on_update(99, 99, 0));
        assert!(began_on_update(99, 100, 0));
        assert!(began_on_update(99, 100, 1));
        assert!(!began_on_update(99, 101, 0));
        for phase in [3, 4, 6] {
            assert!(!began_on_update(99, 100, phase));
        }
        assert!(began_on_update(u32::MAX, 0, 0));
    }
}
