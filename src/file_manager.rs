//! `org.freedesktop.FileManager1` ("Show in folder") served by the app itself: a running Trove
//! takes each call in-process and opens a window for it. With none running, D-Bus starts `trove
//! --file-manager`, the app with no window of its own, which quits once the windows it opened
//! are closed. The windows live in the process that owns the name, so a service going idle
//! never takes them down with its unit.

use std::path::PathBuf;
use std::time::Duration;

use nd::prelude::*;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::{connection, interface};

use crate::root::Start;

pub const NAME: &str = "org.freedesktop.FileManager1";
pub const PATH: &str = "/org/freedesktop/FileManager1";

/// How long `--file-manager` waits for a window before it quits. D-Bus delivers the call that
/// started it right after the name is taken; this only ends a start that never gets one.
const FIRST_CALL: Duration = Duration::from_secs(30);

/// One call, its `file://` URIs as local paths; any other scheme is left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Show {
    Folders(Vec<PathBuf>),
    Items(Vec<PathBuf>),
    Properties(Vec<PathBuf>),
}

impl Show {
    /// Where its window starts, and the items to open the Properties sheet on over it.
    pub fn start(self) -> (Start, Vec<PathBuf>) {
        match self {
            Show::Folders(paths) => (crate::start_for(false, paths), Vec::new()),
            Show::Items(paths) => (crate::start_for(true, paths), Vec::new()),
            Show::Properties(paths) => (crate::start_for(true, paths.clone()), paths),
        }
    }
}

struct FileManager(flume::Sender<Show>);

impl FileManager {
    /// A request with no local item opens nothing.
    fn send(&self, uris: &[String], show: fn(Vec<PathBuf>) -> Show) {
        let paths: Vec<PathBuf> = uris.iter().filter_map(|u| trove_fs::uri_path(u)).collect();
        if !paths.is_empty() {
            let _ = self.0.send(show(paths));
        }
    }
}

/// One window per call: a tab per folder, each with its items selected.
#[interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_folders(&self, uris: Vec<String>, _startup_id: String) {
        self.send(&uris, Show::Folders);
    }

    fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        self.send(&uris, Show::Items);
    }

    /// The items shown as `ShowItems` does, with the Properties sheet over them.
    fn show_item_properties(&self, uris: Vec<String>, _startup_id: String) {
        self.send(&uris, Show::Properties);
    }
}

/// Serves the interface on `builder`'s bus, each call a [`Show`] on `shows`, and asks for the
/// name. The connection serves for as long as it is kept. `NameTaken` when another process (a
/// running Trove, another file manager) holds it.
pub async fn serve(builder: connection::Builder<'_>, shows: flume::Sender<Show>) -> zbus::Result<zbus::Connection> {
    let conn = builder.serve_at(PATH, FileManager(shows))?.build().await?;
    match conn.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await? {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(conn),
        _ => Err(zbus::Error::NameTaken),
    }
}

struct Started;

struct Served {
    _conn: zbus::Connection,
}

/// Takes the name on the session bus for this app. `service` is `trove --file-manager`: it has
/// no window until a call opens one, so it quits when it cannot serve or no call comes.
pub fn start(cx: &mut App, service: bool) {
    if cx.has_global::<Started>() {
        return;
    }
    cx.set_global(Started);
    let (shows, calls) = flume::unbounded();
    nd::system::spawn(
        async move {
            let builder = connection::Builder::session()?;
            serve(builder, shows).await
        },
        move |served, cx| match served {
            Ok(conn) => cx.set_global(Served { _conn: conn }),
            Err(zbus::Error::NameTaken) if !service => {}
            Err(e) => {
                eprintln!("trove: serving {NAME}: {e}");
                if service {
                    cx.quit();
                }
            }
        },
    );
    next(calls);
    if service {
        nd::system::spawn(async_io::Timer::after(FIRST_CALL), |_, cx| {
            if cx.window_ids().next().is_none() {
                cx.quit();
            }
        });
    }
}

fn next(calls: flume::Receiver<Show>) {
    nd::system::spawn(async move { calls.recv_async().await.ok().map(|show| (show, calls)) }, |call, cx| {
        let Some((show, calls)) = call else { return };
        let (start, properties) = show.start();
        let window = crate::open(cx, start);
        if !properties.is_empty() {
            crate::properties::open(cx, window.id(), properties);
        }
        next(calls);
    });
}
