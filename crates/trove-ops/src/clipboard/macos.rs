//! The general pasteboard. Files go on it as file URLs, which Finder and every Cocoa app read;
//! a cut also marks the pasteboard with a private type, since macOS has no cut of its own.

use std::path::PathBuf;

use objc2::ClassType;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString, NSPasteboardURLReadingFileURLsOnlyKey,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

use super::{Backend, GNOME, gnome_payload, parse_gnome};

const TEXT: &str = "text/plain";
const CUT: &str = "com.formalstudio.trove.cut";

pub(super) struct System;

fn has(pb: &NSPasteboard, t: &NSString) -> bool {
    pb.types().is_some_and(|types| types.iter().any(|x| *x == *t))
}

impl Backend for System {
    fn offer(&self, mime: &str, data: &[u8]) -> bool {
        if mime != GNOME {
            return false;
        }
        let Some((paths, cut)) = parse_gnome(&String::from_utf8_lossy(data)) else { return false };
        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
            paths.iter().filter_map(NSURL::from_file_path).map(ProtocolObject::from_retained).collect();
        if urls.len() != paths.len() {
            return false;
        }
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if !pb.writeObjects(&NSArray::from_retained_slice(&urls)) {
            return false;
        }
        !cut || pb.setString_forType(&NSString::from_str("1"), &NSString::from_str(CUT))
    }

    fn types(&self) -> Option<Vec<String>> {
        let pb = NSPasteboard::generalPasteboard();
        let mut out = Vec::new();
        // SAFETY: the pasteboard type constants are immutable strings.
        if has(&pb, unsafe { NSPasteboardTypeFileURL }) {
            out.push(GNOME.to_owned());
        }
        // SAFETY: as above.
        if has(&pb, unsafe { NSPasteboardTypeString }) {
            out.push(TEXT.to_owned());
        }
        Some(out)
    }

    fn read(&self, mime: &str) -> Option<Vec<u8>> {
        let pb = NSPasteboard::generalPasteboard();
        match mime {
            GNOME => {
                let classes = NSArray::from_slice(&[NSURL::class()]);
                let yes = NSNumber::new_bool(true);
                // SAFETY: the option key is an immutable string constant.
                let key = unsafe { NSPasteboardURLReadingFileURLsOnlyKey };
                let value: &AnyObject = &yes;
                let options = NSDictionary::from_slices(&[key], &[value]);
                // SAFETY: the classes are NSURL, and the option takes an NSNumber boolean.
                let objects = unsafe { pb.readObjectsForClasses_options(&classes, Some(&options)) }?;
                let paths: Vec<_> = objects
                    .iter()
                    .filter_map(|o| o.downcast_ref::<NSURL>()?.path())
                    .map(|p| PathBuf::from(p.to_string()))
                    .collect();
                if paths.is_empty() {
                    return None;
                }
                let cut = has(&pb, &NSString::from_str(CUT));
                Some(gnome_payload(&paths, cut).into_bytes())
            }
            // SAFETY: the pasteboard type constant is an immutable string.
            TEXT => pb.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string().into_bytes()),
            _ => None,
        }
    }
}
