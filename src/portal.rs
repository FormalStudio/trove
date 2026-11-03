//! `trove --portal`: the D-Bus service that makes Trove the desktop's file chooser
//! (`org.freedesktop.impl.portal.FileChooser`, behind xdg-desktop-portal) and its file manager
//! (`org.freedesktop.FileManager1`, "Show in folder"). It draws nothing: each pick runs a
//! `trove --pick` process and reads its outcome over a pipe, and each request to show files is
//! a `trove` window of its own (`trove --select <item>...`, `--properties <item>...`, `trove
//! <folder>...`), so a crash in a window never takes the service down. D-Bus starts it on the
//! first call and it exits once idle.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_lite::{AsyncWriteExt, future};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::object_server::ObjectServer;
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type};
use zbus::{connection, interface};

use crate::picker::{ChoiceBox, Filter, Mode, Outcome, Request};

pub const PORTAL_NAME: &str = "org.freedesktop.impl.portal.desktop.trove";
pub const FILE_MANAGER_NAME: &str = "org.freedesktop.FileManager1";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";

/// How long the service waits with nothing open before it exits; D-Bus starts it again.
const IDLE: Duration = Duration::from_secs(300);

/// The portal's responses.
const SUCCESS: u32 = 0;
const CANCELLED: u32 = 1;
const OTHER: u32 = 2;

/// A filter on the wire: its name and its `(kind, pattern)` pairs.
type WireFilter = (String, Vec<(u32, String)>);
/// A choice box on the wire: id, label, its `(id, label)` options and the initial one.
type WireChoice = (String, String, Vec<(String, String)>, String);

/// The options of OpenFile, SaveFile and SaveFiles; each method reads its own.
#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "a{sv}")]
pub struct Options {
    pub accept_label: Option<String>,
    pub modal: Option<bool>,
    pub multiple: Option<bool>,
    pub directory: Option<bool>,
    pub filters: Option<Vec<WireFilter>>,
    pub current_filter: Option<WireFilter>,
    pub choices: Option<Vec<WireChoice>>,
    pub current_name: Option<String>,
    pub current_folder: Option<Vec<u8>>,
    pub current_file: Option<Vec<u8>>,
    pub files: Option<Vec<Vec<u8>>>,
}

/// What a pick answers.
#[derive(SerializeDict, DeserializeDict, Type, Debug, Default, PartialEq)]
#[zvariant(signature = "a{sv}")]
pub struct Results {
    pub uris: Option<Vec<String>>,
    pub choices: Option<Vec<(String, String)>>,
    pub current_filter: Option<WireFilter>,
}

impl Options {
    fn request(self, mode: Mode, title: String, parent_window: String) -> Request {
        let filter = |(name, patterns): WireFilter| Filter { name, patterns };
        let choice = |(id, label, options, initial): WireChoice| ChoiceBox { id, label, options, initial };
        Request {
            mode,
            title,
            parent_window,
            // The portal's default: the caller's window waits on the pick.
            modal: self.modal.unwrap_or(true),
            choices: self.choices.unwrap_or_default().into_iter().map(choice).collect(),
            accept_label: self.accept_label,
            multiple: mode == Mode::Open && self.multiple.unwrap_or(false),
            directory: mode == Mode::Open && self.directory.unwrap_or(false),
            filters: self.filters.unwrap_or_default().into_iter().map(filter).collect(),
            current_filter: self.current_filter.map(filter),
            current_name: self.current_name,
            current_folder: self.current_folder,
            current_file: self.current_file,
            files: self.files.unwrap_or_default(),
        }
    }
}

/// Whether the picker for `parent_window` runs on the X server `display` names: an X11 window
/// can only parent a window on its own server, so under Wayland the picker for an XWayland app
/// runs on XWayland too.
fn on_x_server(parent_window: &str, display: Option<std::ffi::OsString>) -> bool {
    parent_window.starts_with("x11:") && display.is_some_and(|d| !d.is_empty())
}

/// What runs a pick: `program --pick`, the request on its stdin and the outcome on its stdout.
/// Showing a folder runs `program <path>`.
#[derive(Clone)]
struct Launcher {
    program: PathBuf,
    busy: Arc<AtomicUsize>,
    touched: flume::Sender<()>,
}

impl Launcher {
    /// Counts a request as open until the guard drops, so the service never exits under one.
    fn hold(&self) -> Busy {
        self.busy.fetch_add(1, Ordering::SeqCst);
        let _ = self.touched.try_send(());
        Busy(self.clone())
    }

    async fn pick(&self, request: &Request, closed: flume::Receiver<()>) -> (u32, Option<Outcome>) {
        let Ok(json) = serde_json::to_vec(request) else { return (OTHER, None) };
        let mut cmd = async_process::Command::new(&self.program);
        cmd.arg("--pick").stdin(Stdio::piped()).stdout(Stdio::piped()).kill_on_drop(true);
        if on_x_server(&request.parent_window, std::env::var_os("DISPLAY")) {
            cmd.env_remove("WAYLAND_DISPLAY");
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                eprintln!("trove: starting the picker {}: {e}", self.program.display());
                return (OTHER, None);
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let wrote = stdin.write_all(&json).await;
            drop(stdin);
            if let Err(e) = wrote {
                eprintln!("trove: handing the picker its request: {e}");
                return (OTHER, None);
            }
        }
        // Close wins the race by dropping the output future, which kills the picker.
        let ran = future::or(async { Some(child.output().await) }, async {
            let _ = closed.recv_async().await;
            None
        })
        .await;
        let output = match ran {
            None => return (OTHER, None),
            Some(Err(e)) => {
                eprintln!("trove: waiting for the picker: {e}");
                return (OTHER, None);
            }
            Some(Ok(output)) => output,
        };
        let outcome = String::from_utf8_lossy(&output.stdout)
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<Outcome>(line).ok());
        match outcome {
            Some(outcome @ Outcome::Picked { .. }) => (SUCCESS, Some(outcome)),
            Some(Outcome::Cancelled) => (CANCELLED, None),
            // The window was closed without an answer.
            None if output.status.success() => (CANCELLED, None),
            None => (OTHER, None),
        }
    }

    /// A Trove window with `args` (`--select` and items, or folders), handed the caller's
    /// activation token so the compositor lets it take focus.
    fn show(&self, args: &[&std::ffi::OsStr], startup_id: &str) {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(args).stdin(Stdio::null());
        if !startup_id.is_empty() {
            cmd.env("XDG_ACTIVATION_TOKEN", startup_id).env("DESKTOP_STARTUP_ID", startup_id);
        }
        match cmd.spawn() {
            // Reaped on a thread of its own: the window lives as long as the user keeps it.
            Ok(mut child) => {
                let _ = std::thread::Builder::new().name("trove-window".into()).spawn(move || child.wait());
            }
            Err(e) => eprintln!("trove: opening a window: {e}"),
        }
    }

    /// A window over the local ones of `uris` behind `flag` (`--select` or `--properties`);
    /// none when no item is local.
    fn show_items(&self, flag: &str, uris: &[String], startup_id: &str) {
        let _busy = self.hold();
        let items = paths(uris);
        if !items.is_empty() {
            let args = std::iter::once(flag.as_ref()).chain(items.iter().map(|p| p.as_os_str()));
            self.show(&args.collect::<Vec<_>>(), startup_id);
        }
    }
}

struct Busy(Launcher);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
        let _ = self.0.touched.try_send(());
    }
}

/// The request object xdg-desktop-portal holds for one pick; its `Close` ends the pick.
struct PickRequest {
    close: flume::Sender<()>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl PickRequest {
    fn close(&self) {
        let _ = self.close.try_send(());
    }
}

struct FileChooser(Launcher);

impl FileChooser {
    async fn run(
        &self,
        server: &ObjectServer,
        handle: OwnedObjectPath,
        mode: Mode,
        title: String,
        parent_window: String,
        options: Options,
    ) -> (u32, Results) {
        let _busy = self.0.hold();
        let request = options.request(mode, title, parent_window);
        let (close, closed) = flume::bounded(1);
        if let Err(e) = server.at(&handle, PickRequest { close }).await {
            eprintln!("trove: exporting the request {}: {e}", handle.as_str());
            return (OTHER, Results::default());
        }
        let (response, outcome) = self.0.pick(&request, closed).await;
        let _ = server.remove::<PickRequest, _>(&handle).await;
        let mut results = Results::default();
        if let Some(Outcome::Picked { uris, filter, choices }) = outcome {
            results.uris = Some(uris);
            results.current_filter = filter.map(|f| (f.name, f.patterns));
            results.choices = (!request.choices.is_empty()).then_some(choices);
        }
        (response, results)
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[zbus(out_args("response", "results"))]
    async fn open_file(
        &self,
        handle: OwnedObjectPath,
        _app_id: String,
        parent_window: String,
        title: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        self.run(server, handle, Mode::Open, title, parent_window, options).await
    }

    #[zbus(out_args("response", "results"))]
    async fn save_file(
        &self,
        handle: OwnedObjectPath,
        _app_id: String,
        parent_window: String,
        title: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        self.run(server, handle, Mode::Save, title, parent_window, options).await
    }

    #[zbus(out_args("response", "results"))]
    async fn save_files(
        &self,
        handle: OwnedObjectPath,
        _app_id: String,
        parent_window: String,
        title: String,
        options: Options,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        self.run(server, handle, Mode::SaveFiles, title, parent_window, options).await
    }
}

struct FileManager(Launcher);

/// The local paths of `uris`; ones that are not `file://` are skipped.
fn paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter().filter_map(|u| trove_fs::uri_path(u)).collect()
}

/// One window for a whole request: a tab per folder, each with its items selected.
#[interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_folders(&self, uris: Vec<String>, startup_id: String) {
        let _busy = self.0.hold();
        let folders = paths(&uris);
        if !folders.is_empty() {
            self.0.show(&folders.iter().map(|p| p.as_os_str()).collect::<Vec<_>>(), &startup_id);
        }
    }

    fn show_items(&self, uris: Vec<String>, startup_id: String) {
        self.0.show_items("--select", &uris, &startup_id);
    }

    /// The items shown as `ShowItems` does, with the Properties sheet over them.
    fn show_item_properties(&self, uris: Vec<String>, startup_id: String) {
        self.0.show_items("--properties", &uris, &startup_id);
    }
}

/// Serves both interfaces on `builder`'s bus and asks for both names. A name another process
/// holds (a running file manager owning FileManager1) is left to it. The connection serves
/// for as long as it is kept.
pub async fn serve(builder: connection::Builder<'_>, program: PathBuf) -> zbus::Result<Service> {
    let (touched, idle) = flume::unbounded();
    let launcher = Launcher { program, busy: Arc::new(AtomicUsize::new(0)), touched };
    let conn = builder
        .serve_at(PORTAL_PATH, FileChooser(launcher.clone()))?
        .serve_at(FILE_MANAGER_PATH, FileManager(launcher.clone()))?
        .build()
        .await?;
    let mut owned = 0;
    for name in [PORTAL_NAME, FILE_MANAGER_NAME] {
        match conn.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into()).await {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => owned += 1,
            Ok(_) => eprintln!("trove: {name} has another owner"),
            Err(e) => eprintln!("trove: asking for {name}: {e}"),
        }
    }
    if owned == 0 {
        return Err(zbus::Error::NameTaken);
    }
    Ok(Service { conn, busy: launcher.busy, idle })
}

pub struct Service {
    pub conn: zbus::Connection,
    busy: Arc<AtomicUsize>,
    idle: flume::Receiver<()>,
}

impl Service {
    /// Returns once `idle` has passed with no request open and none arriving.
    pub async fn until_idle(&self, idle: Duration) {
        loop {
            let touched = future::or(async { self.idle.recv_async().await.is_ok() }, async {
                async_io::Timer::after(idle).await;
                false
            })
            .await;
            if !touched && self.busy.load(Ordering::SeqCst) == 0 {
                return;
            }
        }
    }
}

/// `trove --portal` on the session bus, picking with this executable, until idle.
pub fn run() -> zbus::Result<()> {
    let program = std::env::current_exe().map_err(|e| zbus::Error::Failure(e.to_string()))?;
    async_io::block_on(async {
        let service = serve(connection::Builder::session()?, program).await?;
        service.until_idle(IDLE).await;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_x11_parent_puts_the_picker_on_its_x_server() {
        let display = || Some(":1".into());
        assert!(super::on_x_server("x11:3a00004", display()));
        assert!(!super::on_x_server("x11:3a00004", None), "no X server to go to");
        assert!(!super::on_x_server("wayland:5f1e", display()));
        assert!(!super::on_x_server("", display()));
    }
}
