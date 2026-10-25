//! Gives TMOP.exe its icon (Explorer, shortcuts) on MSVC targets: `assets/icon/tmop.ico` is
//! repacked into a compiled resource file (`.res`, RT_ICON images plus an RT_GROUP_ICON) and
//! handed to the linker, which accepts `.res` inputs directly. No resource compiler needed.

use std::path::PathBuf;

const ICO: &str = "assets/icon/tmop.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICO}");
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|o| o == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if !(windows && msvc) {
        return;
    }
    let ico = std::fs::read(ICO).expect("read the icon");
    let res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("tmop.res");
    std::fs::write(&res, icon_res(&ico)).expect("write the resource file");
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// One resource entry with an ordinal type and name, neutral language, data padded to 4 bytes.
fn entry(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(flags.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // language neutral
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// A `.res` holding every image of `ico` as RT_ICON 1..n and the group as RT_GROUP_ICON 1.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    assert!(ico.len() >= 6 && u16_at(ico, 0) == 0 && u16_at(ico, 2) == 1, "not an .ico");
    let count = u16_at(ico, 4) as usize;
    let mut out = Vec::new();
    entry(&mut out, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend([0, 0, 1, 0]);
    group.extend((count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + 16 * i;
        let (len, offset) = (u32_at(ico, e + 8) as usize, u32_at(ico, e + 12) as usize);
        let id = i as u16 + 1;
        entry(&mut out, RT_ICON, id, 0x1010, &ico[offset..offset + len]);
        group.extend(&ico[e..e + 12]); // size, colours, reserved, planes, bit count, bytes
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    out
}
