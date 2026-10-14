//! Small (16 px) shell icons of files, as RGBA pixels (`SHGetFileInfoW` + `GetDIBits`).

/// RGBA pixels of a square icon.
#[derive(Clone, Debug, PartialEq)]
pub struct IconImage {
    pub size: usize,
    pub rgba: Vec<u8>,
}

#[cfg(windows)]
pub fn file_icon(path: &str) -> Option<IconImage> {
    use std::ffi::c_void;
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP};
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    let w = super::winutil::wide(path);
    let mut info = SHFILEINFOW::default();
    // SAFETY: valid path and output struct of the size passed.
    let r = unsafe { SHGetFileInfoW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_SMALLICON) };
    if r == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let hicon = info.hIcon;
    // SAFETY: GDI objects created here are released before returning.
    unsafe {
        let mut ii = ICONINFO::default();
        if GetIconInfo(hicon, &mut ii).is_err() {
            let _ = DestroyIcon(hicon);
            return None;
        }
        let read = |bmp: HBITMAP| -> Option<(usize, usize, Vec<u8>)> {
            if bmp.is_invalid() {
                return None;
            }
            let mut bm = BITMAP::default();
            if GetObjectW(bmp, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void)) == 0 {
                return None;
            }
            let (bw, bh) = (bm.bmWidth.max(0) as usize, bm.bmHeight.max(0) as usize);
            if bw == 0 || bh == 0 || bw > 256 || bh > 512 {
                return None;
            }
            let dc = CreateCompatibleDC(None);
            let mut bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bw as i32,
                    biHeight: -(bh as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut px = vec![0u8; bw * bh * 4];
            let lines = GetDIBits(dc, bmp, 0, bh as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
            let _ = DeleteDC(dc);
            (lines > 0).then_some((bw, bh, px))
        };
        let color = read(ii.hbmColor);
        let mask = read(ii.hbmMask);
        let _ = DeleteObject(ii.hbmColor);
        let _ = DeleteObject(ii.hbmMask);
        let _ = DestroyIcon(hicon);
        let (bw, bh, mut px) = color?;
        let has_alpha = px.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        if !has_alpha {
            // Old-style icon: transparency comes from the AND mask (black = opaque).
            if let Some((mw, _, m)) = mask {
                for y in 0..bh {
                    for x in 0..bw {
                        let mi = (y * mw + x) * 4;
                        let opaque = m.get(mi).map(|v| *v == 0).unwrap_or(true);
                        px[(y * bw + x) * 4 + 3] = if opaque { 255 } else { 0 };
                    }
                }
            } else {
                for p in px.as_chunks_mut::<4>().0 {
                    p[3] = 255;
                }
            }
        }
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2); // BGRA -> RGBA
        }
        // Shell icons are square; anything else is not an icon we can draw as one.
        (bw == bh).then_some(IconImage { size: bw, rgba: px })
    }
}

#[cfg(not(windows))]
pub fn file_icon(_path: &str) -> Option<IconImage> {
    None
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn explorer_has_an_icon() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let icon = super::file_icon(&format!(r"{root}\explorer.exe")).expect("icon");
        assert!(icon.size >= 16);
        assert_eq!(icon.rgba.len(), icon.size * icon.size * 4);
        assert!(icon.rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 0));
    }
}
