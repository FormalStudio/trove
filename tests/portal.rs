//! The portal and FileManager1 services on a private bus: a `dbus-daemon` of the test's own,
//! never the session's, with a shell script standing in for `trove --pick` and `trove <path>`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_lite::future;
use trove::picker::{Mode, Request};
use trove::portal::{FILE_MANAGER_NAME, FILE_MANAGER_PATH, PORTAL_NAME, PORTAL_PATH, Results, Service};
use zbus::zvariant::{ObjectPath, Value};

const CHOOSER: &str = "org.freedesktop.impl.portal.FileChooser";

/// A bus daemon for one test, stopped by its pid when the test ends.
struct Bus {
    daemon: Child,
    address: String,
}

impl Bus {
    fn start() -> Self {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon runs (dbus-1 package)");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().expect("its stdout")).read_line(&mut address).expect("an address");
        Self { daemon, address: address.trim().to_owned() }
    }

    fn connect(&self) -> zbus::connection::Builder<'static> {
        zbus::connection::Builder::address(self.address.as_str()).expect("a bus address")
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// A stand-in for the trove executable: `body` runs with `$DIR` set to `dir`.
fn stub(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("trove-stub");
    std::fs::write(&path, format!("#!/bin/sh\nDIR='{}'\n{body}\n", dir.display())).expect("write the stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make it runnable");
    path
}

fn serve(bus: &Bus, program: PathBuf) -> Service {
    async_io::block_on(trove::portal::serve(bus.connect(), program)).expect("the service starts")
}

fn client(bus: &Bus) -> zbus::Connection {
    async_io::block_on(bus.connect().build()).expect("a client connection")
}

/// Waits up to five seconds for `ready`.
async fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out");
        async_io::Timer::after(Duration::from_millis(20)).await;
    }
}

async fn call(
    conn: &zbus::Connection,
    method: &str,
    handle: &str,
    options: HashMap<&str, Value<'_>>,
) -> zbus::Result<(u32, Results)> {
    let handle = ObjectPath::try_from(handle)?;
    let reply = conn
        .call_method(
            Some(PORTAL_NAME),
            PORTAL_PATH,
            Some(CHOOSER),
            method,
            &(handle, "org.example.App", "", "Pick", options),
        )
        .await?;
    reply.body().deserialize::<(u32, Results)>()
}

const PICKED: &str =
    r#"{"outcome":"picked","uris":["file:///tmp/a%20b.txt"],"filter":{"name":"Text","patterns":[[0,"*.txt"]]}}"#;

#[test]
fn open_file_hands_the_request_over_and_answers_the_pick() {
    let bus = Bus::start();
    let dir = tempfile::tempdir().expect("a temp dir");
    let program = stub(dir.path(), &format!("cat > \"$DIR/request.json\"\necho '{PICKED}'"));
    let _service = serve(&bus, program);
    let conn = client(&bus);

    let filters = vec![("Text", vec![(0u32, "*.txt")]), ("Images", vec![(1u32, "image/*")])];
    let choices = vec![("encoding", "Encoding", vec![("utf8", "UTF-8")], "utf8")];
    let options = HashMap::from([
        ("multiple", Value::from(true)),
        ("accept_label", Value::from("_Upload")),
        ("filters", Value::from(filters)),
        ("choices", Value::from(choices)),
        ("current_folder", Value::from(b"/tmp\0".as_slice())),
    ]);
    let (response, results) =
        async_io::block_on(call(&conn, "OpenFile", "/org/freedesktop/portal/desktop/request/1_1/t1", options))
            .expect("OpenFile answers");
    assert_eq!(response, 0);
    assert_eq!(results.uris, Some(vec!["file:///tmp/a%20b.txt".to_owned()]));
    assert_eq!(results.current_filter, Some(("Text".to_owned(), vec![(0, "*.txt".to_owned())])));
    assert_eq!(results.choices, Some(vec![("encoding".to_owned(), "utf8".to_owned())]), "choices come back as set");

    let sent: Request =
        serde_json::from_slice(&std::fs::read(dir.path().join("request.json")).expect("the stub got a request"))
            .expect("a request");
    assert_eq!(sent.mode, Mode::Open);
    assert!(sent.multiple && !sent.directory);
    assert_eq!(sent.accept_label(), "Upload");
    assert_eq!(sent.filters.len(), 2);
    assert_eq!(sent.current_folder.as_deref(), Some(b"/tmp\0".as_slice()));
}

#[test]
fn save_file_answers_cancelled_and_failed_picks() {
    let bus = Bus::start();
    let dir = tempfile::tempdir().expect("a temp dir");
    let _service = serve(&bus, stub(dir.path(), "cat > \"$DIR/request.json\"\necho '{\"outcome\":\"cancelled\"}'"));
    let conn = client(&bus);
    let options = HashMap::from([("current_name", Value::from("notes.txt"))]);
    let (response, results) =
        async_io::block_on(call(&conn, "SaveFile", "/org/freedesktop/portal/desktop/request/1_1/t2", options))
            .expect("SaveFile answers");
    assert_eq!((response, results.uris), (1, None));
    let sent: Request =
        serde_json::from_slice(&std::fs::read(dir.path().join("request.json")).expect("a request")).expect("a request");
    assert_eq!((sent.mode, sent.current_name.as_deref()), (Mode::Save, Some("notes.txt")));

    let bus = Bus::start();
    let _service = serve(&bus, stub(dir.path(), "cat > /dev/null\nexit 3"));
    let conn = client(&bus);
    let (response, _) =
        async_io::block_on(call(&conn, "SaveFiles", "/org/freedesktop/portal/desktop/request/1_1/t3", HashMap::new()))
            .expect("SaveFiles answers");
    assert_eq!(response, 2, "a picker that dies answers other");
}

#[test]
fn close_on_the_request_ends_the_picker() {
    let bus = Bus::start();
    let dir = tempfile::tempdir().expect("a temp dir");
    let program = stub(dir.path(), "cat > /dev/null\necho $$ > \"$DIR/pid\"\nexec sleep 30");
    let _service = serve(&bus, program);
    let conn = client(&bus);
    let handle = "/org/freedesktop/portal/desktop/request/1_1/t4";
    let pid_file = dir.path().join("pid");
    let started = Instant::now();
    let (answer, closed) = async_io::block_on(future::zip(call(&conn, "OpenFile", handle, HashMap::new()), async {
        until(|| std::fs::read_to_string(&pid_file).is_ok_and(|p| p.ends_with('\n'))).await;
        conn.call_method(Some(PORTAL_NAME), handle, Some("org.freedesktop.impl.portal.Request"), "Close", &())
            .await
            .map(|_| ())
    }));
    closed.expect("Close is answered");
    let (response, results) = answer.expect("OpenFile answers");
    assert_eq!((response, results.uris), (2, None));
    assert!(started.elapsed() < Duration::from_secs(20), "the picker did not run out its sleep");
    let pid = std::fs::read_to_string(&pid_file).expect("its pid");
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
    async_io::block_on(until(|| !proc.exists()));
    let gone = conn.call_method(Some(PORTAL_NAME), handle, Some("org.freedesktop.impl.portal.Request"), "Close", &());
    assert!(async_io::block_on(gone).is_err(), "the request object goes with the pick");
}

#[test]
fn file_manager_opens_one_window_per_folder() {
    let bus = Bus::start();
    let dir = tempfile::tempdir().expect("a temp dir");
    let _service = serve(&bus, stub(dir.path(), "echo \"$1 $XDG_ACTIVATION_TOKEN\" >> \"$DIR/shown\""));
    let conn = client(&bus);
    let call = |method: &str, uris: &[&str]| {
        let uris: Vec<&str> = uris.to_vec();
        async_io::block_on(conn.call_method(
            Some(FILE_MANAGER_NAME),
            FILE_MANAGER_PATH,
            Some("org.freedesktop.FileManager1"),
            method,
            &(uris, "token-1"),
        ))
        .expect("answered");
    };
    call(
        "ShowItems",
        &["file:///srv/a%20b/one.txt", "file:///srv/a%20b/two.txt", "https://example.com/x", "file:///opt/three"],
    );
    call("ShowFolders", &["file:///home"]);
    let shown = dir.path().join("shown");
    let lines = || std::fs::read_to_string(&shown).unwrap_or_default().lines().map(str::to_owned).collect::<Vec<_>>();
    async_io::block_on(until(|| lines().len() >= 3));
    let mut got = lines();
    got.sort();
    assert_eq!(got, ["/home token-1", "/opt/three token-1", "/srv/a b/one.txt token-1"]);

    assert_eq!(
        trove::portal::item_windows(&["file:///x/1".into(), "file:///x/2".into(), "file:///y/3".into()]),
        [PathBuf::from("/x/1"), PathBuf::from("/y/3")]
    );
}
