/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSInvocation` and `NSMethodSignature`.

use crate::abi::{extend_stack_for_args, write_next_arg};
use crate::cpu::Cpu;
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::libc::string::strdup;
use crate::mem::{ConstPtr, MutPtr, MutVoidPtr};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, objc_msgSend, release, retain,
    ClassExports, HostObject, NSZonePtr, SEL,
};

// =========================================================================
// MARK: - NSMethodSignature Host Object
// =========================================================================

#[derive(Default)]
struct NSMethodSignatureHostObject {
    return_type: String,
    argument_types: Vec<String>,
    // Кэшированные указатели на строки в памяти гостя для возврата через методы
    return_type_ptr: Option<MutPtr<u8>>,
    argument_type_ptrs: Vec<Option<MutPtr<u8>>>,
}
impl HostObject for NSMethodSignatureHostObject {}

// =========================================================================
// MARK: - NSInvocation Host Object
// =========================================================================

#[derive(Default)]
struct NSInvocationHostObject {
    /// `NSMethodSignature *`
    sig: id,
    /// Строки типов аргументов, полученные из `sig` во время создания
    argument_types: Vec<String>,
    argument_sizes: Vec<u32>,
    target: id,
    selector: Option<SEL>,
    /// Выделенный буфер для каждого аргумента. Option указывает, был ли
    //аргумент задан через `setArgument:atIndex:`
    arguments: Vec<Option<MutVoidPtr>>,
    arguments_retained: bool,
    /// Объекты, удержанные через `retainArguments`
    retained_objects: Vec<id>,
    /// Копии C-строк, созданные через `retainArguments`
    copied_strings: Vec<MutPtr<u8>>,
}
impl HostObject for NSInvocationHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// =========================================================================
// MARK: - NSMethodSignature
// =========================================================================

@implementation NSMethodSignature: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSMethodSignatureHostObject {
        return_type: String::from("v"),
        argument_types: vec![String::from("@"), String::from(":")], // self, _cmd
        return_type_ptr: None,
        argument_type_ptrs: vec![None, None],
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)signatureWithObjCTypes:(MutVoidPtr)_types {
    let sig: id = msg_class![env; NSMethodSignature alloc];
    let sig: id = msg![env; sig init];

    if !_types.is_null() {
        // ИСПРАВЛЕНИЕ E0308: Приводим Ptr<c_void> к Ptr<u8> через .cast()
        let types_str = env.mem.cstr_at_utf8(_types.cast_const().cast()).unwrap_or("");
        let mut parsed_types = Vec::new();
        let mut chars = types_str.chars().peekable();

        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                chars.next(); // Игнорируем размеры и смещения
                continue;
            }

            let mut current_type = String::new();

            // Читаем модификаторы (const, in, inout, out, bycopy, byref,
            // oneway) и указатели
            while let Some(&m) = chars.peek() {
                if "rnNoORV^".contains(m) {
                    current_type.push(chars.next().unwrap());
                } else {
                    break;
                }
            }

            // Читаем основной тип (включая структуры, массивы и объединения)
            if let Some(c) = chars.next() {
                current_type.push(c);
                if c == '{' || c == '[' || c == '(' {
                    let (open, close) = match c {
                        '{' => ('{', '}'),
                        '[' => ('[', ']'),
                        '(' => ('(', ')'),
                        _ => unreachable!()
                    };
                    let mut depth = 1;
                    for sc in chars.by_ref() {
                        current_type.push(sc);
                        if sc == open { depth += 1; }
                        else if sc == close {
                            depth -= 1;
                            if depth == 0 { break; }
                        }
                    }
                } else if c == '@' {
                    if chars.peek() == Some(&'?') {
                        current_type.push(chars.next().unwrap());
                    } else if chars.peek() == Some(&'"') {
                        current_type.push(chars.next().unwrap());
                        for sc in chars.by_ref() {
                            current_type.push(sc);
                            if sc == '"' { break; }
                        }
                    }
                } else if c == '"' {
                    // Обработка именованных полей в структурах
                    for sc in chars.by_ref() {
                        current_type.push(sc);
                        if sc == '"' { break; }
                    }
                }
            }

            if !current_type.is_empty() {
                parsed_types.push(current_type);
            }
        }

        if !parsed_types.is_empty() {
            let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(sig);
            host.return_type = parsed_types.remove(0);
            host.argument_type_ptrs = vec![None; parsed_types.len()];
            host.argument_types = parsed_types;
            host.return_type_ptr = None;
        }
    }

    autorelease(env, sig)
}

- (id)init {
    this
}

- (NSUInteger)numberOfArguments {
    env.objc.borrow::<NSMethodSignatureHostObject>(this).argument_types.len() as NSUInteger
}

- (())_touchHLE_setNumberOfArguments:(NSUInteger)count {
    let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(this);
    let count_usize = count as usize;
    host.argument_types.resize(count_usize, String::from("@"));
    host.argument_type_ptrs.resize(count_usize, None);
}

- (crate::mem::ConstPtr<std::ffi::c_char>)methodReturnType {
    let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(this);
    if host.return_type_ptr.is_none() {
        let bytes = host.return_type.as_bytes();
        let ptr: crate::mem::MutPtr<u8> = env.mem.alloc(bytes.len() as u32 + 1).cast();
        for (i, &b) in bytes.iter().enumerate() {
            // ИСПРАВЛЕНИЕ E0599: Используем оператор сложения (ptr + i) вместо
            // .offset()
            env.mem.write(ptr + (i as u32), b);
        }
        env.mem.write(ptr + (bytes.len() as u32), 0u8);
        host.return_type_ptr = Some(ptr);
    }
    host.return_type_ptr.unwrap().cast_const().cast()
}

- (crate::mem::ConstPtr<std::ffi::c_char>)getArgumentTypeAtIndex:(NSUInteger)_index {
    let host = env.objc.borrow_mut::<NSMethodSignatureHostObject>(this);
    let idx = _index as usize;

    if idx < host.argument_types.len() {
        if host.argument_type_ptrs[idx].is_none() {
            let bytes = host.argument_types[idx].as_bytes();
            let ptr: crate::mem::MutPtr<u8> = env.mem.alloc(bytes.len() as u32 + 1).cast();
            for (i, &b) in bytes.iter().enumerate() {
                // ИСПРАВЛЕНИЕ E0599: Используем ptr + i
                env.mem.write(ptr + (i as u32), b);
            }
            env.mem.write(ptr + (bytes.len() as u32), 0u8);
            host.argument_type_ptrs[idx] = Some(ptr);
        }
        host.argument_type_ptrs[idx].unwrap().cast_const().cast()
    } else {
        // Fallback на случай выхода за границы
        let ptr: crate::mem::MutPtr<u8> = env.mem.alloc(2).cast();
        // ИСПРАВЛЕНИЕ E0599: Используем ptr и ptr + 1
        env.mem.write(ptr, b'@');
        env.mem.write(ptr + 1u32, 0);
        ptr.cast_const().cast()
    }
}

- (NSUInteger)methodReturnLength {
    let ret_type_ptr: crate::mem::ConstPtr<std::ffi::c_char> = msg![env; this methodReturnType];
    if ret_type_ptr.is_null() {
        return 0;
    }

    let ret_type_str = env.mem.cstr_at_utf8(ret_type_ptr.cast()).unwrap_or("");
    let core_type = ret_type_str.trim_start_matches(|c| "rnNoORV".contains(c));

    match core_type.chars().next() {
        Some('v') => 0,
        Some('c') | Some('C') | Some('B') => 1,
        Some('s') | Some('S') => 2,
        Some('i') | Some('I') | Some('l') | Some('L') | Some('f') => 4,
        Some('q') | Some('Q') | Some('d') => 8,
        Some('@') | Some('#') | Some('*') | Some('^') | Some(':') | Some('?') => 4,
        Some('{') => {
            log!("Warning: methodReturnLength for struct {} not calculated", core_type);
            0
        }
        _ => {
            log!("Warning: methodReturnLength unknown type '{}', returning 4", core_type);
            4
        }
    }
}

- (())dealloc {
    {
        let host = env.objc.borrow::<NSMethodSignatureHostObject>(this);
        if let Some(ptr) = host.return_type_ptr {
            env.mem.free(ptr.cast());
        }
        for ptr in host.argument_type_ptrs.iter().flatten() {
            env.mem.free(ptr.cast());
        }
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

// =========================================================================
// MARK: - NSInvocation
// =========================================================================

@implementation NSInvocation: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSInvocationHostObject {
        sig: nil,
        argument_types: Vec::new(),
        argument_sizes: Vec::new(),
        target: nil,
        selector: None,
        arguments: Vec::new(),
        arguments_retained: false,
        retained_objects: Vec::new(),
        copied_strings: Vec::new(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)invocationWithMethodSignature:(id)sig {
    retain(env, sig);
    let num_of_args: NSUInteger = msg![env; sig numberOfArguments];
    let mut argument_types: Vec<String> = Vec::with_capacity(num_of_args as usize);
    let mut argument_sizes = Vec::with_capacity(num_of_args as usize);

    for i in 0..num_of_args {
        let type_ptr: ConstPtr<u8> = msg![env; sig getArgumentTypeAtIndex:i];
        let (_, size, _) = super::parse_objc_type(env, type_ptr);
        argument_sizes.push(size);
        let encoding = env.mem.cstr_at_utf8(type_ptr).unwrap();
        let encoding = encoding.trim_start_matches(['r', 'n', 'N', 'o', 'O', 'R', 'V']);
        argument_types.push(if encoding.starts_with("@\"") {
            "@".to_owned()
        } else {
            encoding.to_owned()
        });
    }

    let host_object = Box::new(NSInvocationHostObject {
        sig,
        argument_types,
        argument_sizes,
        target: nil,
        selector: None,
        arguments: vec![None; num_of_args as usize],
        arguments_retained: false,
        retained_objects: Vec::new(),
        copied_strings: Vec::new(),
    });
    let res = env.objc.alloc_object(this, host_object, &mut env.mem);
    autorelease(env, res)
}

- (id)init {
    this
}

- (id)methodSignature {
    env.objc.borrow::<NSInvocationHostObject>(this).sig
}

- (id)target {
    env.objc.borrow::<NSInvocationHostObject>(this).target
}

- (())setTarget:(id)target {
    let old_target = env.objc.borrow::<NSInvocationHostObject>(this).target;
    let arguments_retained = env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained;
    env.objc.borrow_mut::<NSInvocationHostObject>(this).target = target;
    if arguments_retained {
        retain(env, target);
        release(env, old_target);
    }
}

- (SEL)selector {
    env.objc.borrow::<NSInvocationHostObject>(this).selector.expect("NSInvocation selector not set")
}

- (())setSelector:(SEL)selector {
    assert!(env.objc.borrow_mut::<NSInvocationHostObject>(this).selector.is_none());
    env.objc.borrow_mut::<NSInvocationHostObject>(this).selector = Some(selector);
}

- (bool)argumentsRetained {
    env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained
}

- (())retainArguments {
    assert!(!env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained);

    let target = env.objc.borrow::<NSInvocationHostObject>(this).target;
    retain(env, target);

    let mut retained_objects: Vec<id> = Vec::new();
    let mut copied_strings: Vec<MutPtr<u8>> = Vec::new();

    let num_of_args = env.objc.borrow::<NSInvocationHostObject>(this).argument_types.len();
    for i in 2..num_of_args {
        let host = env.objc.borrow::<NSInvocationHostObject>(this);
        let Some(arg_loc) = host.arguments[i] else { continue };
        match host.argument_types[i].as_str() {
            "@" => {
                let obj: id = env.mem.read(arg_loc.cast().cast_const());
                retain(env, obj);
                retained_objects.push(obj);
            }
            "*" => {
                let str: MutPtr<u8> = env.mem.read(arg_loc.cast().cast_const());
                let str_copy = strdup(env, str.cast_const());
                env.mem.write(arg_loc.cast(), str_copy);
                copied_strings.push(str_copy);
            }
            _ => {}
        }
    }

    let host = env.objc.borrow_mut::<NSInvocationHostObject>(this);
    host.retained_objects = retained_objects;
    host.copied_strings = copied_strings;
    host.arguments_retained = true;
}

- (())getArgument:(MutVoidPtr)buffer atIndex:(NSInteger)index {
    let host = env.objc.borrow::<NSInvocationHostObject>(this);
    if let Some(arg_loc) = host.arguments.get(index as usize).and_then(|&a| a) {
        let arg_type = host.argument_types.get(index as usize).map(|s| s.as_str()).unwrap_or("");
        match arg_type {
            "f" => {
                let val: f32 = env.mem.read(arg_loc.cast().cast_const());
                env.mem.write(buffer.cast(), val);
            }
            "@" => {
                let val: id = env.mem.read(arg_loc.cast().cast_const());
                env.mem.write(buffer.cast(), val);
            }
            "*" => {
                let val: MutPtr<u8> = env.mem.read(arg_loc.cast().cast_const());
                env.mem.write(buffer.cast(), val);
            }
            _ if arg_type.starts_with('^') => {
                let val: MutVoidPtr = env.mem.read(arg_loc.cast().cast_const());
                env.mem.write(buffer.cast(), val);
            }
            _ => {
                let val: u32 = env.mem.read(arg_loc.cast().cast_const());
                env.mem.write(buffer.cast(), val);
            }
        }
    }
}

- (())setArgument:(MutVoidPtr)arg_loc atIndex:(NSInteger)idx {
    let arguments_retained =
        env.objc.borrow::<NSInvocationHostObject>(this).arguments_retained;
    let args_len =
        env.objc.borrow::<NSInvocationHostObject>(this).arguments.len();

    assert!(1 < idx && (idx as usize) < args_len);

    let prev_arg =
        env.objc.borrow::<NSInvocationHostObject>(this).arguments[idx as usize];
    let arg_type =
        env.objc.borrow::<NSInvocationHostObject>(this)
            .argument_types[idx as usize].clone();

    if arguments_retained {
        if let Some(prev_buf) = prev_arg {
            match arg_type.as_str() {
                "@" => {
                    let old_obj: id = env.mem.read(prev_buf.cast().cast_const());
                    {
                        let host = env.objc.borrow_mut::<NSInvocationHostObject>(this);
                        let mut found = host.retained_objects.len();
                        for ri in 0..host.retained_objects.len() {
                            if host.retained_objects[ri] == old_obj {
                                found = ri;
                                break;
                            }
                        }
                        if found < host.retained_objects.len() {
                            host.retained_objects.swap_remove(found);
                        }
                    }
                    release(env, old_obj);
                }
                "*" => {
                    let old_str: MutPtr<u8> = env.mem.read(prev_buf.cast().cast_const());
                    {
                        let host = env.objc.borrow_mut::<NSInvocationHostObject>(this);
                        let mut found = host.copied_strings.len();
                        for si in 0..host.copied_strings.len() {
                            if host.copied_strings[si] == old_str {
                                found = si;
                                break;
                            }
                        }
                        if found < host.copied_strings.len() {
                            host.copied_strings.swap_remove(found);
                        }
                    }
                    env.mem.free(old_str.cast());
                }
                _ => {}
            }
        }
    }

    if let Some(prev_buf) = prev_arg {
        env.mem.free(prev_buf.cast());
    }

    let new: MutVoidPtr = match arg_type.as_str() {
        "f" => {
            let arg_loc: MutPtr<f32> = arg_loc.cast();
            let arg = env.mem.read(arg_loc);
            env.mem.alloc_and_write(arg).cast()
        }
        "@" => {
            let arg_loc: MutPtr<id> = arg_loc.cast();
            let arg = env.mem.read(arg_loc);
            if arguments_retained {
                retain(env, arg);
                env.objc
                    .borrow_mut::<NSInvocationHostObject>(this)
                    .retained_objects
                    .push(arg);
            }
            env.mem.alloc_and_write(arg).cast()
        }
        "*" => {
            let arg_loc: MutPtr<MutPtr<u8>> = arg_loc.cast();
            let arg = env.mem.read(arg_loc);
            if arguments_retained {
                let str_copy = strdup(env, arg.cast_const());
                env.objc
                    .borrow_mut::<NSInvocationHostObject>(this)
                    .copied_strings
                    .push(str_copy);
                env.mem.alloc_and_write(str_copy).cast()
            } else {
                env.mem.alloc_and_write(arg).cast()
            }
        }
        _ if arg_type.starts_with('^') => {
            let arg_loc: MutPtr<MutVoidPtr> = arg_loc.cast();
            let arg = env.mem.read(arg_loc);
            env.mem.alloc_and_write(arg).cast()
        }
        _ => {
            let size = env.objc.borrow::<NSInvocationHostObject>(this).argument_sizes[idx as usize];
            let bytes = env.mem.bytes_at(arg_loc.cast(), size).to_vec();
            let buffer = env.mem.alloc(size);
            env.mem.bytes_at_mut(buffer.cast(), size).copy_from_slice(&bytes);
            buffer
        }
    };

    env.objc
        .borrow_mut::<NSInvocationHostObject>(this)
        .arguments[idx as usize] = Some(new);
}

- (())invokeWithTarget:(id)target {
    () = msg![env; this setTarget:target];
    () = msg![env; this invoke];
}

- (())invoke {
    let selector_opt = env.objc.borrow::<NSInvocationHostObject>(this).selector;
    if selector_opt.is_none() {
        log!("Warning: NSInvocation invoked without a selector!");
        return;
    }

    let host = env.objc.borrow::<NSInvocationHostObject>(this);
    if host.arguments.iter().skip(2).any(Option::is_none) {
        log!("Warning: NSInvocation has unset arguments; using zero without shifting later arguments");
    }

    let sig = env.objc.borrow::<NSInvocationHostObject>(this).sig;

    if sig != nil {
        let ret_type: ConstPtr<u8> = msg![env; sig methodReturnType];
        if !ret_type.is_null() {
            let ret_char = env.mem.read(ret_type);
            if ret_char != b'v' {
                log!("Warning: NSInvocation return type is '{}', expected 'v'. Invoking anyway.", ret_char as char);
            }
        }
    }

    let host = env.objc.borrow::<NSInvocationHostObject>(this);
    let mut words = vec![host.target.to_bits(), host.selector.unwrap().to_bits()];
    for i in 2..host.arguments.len() {
        let size = host.argument_sizes[i] as usize;
        let bytes = match host.arguments[i] {
            Some(ptr) => env.mem.bytes_at(ptr.cast(), size as u32).to_vec(),
            None => vec![0; size],
        };
        words.extend(argument_words(&host.argument_types[i], &bytes));
    }
    let old_sp = extend_stack_for_args(words.len(), env.cpu.regs_mut());
    let mut reg_offset = 0;
    for word in words {
        write_next_arg(&mut reg_offset, env.cpu.regs_mut(), &mut env.mem, word);
    }

    let &NSInvocationHostObject { target, selector, .. } = env.objc.borrow::<NSInvocationHostObject>(this);
    objc_msgSend(env, target, selector.unwrap());

    let regs = env.cpu.regs_mut();
    regs[Cpu::SP] = old_sp;
}

- (())dealloc {
    let &NSInvocationHostObject { sig, target, arguments_retained, .. } = env.objc.borrow::<NSInvocationHostObject>(this);
    release(env, sig);

    if arguments_retained {
        release(env, target);
        let retained_objects = std::mem::take(
            &mut env.objc.borrow_mut::<NSInvocationHostObject>(this).retained_objects
        );
        for obj in retained_objects {
            release(env, obj);
        }
        let copied_strings = std::mem::take(
            &mut env.objc.borrow_mut::<NSInvocationHostObject>(this).copied_strings
        );
        for s in copied_strings {
            env.mem.free(s.cast());
        }
    } else {
        assert!(env.objc.borrow::<NSInvocationHostObject>(this).retained_objects.is_empty());
        assert!(env.objc.borrow::<NSInvocationHostObject>(this).copied_strings.is_empty());
    }

    for ptr in env.objc.borrow::<NSInvocationHostObject>(this).arguments.iter().flatten() {
        env.mem.free(ptr.cast());
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};


/// The iPhone OS ABI passes aggregates and wide scalars as consecutive words.
/// Pad the last word, and extend signed narrow scalars to a full register.
fn argument_words(encoding: &str, bytes: &[u8]) -> Vec<u32> {
    match (encoding, bytes) {
        ("c", [value]) => return vec![*value as i8 as i32 as u32],
        ("s", [a, b]) => return vec![i16::from_le_bytes([*a, *b]) as i32 as u32],
        _ => {}
    }
    bytes
        .chunks(4)
        .map(|chunk| {
            let mut word = [0; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(word)
        })
        .collect()
}

#[cfg(test)]
mod argument_tests {
    use super::argument_words;
    #[test]
    fn preserves_wide_and_aggregate_arguments() {
        assert_eq!(
            argument_words("d", &1.0_f64.to_le_bytes()),
            vec![0, 0x3ff00000]
        );
        assert_eq!(
            argument_words("q", &(-1_i64).to_le_bytes()),
            vec![u32::MAX; 2]
        );
        assert_eq!(argument_words("{color=CCC}", &[1, 2, 3]), vec![0x030201]);
        assert_eq!(argument_words("c", &[0xff]), vec![u32::MAX]);
        assert_eq!(argument_words("s", &[0xfe, 0xff]), vec![u32::MAX - 1]);
    }
    #[test]
    fn unset_wide_argument_keeps_following_argument_in_place() {
        let mut words = argument_words("d", &[0; 8]);
        words.extend(argument_words("i", &42_u32.to_le_bytes()));
        assert_eq!(words, vec![0, 0, 42]);
    }
}
