/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Read Objective-C 2 metadata without loading or executing an app.
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodInfo {
    pub class_method: bool,
    pub selector: String,
    pub encoding: String,
}
impl MethodInfo {
    pub fn return_description(&self) -> &'static str {
        match self.encoding.bytes().find(|b| !b"rnNoORV".contains(b)) {
            Some(b'v') => "void",
            Some(b'@') => "object",
            Some(b'#') => "class",
            Some(b'^' | b'*') => "pointer",
            Some(b'B') => "bool",
            Some(b'c' | b'C') => "char / BOOL",
            Some(b's' | b'S') => "16-bit integer",
            Some(b'i' | b'I' | b'l' | b'L') => "32-bit integer",
            _ => "unsupported return type",
        }
    }

    pub fn accepts(&self, value: &str) -> bool {
        let ty = self.encoding.bytes().find(|b| !b"rnNoORV".contains(b));
        match value {
            "nil" => matches!(ty, Some(b'@' | b'#' | b'^' | b'*')),
            "skip" => ty == Some(b'v'),
            "true" | "false" => matches!(
                ty,
                Some(b'B' | b'c' | b'C' | b's' | b'S' | b'i' | b'I' | b'l' | b'L')
            ),
            _ => false,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub superclass: Option<String>,
    pub methods: Vec<MethodInfo>,
}

fn word(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("Metadata offset overflow")?;
    let b = bytes
        .get(offset..end)
        .ok_or("Truncated executable metadata")?;
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}
fn arm_slice(bytes: &[u8]) -> Result<&[u8], String> {
    if bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        let be = |offset: usize| word(bytes, offset).map(u32::swap_bytes);
        let count = be(4)? as usize;
        if count > 64 {
            return Err("Too many universal binary slices".into());
        }
        let mut selected = None;
        for index in 0..count {
            let offset = 8 + index * 20;
            if be(offset)? != 12 {
                continue;
            }
            let subtype = be(offset + 4)? & 0xff;
            let rank = match subtype {
                9 => 4,
                11 => 3,
                10 => 2,
                6 => 1,
                _ => 0,
            };
            let start = be(offset + 8)? as usize;
            let size = be(offset + 12)? as usize;
            let end = start.checked_add(size).ok_or("Universal slice overflow")?;
            let slice = bytes.get(start..end).ok_or("Truncated universal slice")?;
            if selected.as_ref().map_or(true, |&(r, _)| rank > r) {
                selected = Some((rank, slice));
            }
        }
        return selected
            .map(|(_, slice)| slice)
            .ok_or_else(|| "No ARM32 executable slice".into());
    }
    Ok(bytes)
}
struct Reader<'a> {
    bytes: &'a [u8],
    // VM address, size of file-backed bytes, file offset.
    segments: Vec<(u32, u32, u32)>,
    method_budget: std::cell::Cell<usize>,
}
impl Reader<'_> {
    fn offset(&self, address: u32, size: usize) -> Result<usize, String> {
        for &(start, length, file_offset) in &self.segments {
            if address < start {
                continue;
            }
            let delta = address - start;
            if (delta as u64) + (size as u64) <= length as u64 {
                let offset = file_offset as usize + delta as usize;
                if offset
                    .checked_add(size)
                    .map_or(false, |end| end <= self.bytes.len())
                {
                    return Ok(offset);
                }
            }
        }
        Err(format!(
            "Unmapped Objective-C metadata address 0x{address:08x}"
        ))
    }
    fn word(&self, address: u32) -> Result<u32, String> {
        word(self.bytes, self.offset(address, 4)?)
    }
    fn at(&self, address: u32, delta: u32) -> Result<u32, String> {
        self.word(
            address
                .checked_add(delta)
                .ok_or("Metadata address overflow")?,
        )
    }
    fn string(&self, address: u32) -> Result<String, String> {
        let offset = self.offset(address, 1)?;
        let mut end = offset;
        while end - offset < 4096 {
            self.offset(
                address
                    .checked_add((end - offset) as u32)
                    .ok_or("String address overflow")?,
                1,
            )?;
            if self.bytes[end] == 0 {
                return std::str::from_utf8(&self.bytes[offset..end])
                    .map(str::to_owned)
                    .map_err(|_| "Unreadable Objective-C name or encoding".into());
            }
            end += 1;
        }
        Err("Unterminated Objective-C string".into())
    }
    fn class_name(&self, address: u32) -> Result<String, String> {
        let data = self.at(address, 16)? & !3;
        self.string(self.at(data, 16)?)
    }
    fn methods(&self, list: u32, class_method: bool) -> Result<Vec<MethodInfo>, String> {
        if list == 0 {
            return Ok(Vec::new());
        }
        let stride = self.word(list)?;
        let count = self.at(list, 4)?;
        if !(12..=1024).contains(&stride) || count > 100_000 {
            return Err("Unsupported or malformed Objective-C method list".into());
        }
        let remaining = self
            .method_budget
            .get()
            .checked_sub(count as usize)
            .ok_or("Too many methods in executable metadata")?;
        self.method_budget.set(remaining);
        let mut methods = Vec::new();
        for index in 0..count {
            let delta = index
                .checked_mul(stride)
                .and_then(|v| v.checked_add(8))
                .ok_or("Method list overflow")?;
            let entry = list.checked_add(delta).ok_or("Method address overflow")?;
            methods.push(MethodInfo {
                class_method,
                selector: self.string(self.word(entry)?)?,
                encoding: self.string(self.at(entry, 4)?)?,
            });
        }
        Ok(methods)
    }
    fn class_methods(&self, class: u32, class_method: bool) -> Result<Vec<MethodInfo>, String> {
        if class == 0 {
            return Ok(Vec::new());
        }
        let data = self.at(class, 16)? & !3;
        self.methods(self.at(data, 20)?, class_method)
    }
}

pub fn browse(bytes: &[u8]) -> Result<Vec<ClassInfo>, String> {
    let bytes = arm_slice(bytes)?;
    if word(bytes, 0)? != 0xfeedface || word(bytes, 4)? != 12 {
        return Err("Browser supports little-endian ARM32 Mach-O apps".into());
    }
    let count = word(bytes, 16)? as usize;
    if count > 4096 {
        return Err("Too many Mach-O load commands".into());
    }
    let commands_end = 28usize
        .checked_add(word(bytes, 20)? as usize)
        .ok_or("Load command overflow")?;
    if commands_end > bytes.len() {
        return Err("Truncated Mach-O load commands".into());
    }
    let mut reader = Reader {
        bytes,
        segments: Vec::new(),
        method_budget: std::cell::Cell::new(100_000),
    };
    let mut sections = Vec::new();
    let mut cursor = 28usize;
    for _ in 0..count {
        let command = word(bytes, cursor)?;
        let size = word(bytes, cursor + 4)? as usize;
        let end = cursor.checked_add(size).ok_or("Load command overflow")?;
        if size < 8 || end > commands_end {
            return Err("Invalid Mach-O load command".into());
        }
        if command == 0x21 {
            if size < 20 {
                return Err("Truncated encryption command".into());
            }
            if word(bytes, cursor + 16)? != 0 {
                return Err(
                    "Executable is encrypted; use a decrypted IPA to browse methods".into(),
                );
            }
        }
        if command == 1 {
            if size < 56 {
                return Err("Truncated segment command".into());
            }
            let address = word(bytes, cursor + 24)?;
            let file_offset = word(bytes, cursor + 32)?;
            let file_size = word(bytes, cursor + 36)?;
            if file_offset as u64 + file_size as u64 > bytes.len() as u64 {
                return Err("Truncated Mach-O segment".into());
            }
            reader.segments.push((address, file_size, file_offset));
            let nsects = word(bytes, cursor + 48)? as usize;
            if nsects > (size - 56) / 68 {
                return Err("Truncated Mach-O section table".into());
            }
            for index in 0..nsects {
                let section = cursor + 56 + index * 68;
                let name_bytes = &bytes[section..section + 16];
                let name = &name_bytes[..name_bytes.iter().position(|&b| b == 0).unwrap_or(16)];
                if name == b"__objc_classlist" || name == b"__objc_catlist" {
                    sections.push((
                        name == b"__objc_catlist",
                        word(bytes, section + 32)?,
                        word(bytes, section + 36)?,
                    ));
                }
            }
        }
        cursor = end;
    }
    let mut classes: BTreeMap<String, ClassInfo> = BTreeMap::new();
    for &(category, address, size) in &sections {
        if category {
            continue;
        }
        if size % 4 != 0 || size / 4 > 10000 {
            return Err("Invalid Objective-C class list".into());
        }
        for index in 0..size / 4 {
            let class = reader.at(address, index * 4)?;
            let name = reader.class_name(class)?;
            let superclass = reader.at(class, 4)?;
            let superclass = if superclass == 0 {
                None
            } else {
                reader.class_name(superclass).ok()
            };
            let mut methods = reader.class_methods(class, false)?;
            methods.extend(reader.class_methods(reader.word(class)?, true)?);
            classes.insert(
                name.clone(),
                ClassInfo {
                    name,
                    superclass,
                    methods,
                },
            );
        }
    }
    for &(category, address, size) in &sections {
        if !category {
            continue;
        }
        if size % 4 != 0 || size / 4 > 10000 {
            return Err("Invalid Objective-C category list".into());
        }
        for index in 0..size / 4 {
            let category = reader.at(address, index * 4)?;
            // External category class pointers need dyld binding; omit them.
            let class = reader.at(category, 4)?;
            if class == 0 {
                continue;
            }
            let name = reader.class_name(class)?;
            if let Some(info) = classes.get_mut(&name) {
                let mut methods = reader.methods(reader.at(category, 8)?, false)?;
                methods.extend(reader.methods(reader.at(category, 12)?, true)?);
                methods.append(&mut info.methods);
                info.methods = methods;
            }
        }
    }
    for class in classes.values_mut() {
        class
            .methods
            .sort_by(|a, b| (a.class_method, &a.selector).cmp(&(b.class_method, &b.selector)));
        class
            .methods
            .dedup_by(|a, b| a.class_method == b.class_method && a.selector == b.selector);
    }
    Ok(classes.into_values().collect())
}

/// Include inherited guest methods but retain the selected receiver class.
pub fn methods_for(classes: &[ClassInfo], index: usize) -> Vec<MethodInfo> {
    let mut methods = BTreeMap::new();
    let mut class = classes.get(index);
    let mut visited = std::collections::HashSet::new();
    while let Some(info) = class {
        if !visited.insert(&info.name) {
            break;
        }
        for method in &info.methods {
            methods
                .entry((method.class_method, method.selector.clone()))
                .or_insert_with(|| method.clone());
        }
        class = info
            .superclass
            .as_ref()
            .and_then(|name| classes.iter().find(|c| &c.name == name));
    }
    methods.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(bytes: &mut [u8], offset: usize, n: u32) {
        bytes[offset..offset + 4].copy_from_slice(&n.to_le_bytes());
    }
    fn fixture() -> Vec<u8> {
        let mut b = vec![0; 1024];
        put(&mut b, 0, 0xfeedface);
        put(&mut b, 4, 12);
        put(&mut b, 16, 1);
        put(&mut b, 20, 124);
        put(&mut b, 28, 1);
        put(&mut b, 32, 124);
        put(&mut b, 52, 0x1000);
        put(&mut b, 64, 1024);
        put(&mut b, 76, 1);
        b[84..100].copy_from_slice(b"__objc_classlist");
        put(&mut b, 116, 0x1100);
        put(&mut b, 120, 4);
        put(&mut b, 256, 0x1120);
        put(&mut b, 288, 0x1160);
        put(&mut b, 304, 0x1140);
        put(&mut b, 368, 0x1180);
        put(&mut b, 336, 0x11c0);
        put(&mut b, 340, 0x11e0);
        put(&mut b, 400, 0x11c0);
        put(&mut b, 404, 0x1240);
        b[448..453].copy_from_slice(b"Gate\0");
        put(&mut b, 480, 12);
        put(&mut b, 484, 1);
        put(&mut b, 488, 0x1200);
        put(&mut b, 492, 0x1220);
        b[512..521].copy_from_slice(b"isOnline\0");
        b[544..551].copy_from_slice(b"c8@0:4\0");
        put(&mut b, 576, 12);
        put(&mut b, 580, 1);
        put(&mut b, 584, 0x1260);
        put(&mut b, 588, 0x1280);
        b[608..614].copy_from_slice(b"token\0");
        b[640..647].copy_from_slice(b"@8@0:4\0");
        b
    }
    #[test]
    fn extracts_methods_and_encodings() {
        let classes = browse(&fixture()).unwrap();
        assert_eq!(classes[0].name, "Gate");
        assert_eq!(classes[0].methods[0].selector, "isOnline");
        assert!(classes[0].methods[0].accepts("false"));
        assert_eq!(classes[0].methods.len(), 2);
        assert!(classes[0].methods[1].class_method);
        assert!(classes[0].methods[1].accepts("nil"));
        assert!(!classes[0].methods[0].accepts("nil"));
    }
    #[test]
    fn universal_slice_and_encryption() {
        let bytes = fixture();
        let mut fat = vec![0; 48];
        fat[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
        fat[4..8].copy_from_slice(&2u32.to_be_bytes());
        fat[8..12].copy_from_slice(&0x0100000cu32.to_be_bytes());
        fat[28..32].copy_from_slice(&12u32.to_be_bytes());
        fat[32..36].copy_from_slice(&9u32.to_be_bytes());
        fat[36..40].copy_from_slice(&48u32.to_be_bytes());
        fat[40..44].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        fat.extend(bytes);
        assert_eq!(browse(&fat).unwrap()[0].name, "Gate");
        let mut encrypted = fixture();
        put(&mut encrypted, 16, 2);
        put(&mut encrypted, 20, 144);
        put(&mut encrypted, 152, 0x21);
        put(&mut encrypted, 156, 20);
        put(&mut encrypted, 168, 1);
        assert!(browse(&encrypted).unwrap_err().contains("encrypted"));
    }
    #[test]
    fn category_overrides_original_method() {
        let mut bytes = fixture();
        put(&mut bytes, 20, 192);
        put(&mut bytes, 32, 192);
        put(&mut bytes, 76, 2);
        let name = b"__objc_catlist";
        bytes[152..152 + name.len()].copy_from_slice(name);
        put(&mut bytes, 184, 0x1290);
        put(&mut bytes, 188, 4);
        put(&mut bytes, 656, 0x12a0);
        put(&mut bytes, 676, 0x1120);
        put(&mut bytes, 680, 0x12c0);
        put(&mut bytes, 704, 12);
        put(&mut bytes, 708, 1);
        put(&mut bytes, 712, 0x1200);
        put(&mut bytes, 716, 0x1300);
        bytes[768..775].copy_from_slice(b"v8@0:4\0");
        let classes = browse(&bytes).unwrap();
        assert_eq!(classes[0].methods.len(), 2);
        assert!(classes[0].methods[0].accepts("skip"));
        assert!(!classes[0].methods[0].accepts("false"));
    }
    #[test]
    fn malformed_inputs_do_not_panic() {
        let bytes = fixture();
        for length in 0..bytes.len() {
            assert!(browse(&bytes[..length]).is_err());
        }
        let mut bytes = fixture();
        put(&mut bytes, 304, 0xfffffff0);
        assert!(browse(&bytes).is_err());
    }
    #[test]
    fn inherited_methods_prefer_override_and_stop_cycles() {
        let method = MethodInfo {
            class_method: false,
            selector: "online".into(),
            encoding: "c".into(),
        };
        let classes = vec![
            ClassInfo {
                name: "A".into(),
                superclass: Some("B".into()),
                methods: vec![method.clone()],
            },
            ClassInfo {
                name: "B".into(),
                superclass: Some("A".into()),
                methods: vec![method],
            },
        ];
        assert_eq!(methods_for(&classes, 0).len(), 1);
    }
}
