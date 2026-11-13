//! The Windows clipboard. Files go on it as `CF_HDROP`, which Explorer reads, with a
//! "Preferred DropEffect" of move for a cut.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, POINT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY, DROPEFFECT_MOVE};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

use super::{Backend, GNOME, gnome_payload, parse_gnome};

const TEXT: &str = "text/plain";

pub(super) struct System;

fn drop_effect_format() -> u32 {
    let name: Vec<u16> = "Preferred DropEffect".encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated name.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// The clipboard, open until dropped.
struct Open;

impl Open {
    fn new() -> Option<Self> {
        // Another app may hold the clipboard for a moment.
        for _ in 0..10 {
            // SAFETY: no owner window is needed to read or replace the contents.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this process opened the clipboard.
        unsafe { CloseClipboard() };
    }
}

/// Moveable global memory holding `bytes`, which the clipboard owns once it is set.
fn global(bytes: &[u8]) -> Option<HANDLE> {
    // SAFETY: the block is `bytes.len()` long and locked while it is written.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if h.is_null() {
            return None;
        }
        let p = GlobalLock(h);
        if p.is_null() {
            GlobalFree(h);
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        GlobalUnlock(h);
        Some(h)
    }
}

fn set(format: u32, bytes: &[u8]) -> bool {
    let Some(h) = global(bytes) else { return false };
    // SAFETY: the clipboard is open and takes ownership of `h` on success.
    if unsafe { SetClipboardData(format, h) }.is_null() {
        // SAFETY: still ours, as the clipboard refused it.
        unsafe { GlobalFree(h) };
        return false;
    }
    true
}

/// A copy of the global memory behind clipboard `format`.
fn get(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open; the block is locked while it is read and stays the
    // clipboard's.
    unsafe {
        let h = GetClipboardData(format);
        if h.is_null() {
            return None;
        }
        let p = GlobalLock(h);
        if p.is_null() {
            return None;
        }
        let out = std::slice::from_raw_parts(p.cast::<u8>(), GlobalSize(h)).to_vec();
        GlobalUnlock(h);
        Some(out)
    }
}

/// A DROPFILES header, then each path as NUL-terminated UTF-16, then one more NUL.
fn hdrop_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let header = std::mem::size_of::<DROPFILES>();
    let df = DROPFILES { pFiles: header as u32, pt: POINT { x: 0, y: 0 }, fNC: 0, fWide: 1 };
    // SAFETY: DROPFILES is plain data with no padding.
    let mut out = unsafe { std::slice::from_raw_parts((&raw const df).cast::<u8>(), header) }.to_vec();
    for p in paths {
        for unit in p.as_os_str().encode_wide().chain([0]) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn dropped_paths() -> Option<Vec<PathBuf>> {
    // SAFETY: the clipboard is open and the HDROP stays its own.
    unsafe {
        let h: HDROP = GetClipboardData(u32::from(CF_HDROP));
        if h.is_null() {
            return None;
        }
        let count = DragQueryFileW(h, u32::MAX, std::ptr::null_mut(), 0);
        let mut out = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32);
            buf.truncate(got as usize);
            out.push(PathBuf::from(OsString::from_wide(&buf)));
        }
        Some(out)
    }
}

fn available(format: u32) -> bool {
    // SAFETY: no preconditions.
    unsafe { IsClipboardFormatAvailable(format) != 0 }
}

impl Backend for System {
    fn offer(&self, mime: &str, data: &[u8]) -> bool {
        if mime != GNOME {
            return false;
        }
        let Some((paths, cut)) = parse_gnome(&String::from_utf8_lossy(data)) else { return false };
        let Some(_open) = Open::new() else { return false };
        // SAFETY: the clipboard is open.
        if unsafe { EmptyClipboard() } == 0 {
            return false;
        }
        let effect = if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
        set(u32::from(CF_HDROP), &hdrop_bytes(&paths)) && set(drop_effect_format(), &effect.to_le_bytes())
    }

    fn types(&self) -> Option<Vec<String>> {
        let mut out = Vec::new();
        if available(u32::from(CF_HDROP)) {
            out.push(GNOME.to_owned());
        }
        if available(u32::from(CF_UNICODETEXT)) {
            out.push(TEXT.to_owned());
        }
        Some(out)
    }

    fn read(&self, mime: &str) -> Option<Vec<u8>> {
        let _open = Open::new()?;
        match mime {
            GNOME => {
                let paths = dropped_paths().filter(|p| !p.is_empty())?;
                let cut = get(drop_effect_format())
                    .and_then(|b| b.get(..4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])))
                    .is_some_and(|e| e & DROPEFFECT_MOVE != 0);
                Some(gnome_payload(&paths, cut).into_bytes())
            }
            TEXT => {
                let bytes = get(u32::from(CF_UNICODETEXT))?;
                let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                Some(String::from_utf16_lossy(&units[..end]).into_bytes())
            }
            _ => None,
        }
    }
}
