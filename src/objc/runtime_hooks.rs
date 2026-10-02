/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! App-scoped, reversible Objective-C return overrides. No guest bytes change.
use super::{Class, SEL};
use crate::{paths, Environment};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
enum ReturnValue {
    Nil,
    Integer(u32),
    Skip,
}
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    class_method: bool,
    class: String,
    selector: String,
    value: ReturnValue,
}

fn parse(text: &str) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        let error = || {
            format!(
                "Line {}: use +/- Class selector nil/true/false/skip/integer",
                index + 1
            )
        };
        if fields.len() != 4 || !matches!(fields[0], "+" | "-") {
            return Err(error());
        }
        let value = match fields[3] {
            "nil" => ReturnValue::Nil,
            "true" => ReturnValue::Integer(1),
            "false" => ReturnValue::Integer(0),
            "skip" => ReturnValue::Skip,
            n => {
                let bits = if let Some(hex) = n.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    n.parse::<i32>()
                        .map(|v| v as u32)
                        .ok()
                        .or_else(|| n.parse::<u32>().ok())
                }
                .ok_or_else(error)?;
                ReturnValue::Integer(bits)
            }
        };
        let rule = Rule {
            class_method: fields[0] == "+",
            class: fields[1].into(),
            selector: fields[2].into(),
            value,
        };
        if rules.iter().any(|r: &Rule| {
            r.class_method == rule.class_method
                && r.class == rule.class
                && r.selector == rule.selector
        }) {
            return Err(format!("Line {}: duplicate method override", index + 1));
        }
        rules.push(rule);
    }
    Ok(rules)
}

pub fn profile_path(bundle_id: &str) -> PathBuf {
    // Encode identifiers rather than allowing them to become path components.
    let name: String = bundle_id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    paths::user_data_base_path()
        .join("touchHLE_flex")
        .join(format!("{name}.txt"))
}
pub fn load(bundle_id: &str) -> Result<String, String> {
    match std::fs::read_to_string(profile_path(bundle_id)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}
pub fn save(bundle_id: &str, text: &str) -> Result<(), String> {
    parse(text)?;
    let path = profile_path(bundle_id);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn replacement(value: &ReturnValue, signature: &[u8]) -> Option<u32> {
    let ty = *signature.iter().find(|b| !b"rnNoORV".contains(b))?;
    match (value, ty) {
        (ReturnValue::Skip, b'v') => Some(0),
        (ReturnValue::Nil, b'@' | b'#' | b'^' | b'*') => Some(0),
        (ReturnValue::Integer(v), b'B') if *v <= 1 => Some(*v),
        (ReturnValue::Integer(v), b'c') if (*v as i32) >= -128 && (*v as i32) <= 127 => Some(*v),
        (ReturnValue::Integer(v), b'C') if *v <= 255 => Some(*v),
        (ReturnValue::Integer(v), b's') if (*v as i32) >= -32768 && (*v as i32) <= 32767 => {
            Some(*v)
        }
        (ReturnValue::Integer(v), b'S') if *v <= 65535 => Some(*v),
        (ReturnValue::Integer(v), b'i' | b'I' | b'l' | b'L') => Some(*v),
        _ => None,
    }
}
#[derive(Default)]
struct Cache {
    bundle: String,
    checked: Option<Instant>,
    rules: Vec<Rule>,
}

pub(super) fn intercept(env: &mut Environment, class: Class, selector: SEL) -> bool {
    if !env.options.runtime_hooks_enabled {
        return false;
    }
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let bundle = env.bundle.bundle_identifier();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .unwrap();
    if cache.bundle != bundle
        || cache
            .checked
            .map_or(true, |t| t.elapsed() >= Duration::from_millis(500))
    {
        cache.bundle = bundle.to_owned();
        cache.checked = Some(Instant::now());
        cache.rules = match load(bundle).and_then(|s| parse(&s)) {
            Ok(rules) => rules,
            Err(e) => {
                log!("Flex profile ignored: {}", e);
                Vec::new()
            }
        };
    }
    let name = env.objc.get_class_name(class);
    let selector_name = selector.as_str(&env.mem);
    let Some(host) = env.objc.get_host_object(class) else {
        return false;
    };
    let Some(class_object) = host.as_any().downcast_ref::<super::ClassHostObject>() else {
        return false;
    };
    let class_method = class_object.is_metaclass;
    let Some(rule) = cache
        .rules
        .iter()
        .find(|r| r.class == name && r.selector == selector_name && r.class_method == class_method)
    else {
        return false;
    };
    // Only guest methods expose signatures. Host framework methods are untouched.
    let Some(signature) = env.objc.class_get_method_signature(class, selector) else {
        return false;
    };
    let Some(value) = replacement(&rule.value, env.mem.cstr_at(*signature)) else {
        return false;
    };
    env.cpu.regs_mut()[0] = value;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_validate_and_disable_rules() {
        assert_eq!(
            parse("# - Gate online true\n- Gate online false\n+ Gate token nil")
                .unwrap()
                .len(),
            2
        );
        assert!(parse("- Gate online false\n- Gate online true").is_err());
        assert!(parse("- Gate online 4294967296").is_err());
        assert!(parse("- Gate online garbage").is_err());
        assert_eq!(
            parse("- Gate code -1").unwrap()[0].value,
            ReturnValue::Integer(u32::MAX)
        );
    }
    #[test]
    fn respects_guest_return_abi() {
        assert_eq!(replacement(&ReturnValue::Integer(0), b"c8@0:4"), Some(0));
        assert_eq!(replacement(&ReturnValue::Nil, b"r@8@0:4"), Some(0));
        assert_eq!(replacement(&ReturnValue::Skip, b"v8@0:4"), Some(0));
        for ty in [b"{CGRect=ffff}".as_slice(), b"d", b"f", b"q", b"@", b"v"] {
            assert_eq!(replacement(&ReturnValue::Integer(1), ty), None);
        }
        assert_eq!(replacement(&ReturnValue::Skip, b"i"), None);
        assert_eq!(replacement(&ReturnValue::Integer(256), b"C"), None);
    }
}
