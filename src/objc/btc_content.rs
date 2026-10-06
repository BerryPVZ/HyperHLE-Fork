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

pub(super) enum MenuHook { Background(id, u32), Beaters(id) }

pub(super) fn before(env: &mut Environment, object: id, class: Class, sel: SEL) -> Option<MenuHook> {
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
    if class_name == "BeatersMaterialsSubMenu" && selector == "loadMenu" {
        return Some(MenuHook::Beaters(object));
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
        return Some(MenuHook::Background(object, page));
    }
    None
}

pub(super) fn after(env: &mut Environment, token: Option<MenuHook>) {
    let (object, page) = match token {
        Some(MenuHook::Background(object, page)) => (object, page),
        Some(MenuHook::Beaters(object)) => {
            let saved = *env.cpu.regs();
            add_bush_weapon(env, object);
            env.cpu.regs_mut().copy_from_slice(&saved);
            return;
        }
        None => return,
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

// Sports retains Bush's sprites and handler but leaves the last menu slot empty.
fn add_bush_weapon(env: &mut Environment, object: id) {
    let Some(page) = slot(env, object, "pageNumber") else { return };
    if env.mem.read(page) != 3 { return; }
    let menu = node(env, object, "menu");
    let torch = node(env, object, "torchButton");
    let boxbag = node(env, object, "boxbagButton");
    if menu.is_null() || torch.is_null() || boxbag.is_null() { return; }
    const BUSH_TAG: i32 = 0x42555348;
    let existing: id = msg![env; menu getChildByTag:BUSH_TAG];
    if !existing.is_null() { return; }
    let frame = crate::frameworks::foundation::ns_string::get_static_str(env, "bush_head.png");
    let normal: id = msg_class![env; CCSprite spriteWithSpriteFrameName:frame];
    let selected: id = msg_class![env; CCSprite spriteWithSpriteFrameName:frame];
    if normal.is_null() || selected.is_null() { return; }
    let selector = env.objc.register_host_selector("bushButtonTapped:".to_owned(), &mut env.mem);
    let button: id = msg_class![env; CCMenuItemSprite
        itemFromNormalSprite:normal selectedSprite:selected target:object selector:selector];
    let column: CGPoint = msg![env; boxbag position];
    let row: CGPoint = msg![env; torch position];
    let position = CGPoint { x: column.x, y: row.y };
    () = msg![env; button setPosition:position];
    () = msg![env; menu addChild:button z:0i32 tag:BUSH_TAG];
    let title = crate::frameworks::foundation::ns_string::get_static_str(env, "Bush");
    let font = crate::frameworks::foundation::ns_string::get_static_str(env, "Arial");
    let label: id = msg_class![env; CCLabelTTF labelWithString:title fontName:font fontSize:14.0f32];
    let size: CGSize = msg![env; button contentSize];
    let position = CGPoint { x: size.width / 2.0, y: -10.0 };
    () = msg![env; label setPosition:position];
    () = msg![env; button addChild:label];
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
