/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Guarded recovery of BTC Sports' native GameLayer scene components.
use super::{id, msg, msg_class, nil, objc_storeStrong, release, Class, SEL};
use crate::frameworks::core_graphics::{CGPoint, CGSize};
use crate::mem::MutPtr;
use crate::Environment;

const BODIES: [&str; 6] = ["head", "torso", "lArm", "rArm", "lLeg", "rLeg"];
const SPRITES: [&str; 6] = [
    "headSprite",
    "torsoSprite",
    "lArmSprite",
    "rArmSprite",
    "lLegSprite",
    "rLegSprite",
];

fn ivar(env: &Environment, object: id, name: &str) -> Option<MutPtr<u32>> {
    env.objc
        .object_lookup_ivar(&env.mem, object, &name.to_owned())
}

fn value(env: &Environment, object: id, name: &str) -> Option<u32> {
    ivar(env, object, name).map(|ptr| env.mem.read(ptr))
}

fn can_create_ragdoll(world: u32, bodies: &[u32; 6], sprites: &[u32; 6]) -> bool {
    world != 0 && bodies.iter().chain(sprites).all(|&ptr| ptr == 0)
}

/// Runs before the original onEnter, so newly added children receive the
/// normal Cocos lifecycle. Never replay GameLayer.init on a partial object:
/// it would overwrite live Box2D pointers, leak bodies and duplicate menus.
pub(super) fn before_on_enter(env: &mut Environment, object: id, class: Class, selector: SEL) {
    if env.bundle.bundle_identifier() != "hu.BV.BTC-Olympic"
        || selector.as_str(&env.mem) != "onEnter"
        || env.objc.get_class_name(class) != "GameLayer"
    {
        return;
    }
    // Metadata names, not fixed offsets: the app contains ARMv6 and ARMv7.
    let Some(menu_slot) = ivar(env, object, "gameMenuMain") else {
        return;
    };
    let Some(size_slot) = ivar(env, object, "screenSize") else {
        return;
    };
    let Some(world) = value(env, object, "world") else {
        return;
    };
    let mut bodies = [0; 6];
    let mut sprites = [0; 6];
    for i in 0..6 {
        let (Some(body), Some(sprite)) = (
            value(env, object, BODIES[i]),
            value(env, object, SPRITES[i]),
        ) else {
            return;
        };
        bodies[i] = body;
        sprites[i] = sprite;
    }
    let size: CGSize = env.mem.read(size_slot.cast());
    let menu = env.mem.read(menu_slot);
    log!(
        "BTC Sports GameLayer onEnter: world={:#x}, menu={:#x}, bodies={:x?}, sprites={:x?}",
        world,
        menu,
        bodies,
        sprites
    );
    if world == 0
        || !size.width.is_finite()
        || !size.height.is_finite()
        || size.width <= 0.0
        || size.height <= 0.0
        || value(env, object, "materials").unwrap_or(0) == 0
        || value(env, object, "contactListener").unwrap_or(0) == 0
    {
        log!("BTC Sports: GameLayer initialization is incomplete before scene entry; native world/size/materials/contact listener are required for safe component recovery.");
        return;
    }
    // Nested guest messages clobber argument registers. The original onEnter
    // must still receive its original receiver and selector afterwards.
    let saved = *env.cpu.regs();
    if menu == 0 {
        let new_menu: id = msg_class![env; GameMenuMain alloc];
        let new_menu: id = msg![env; new_menu init];
        if new_menu != nil {
            objc_storeStrong(env, menu_slot.cast(), new_menu);
            let _: () = msg![env; object addChild:new_menu z:101i32];
            release(env, new_menu);
            log!("BTC Sports: restored missing GameMenuMain using its native initializer.");
        } else {
            log!("BTC Sports: native GameMenuMain initialization returned nil.");
        }
    }
    if can_create_ragdoll(world, &bodies, &sprites) {
        // GameLayer.init uses the director's screenCenter, not an arbitrary
        // hard-coded position. createRagdoll: also constructs the joints.
        let director: id = msg_class![env; CCDirector sharedDirector];
        let center: CGPoint = msg![env; director screenCenter];
        if director != nil && center.x.is_finite() && center.y.is_finite() {
            let _: () = msg![env; object createRagdoll:center];
            let all_bodies = BODIES
                .iter()
                .all(|name| value(env, object, name).unwrap_or(0) != 0);
            let all_sprites = SPRITES
                .iter()
                .all(|name| value(env, object, name).unwrap_or(0) != 0);
            if all_bodies && all_sprites {
                let _: () = msg![env; object initRagdollHealth];
                log!("BTC Sports: restored all six native ragdoll bodies and sprites.");
            } else {
                log!("BTC Sports: native ragdoll recovery remains incomplete; bodies={}, sprites={}. Not recreating existing bodies.", all_bodies, all_sprites);
            }
        }
    } else if bodies.contains(&0) || sprites.contains(&0) {
        log!("BTC Sports: partial ragdoll detected; preserving existing bodies and joints rather than creating duplicates.");
    }
    env.cpu.regs_mut().copy_from_slice(&saved);
}

#[cfg(test)]
mod tests {
    use super::can_create_ragdoll;

    #[test]
    fn recovery_requires_world_and_completely_absent_ragdoll() {
        assert!(can_create_ragdoll(1, &[0; 6], &[0; 6]));
        assert!(!can_create_ragdoll(0, &[0; 6], &[0; 6]));
        assert!(!can_create_ragdoll(1, &[1; 6], &[1; 6]));
        for i in 0..6 {
            let mut partial = [0; 6];
            partial[i] = 1;
            assert!(!can_create_ragdoll(1, &partial, &[0; 6]));
            assert!(!can_create_ragdoll(1, &[0; 6], &partial));
        }
    }
}
