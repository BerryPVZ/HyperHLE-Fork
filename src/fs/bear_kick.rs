/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Bear Kick 3.6.6 stores its preinstalled game under obfuscated filenames.
//! Its Lua plugin loader checks kickthebuddy/1.0.94/driver.data and vail.txt
//! before entering gameplay. Expose the manifest's real files to filesystem
//! checks as well as resource reads, rather than leaving the loader on its
//! misleading "Decompressing Resource" / reload screen.

use super::{FileLocation, FsNode};
use std::io::{Cursor, Read};

fn components(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == ".." || p.contains('\\')) {
        return None;
    }
    Some(parts)
}

fn find<'a>(mut node: &'a FsNode, parts: &[&str]) -> Option<&'a FsNode> {
    for part in parts {
        let FsNode::Directory { children, .. } = node else { return None; };
        node = children.get(*part)?;
    }
    Some(node)
}

fn read(node: &FsNode) -> Option<Vec<u8>> {
    let FsNode::File { location, .. } = node else { return None; };
    let reader: Box<dyn Read> = match location {
        FileLocation::Path(path) => Box::new(std::fs::File::open(path).ok()?),
        FileLocation::IpaFileRef(file) => Box::new(file.open()),
        FileLocation::ResourceFilePath(_) => return None,
    };
    let mut bytes = Vec::new();
    reader.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 8 * 1024 * 1024).then_some(bytes)
}

fn alias(node: &FsNode) -> Option<FsNode> {
    let FsNode::File { location, .. } = node else { return None; };
    let location = match location {
        FileLocation::Path(path) => FileLocation::Path(path.clone()),
        FileLocation::IpaFileRef(file) => FileLocation::IpaFileRef(file.clone()),
        FileLocation::ResourceFilePath(_) => return None,
    };
    Some(FsNode::File { location, writeable: false })
}

fn insert(node: &mut FsNode, parts: &[&str], file: FsNode) {
    let FsNode::Directory { children, .. } = node else { return; };
    if parts.len() == 1 {
        children.entry(parts[0].to_string()).or_insert(file);
    } else {
        let child = children.entry(parts[0].to_string()).or_insert_with(FsNode::dir);
        insert(child, &parts[1..], file);
    }
}

pub(super) fn expose_packaged_resources(root: &mut FsNode) {
    let Some(info) = find(root, &["Info.plist"]).and_then(read) else { return; };
    let Ok(info) = plist::Value::from_reader(Cursor::new(info)) else { return; };
    let Some(info) = info.as_dictionary() else { return; };
    if info.get("CFBundleShortVersionString").and_then(plist::Value::as_string) != Some("3.6.6") {
        return;
    }
    let Some(mut data) = find(root, &["PrCoverCh"]).and_then(read) else { return; };
    // The shipped filename manifest uses this repeating 644-byte XOR mask.
    // Resource payloads stay untouched: the guest performs their decoding.
    for (i, byte) in data.iter_mut().enumerate() {
        *byte ^= match i % 644 {
            0..=72 => b'y',
            73..=310 => b'd',
            311..=393 => b'u',
            394..=475 => b'B',
            476..=554 => b'e',
            _ => b'h',
        };
    }
    let Ok(manifest) = plist::Value::from_reader(Cursor::new(data)) else { return; };
    let Some(manifest) = manifest.as_dictionary() else { return; };
    if manifest.is_empty() || manifest.len() > 20000
        || !manifest.contains_key("kickthebuddy/1.0.94/driver.data")
        || !manifest.contains_key("kickthebuddy/1.0.94/vail.txt") {
        return;
    }
    // Validate the entire manifest before adding anything. Never synthesize a
    // completion marker or claim an absent game package has been installed.
    let mut aliases = Vec::new();
    for (name, target) in manifest {
        let Some(parts) = components(name) else { return; };
        let Some(target) = target.as_string().and_then(components) else { return; };
        let Some(file) = find(root, &target).and_then(alias) else {
            log!("Bear Kick resource manifest has a missing target for {:?}", name);
            return;
        };
        for end in 1..parts.len() {
            if manifest.contains_key(&parts[..end].join("/"))
                || matches!(find(root, &parts[..end]), Some(FsNode::File { .. })) {
                return;
            }
        }
        aliases.push((parts, file));
    }
    let count = aliases.len();
    for (parts, file) in aliases {
        insert(root, &parts, file);
    }
    log!("Bear Kick: exposed {} packaged resource paths from its manifest", count);
}
