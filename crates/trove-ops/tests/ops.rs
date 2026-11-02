#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use trove_ops::{Config, JobId, Op, OpEvent, Ops, Resolution, ResolutionChoice, TrashConfig};

struct Rig {
    ops: Ops,
    rx: flume::Receiver<OpEvent>,
    work: TempDir,
    _data: TempDir,
    data_home: PathBuf,
}

#[derive(Debug)]
enum Done {
    Finished(Vec<PathBuf>),
    Failed(String),
    Cancelled,
}

impl Done {
    fn created(self) -> Vec<PathBuf> {
        match self {
            Done::Finished(c) => c,
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    fn error(self) -> String {
        match self {
            Done::Failed(e) => e,
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

fn rig_with(mounts: Option<Vec<PathBuf>>) -> Rig {
    let data = tempfile::tempdir().unwrap();
    let data_home = data.path().join("share");
    fs::create_dir_all(&data_home).unwrap();
    let uid = unsafe_uid();
    let trash = TrashConfig { data_home: data_home.clone(), uid, mounts: Some(mounts.unwrap_or_default()) };
    let (ops, rx) = Ops::with_config(Config { trash });
    Rig { ops, rx, work: tempfile::tempdir().unwrap(), _data: data, data_home }
}

fn unsafe_uid() -> u32 {
    fs::metadata("/proc/self").unwrap().uid()
}

fn rig() -> Rig {
    rig_with(None)
}

impl Rig {
    fn p(&self, rel: &str) -> PathBuf {
        self.work.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.p(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.p(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn wait(&self, id: JobId, answers: &mut dyn FnMut(&Path, &Path) -> ResolutionChoice) -> (Done, Vec<OpEvent>) {
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let ev = self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("timed out");
            let mine = match &ev {
                OpEvent::Started { job, .. }
                | OpEvent::Progress { job, .. }
                | OpEvent::Conflict { job, .. }
                | OpEvent::Finished { job, .. }
                | OpEvent::Failed { job, .. }
                | OpEvent::Cancelled { job } => *job == id,
            };
            if !mine {
                continue;
            }
            seen.push(ev.clone());
            match ev {
                OpEvent::Conflict { source, existing, .. } => self.ops.resolve(id, answers(&source, &existing)),
                OpEvent::Finished { created, .. } => return (Done::Finished(created), seen),
                OpEvent::Failed { error, .. } => return (Done::Failed(error), seen),
                OpEvent::Cancelled { .. } => return (Done::Cancelled, seen),
                _ => {}
            }
        }
    }

    fn run(&self, op: Op) -> Done {
        let id = self.ops.submit(op);
        self.wait(id, &mut |s, e| panic!("unexpected conflict {} vs {}", s.display(), e.display())).0
    }

    fn run_answering(&self, op: Op, choice: ResolutionChoice) -> (Done, usize) {
        let id = self.ops.submit(op);
        let mut asked = 0;
        let (done, _) = self.wait(id, &mut |_, _| {
            asked += 1;
            choice
        });
        (done, asked)
    }

    fn undo(&self) -> Done {
        let id = self.ops.undo().expect("something to undo");
        self.wait(id, &mut |_, _| panic!("conflict")).0
    }

    fn redo(&self) -> Done {
        let id = self.ops.redo().expect("something to redo");
        self.wait(id, &mut |_, _| panic!("conflict")).0
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn move_within_one_file_system_renames() {
    let r = rig();
    let a = r.write("a.txt", "hi");
    let before = fs::metadata(&a).unwrap().ino();
    let dest = r.dir("dest");
    let created = r.run(Op::Move { sources: vec![a.clone()], dest_dir: dest.clone() }).created();
    assert_eq!(created, vec![dest.join("a.txt")]);
    assert!(!a.exists());
    assert_eq!(fs::metadata(dest.join("a.txt")).unwrap().ino(), before);
}

#[test]
fn move_across_file_systems_copies_then_deletes() {
    let r = rig();
    let Ok(other) = tempfile::tempdir_in("/dev/shm") else { return };
    if fs::metadata(other.path()).unwrap().dev() == fs::metadata(r.work.path()).unwrap().dev() {
        return;
    }
    let tree = r.write("tree/sub/a.txt", "alpha");
    r.write("tree/b.txt", "beta");
    let created = r.run(Op::Move { sources: vec![r.p("tree")], dest_dir: other.path().to_path_buf() }).created();
    assert_eq!(created, vec![other.path().join("tree")]);
    assert!(!tree.exists() && !r.p("tree").exists());
    assert_eq!(read(&other.path().join("tree/sub/a.txt")), "alpha");
    assert_eq!(read(&other.path().join("tree/b.txt")), "beta");
}

#[test]
fn moving_a_folder_into_itself_fails() {
    let r = rig();
    r.write("f/inner/x", "");
    let err = r.run(Op::Move { sources: vec![r.p("f")], dest_dir: r.p("f/inner") }).error();
    assert!(err.contains("into itself"), "{err}");
    let err = r.run(Op::Move { sources: vec![r.p("f")], dest_dir: r.p("f") }).error();
    assert!(err.contains("into itself"), "{err}");
    let err = r.run(Op::Copy { sources: vec![r.p("f")], dest_dir: r.p("f/inner") }).error();
    assert!(err.contains("into itself"), "{err}");
    assert!(r.p("f/inner/x").exists());
}

#[test]
fn copy_into_the_same_folder_names_copies() {
    let r = rig();
    let a = r.write("a.txt", "one");
    let dir = r.work.path().to_path_buf();
    let c1 = r.run(Op::Copy { sources: vec![a.clone()], dest_dir: dir.clone() }).created();
    assert_eq!(c1, vec![r.p("a copy.txt")]);
    let c2 = r.run(Op::Copy { sources: vec![a.clone()], dest_dir: dir.clone() }).created();
    assert_eq!(c2, vec![r.p("a copy 2.txt")]);
    assert_eq!(read(&r.p("a copy 2.txt")), "one");
    let c3 = r.run(Op::Duplicate { paths: vec![a] }).created();
    assert_eq!(c3, vec![r.p("a copy 3.txt")]);
}

#[test]
fn move_onto_itself_is_a_no_op() {
    let r = rig();
    let a = r.write("a.txt", "one");
    let created = r.run(Op::Move { sources: vec![a.clone()], dest_dir: r.work.path().to_path_buf() }).created();
    assert_eq!(created, vec![a.clone()]);
    assert_eq!(read(&a), "one");
}

#[test]
fn every_resolution_on_a_file_conflict() {
    let r = rig();
    let src = r.write("src/a.txt", "new");
    let dest = r.write("dest/a.txt", "old");
    let op = Op::Copy { sources: vec![src.clone()], dest_dir: r.p("dest") };

    let (done, asked) = r.run_answering(op.clone(), ResolutionChoice::once(Resolution::Skip));
    assert!(done.created().is_empty());
    assert_eq!((asked, read(&dest).as_str()), (1, "old"));

    let (done, _) = r.run_answering(op.clone(), ResolutionChoice::once(Resolution::KeepBoth));
    assert_eq!(done.created(), vec![r.p("dest/a 2.txt")]);
    assert_eq!((read(&dest).as_str(), read(&r.p("dest/a 2.txt")).as_str()), ("old", "new"));

    let (done, _) = r.run_answering(op.clone(), ResolutionChoice::once(Resolution::Cancel));
    assert!(matches!(done, Done::Cancelled));

    let (done, _) = r.run_answering(op, ResolutionChoice::once(Resolution::Replace));
    assert_eq!(done.created(), vec![dest.clone()]);
    assert_eq!(read(&dest), "new");
    assert!(names(&r.p("dest")).iter().all(|n| !n.contains("trove-part")));
}

#[test]
fn apply_to_all_asks_once() {
    let r = rig();
    for n in ["a", "b", "c"] {
        r.write(&format!("src/{n}"), &format!("new-{n}"));
        r.write(&format!("dest/{n}"), "old");
    }
    let sources: Vec<PathBuf> = ["a", "b", "c"].iter().map(|n| r.p(&format!("src/{n}"))).collect();
    let (done, asked) =
        r.run_answering(Op::Copy { sources, dest_dir: r.p("dest") }, ResolutionChoice::all(Resolution::KeepBoth));
    assert_eq!(asked, 1);
    assert_eq!(done.created().len(), 3);
    assert_eq!(names(&r.p("dest")), ["a", "a 2", "b", "b 2", "c", "c 2"]);

    let sources: Vec<PathBuf> = ["a", "b", "c"].iter().map(|n| r.p(&format!("src/{n}"))).collect();
    let (_, asked) =
        r.run_answering(Op::Copy { sources, dest_dir: r.p("dest") }, ResolutionChoice::all(Resolution::Skip));
    assert_eq!(asked, 1);
    assert_eq!(read(&r.p("dest/b")), "old");
}

#[test]
fn replace_on_a_folder_merges() {
    let r = rig();
    r.write("src/box/x.txt", "new x");
    r.write("src/box/y.txt", "new y");
    r.write("src/box/deep/z.txt", "new z");
    r.write("dest/box/x.txt", "old x");
    r.write("dest/box/keep.txt", "keep");
    r.write("dest/box/deep/w.txt", "old w");
    let (done, asked) = r.run_answering(
        Op::Copy { sources: vec![r.p("src/box")], dest_dir: r.p("dest") },
        ResolutionChoice::once(Resolution::Replace),
    );
    assert_eq!(done.created(), vec![r.p("dest/box")]);
    assert_eq!(asked, 1);
    assert_eq!(read(&r.p("dest/box/x.txt")), "new x");
    assert_eq!(read(&r.p("dest/box/y.txt")), "new y");
    assert_eq!(read(&r.p("dest/box/keep.txt")), "keep");
    assert_eq!(read(&r.p("dest/box/deep/w.txt")), "old w");
    assert_eq!(read(&r.p("dest/box/deep/z.txt")), "new z");

    let (done, _) = r.run_answering(
        Op::Move { sources: vec![r.p("src/box")], dest_dir: r.p("dest") },
        ResolutionChoice::once(Resolution::Replace),
    );
    done.created();
    assert!(!r.p("src/box").exists());
    assert_eq!(read(&r.p("dest/box/keep.txt")), "keep");
}

#[test]
fn replacing_a_folder_with_a_file_is_refused() {
    let r = rig();
    r.write("src/n", "file");
    r.write("dest/n/inside", "x");
    let (done, _) = r.run_answering(
        Op::Copy { sources: vec![r.p("src/n")], dest_dir: r.p("dest") },
        ResolutionChoice::once(Resolution::Replace),
    );
    assert!(done.error().contains("Cannot replace folder"));
    assert_eq!(read(&r.p("dest/n/inside")), "x");
}

#[test]
fn cancel_while_waiting_on_a_conflict() {
    let r = rig();
    r.write("src/a", "new");
    r.write("dest/a", "old");
    let id = r.ops.submit(Op::Copy { sources: vec![r.p("src/a")], dest_dir: r.p("dest") });
    let (done, _) = r.wait(id, &mut |_, _| {
        r.ops.cancel(id);
        // Never answered: cancel alone has to release the job.
        ResolutionChoice::once(Resolution::Cancel)
    });
    assert!(matches!(done, Done::Cancelled));
    assert_eq!(read(&r.p("dest/a")), "old");
    // The queue still works afterwards.
    r.run(Op::NewFolder { parent: r.p("dest"), name: "x".into() }).created();
}

#[test]
fn jobs_run_in_submission_order() {
    let r = rig();
    let a = r.ops.submit(Op::NewFolder { parent: r.p(""), name: "one".into() });
    let b = r.ops.submit(Op::Rename { path: r.p("one"), new_name: "two".into() });
    let c = r.ops.submit(Op::NewFolder { parent: r.p(""), name: "one".into() });
    let mut finished = Vec::new();
    while finished.len() < 3 {
        if let OpEvent::Finished { job, .. } = r.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            finished.push(job);
        }
    }
    assert_eq!(finished, [a, b, c]);
    assert_eq!(names(r.work.path()), ["one", "two"]);
}

#[test]
fn progress_is_throttled_and_counts_everything() {
    let r = rig();
    for i in 0..1500 {
        r.write(&format!("many/f{i}"), "x");
    }
    let id = r.ops.submit(Op::Copy { sources: vec![r.p("many")], dest_dir: r.dir("out") });
    let start = Instant::now();
    let (done, events) = r.wait(id, &mut |_, _| panic!("conflict"));
    let secs = start.elapsed().as_secs_f64();
    done.created();
    let progress: Vec<_> = events.iter().filter(|e| matches!(e, OpEvent::Progress { .. })).collect();
    assert!(progress.len() as f64 <= secs * 20.0 + 6.0, "{} events in {secs}s", progress.len());
    let Some(OpEvent::Progress { done_files, total_files, done_bytes, total_bytes, .. }) = progress.last() else {
        panic!("no progress")
    };
    assert_eq!((*done_files, *total_files), (1501, 1501));
    assert_eq!((*done_bytes, *total_bytes), (1500, 1500));
}

#[test]
fn rename_rules() {
    let r = rig();
    let a = r.write("a.txt", "a");
    r.write("b.txt", "b");
    for bad in ["", "   ", "x/y", ".", ".."] {
        r.run(Op::Rename { path: a.clone(), new_name: bad.into() }).error();
    }
    let e = r.run(Op::Rename { path: a.clone(), new_name: "b.txt".into() }).error();
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(read(&a), "a");
    assert_eq!(r.run(Op::Rename { path: a.clone(), new_name: "a.txt".into() }).created(), vec![a.clone()]);
    assert_eq!(r.run(Op::Rename { path: a.clone(), new_name: "A.txt".into() }).created(), vec![r.p("A.txt")]);
    assert_eq!(names(r.work.path()), ["A.txt", "b.txt"]);
}

#[test]
fn new_folder_and_file_get_suffixes() {
    let r = rig();
    let parent = r.work.path().to_path_buf();
    let f1 = r.run(Op::NewFolder { parent: parent.clone(), name: "untitled folder".into() }).created();
    let f2 = r.run(Op::NewFolder { parent: parent.clone(), name: "untitled folder".into() }).created();
    assert_eq!((f1, f2), (vec![r.p("untitled folder")], vec![r.p("untitled folder 2")]));
    r.run(Op::NewFile { parent: parent.clone(), name: "note.txt".into() }).created();
    let n2 = r.run(Op::NewFile { parent, name: "note.txt".into() }).created();
    assert_eq!(n2, vec![r.p("note 2.txt")]);
    assert_eq!(fs::metadata(r.p("note.txt")).unwrap().len(), 0);
}

#[test]
fn undo_and_redo_rename() {
    let r = rig();
    let a = r.write("a.txt", "a");
    r.run(Op::Rename { path: a.clone(), new_name: "b.txt".into() }).created();
    assert_eq!(r.undo().created(), vec![a.clone()]);
    assert!(a.exists() && !r.p("b.txt").exists());
    r.redo().created();
    assert!(!a.exists() && r.p("b.txt").exists());
    assert_eq!(r.undo().created(), vec![a.clone()]);
    assert!(a.exists());
}

#[test]
fn undo_and_redo_move() {
    let r = rig();
    let a = r.write("a.txt", "a");
    let b = r.write("b.txt", "b");
    let dest = r.dir("dest");
    r.run(Op::Move { sources: vec![a.clone(), b.clone()], dest_dir: dest.clone() }).created();
    r.undo().created();
    assert!(a.exists() && b.exists() && names(&dest).is_empty());
    r.redo().created();
    assert_eq!(names(&dest), ["a.txt", "b.txt"]);
    assert!(!a.exists());
}

#[test]
fn undo_and_redo_copy_duplicate_and_new() {
    let r = rig();
    let a = r.write("a.txt", "a");
    let dest = r.dir("dest");
    r.run(Op::Copy { sources: vec![a.clone()], dest_dir: dest.clone() }).created();
    r.undo().created();
    assert!(names(&dest).is_empty() && a.exists());
    r.redo().created();
    assert_eq!(read(&dest.join("a.txt")), "a");

    r.run(Op::Duplicate { paths: vec![a.clone()] }).created();
    assert!(r.p("a copy.txt").exists());
    r.undo().created();
    assert!(!r.p("a copy.txt").exists());

    r.run(Op::NewFolder { parent: r.p(""), name: "nf".into() }).created();
    r.run(Op::NewFile { parent: r.p(""), name: "nfile".into() }).created();
    r.undo().created();
    assert!(!r.p("nfile").exists() && r.p("nf").exists());
    r.undo().created();
    assert!(!r.p("nf").exists());
    r.redo().created();
    assert!(r.p("nf").exists());
}

#[test]
fn undo_and_redo_trash() {
    let r = rig();
    let a = r.write("a.txt", "a");
    let d = r.write("d/inner.txt", "i");
    r.run(Op::Trash { paths: vec![a.clone(), r.p("d")] }).created();
    assert!(!a.exists() && !d.exists());
    assert_eq!(r.undo().created().len(), 2);
    assert_eq!((read(&a), read(&d).as_str()), ("a".to_string(), "i"));
    r.redo().created();
    assert!(!a.exists());
    r.undo().created();
    assert!(a.exists());
}

#[test]
fn delete_and_empty_trash_are_not_undoable_and_leave_the_journal_alone() {
    let r = rig();
    let a = r.write("a.txt", "a");
    r.run(Op::Rename { path: a, new_name: "b.txt".into() }).created();
    r.write("gone.txt", "g");
    r.run(Op::Delete { paths: vec![r.p("gone.txt")] }).created();
    assert!(!r.p("gone.txt").exists());
    r.run(Op::EmptyTrash).created();
    r.undo().created();
    assert!(r.p("a.txt").exists());
    assert!(r.ops.undo().is_none());
}

#[test]
fn a_new_job_clears_redo() {
    let r = rig();
    let a = r.write("a.txt", "a");
    r.run(Op::Rename { path: a, new_name: "b.txt".into() }).created();
    r.undo().created();
    r.run(Op::NewFolder { parent: r.p(""), name: "x".into() }).created();
    assert!(r.ops.redo().is_none());
}

#[test]
fn a_replacing_job_is_not_undoable() {
    let r = rig();
    r.write("src/a", "new");
    r.write("dest/a", "old");
    r.run_answering(
        Op::Copy { sources: vec![r.p("src/a")], dest_dir: r.p("dest") },
        ResolutionChoice::once(Resolution::Replace),
    )
    .0
    .created();
    assert!(r.ops.undo().is_none());
}

#[test]
fn undo_reaches_the_job_still_running() {
    let r = rig();
    let a = r.write("a.txt", "a");
    r.ops.submit(Op::Rename { path: a.clone(), new_name: "b.txt".into() });
    let id = r.ops.undo().expect("the rename counts as pending");
    r.wait(id, &mut |_, _| panic!()).0.created();
    assert!(a.exists());
}

#[test]
fn trash_writes_trashinfo_and_restore_puts_it_back() {
    let r = rig();
    let a = r.write("dir with space/é a.txt", "a");
    r.run(Op::Trash { paths: vec![a.clone()] }).created();
    assert!(!a.exists());
    let trash = r.data_home.join("Trash");
    assert_eq!(names(&trash.join("files")), ["é a.txt"]);
    let info = read(&trash.join("info/é a.txt.trashinfo"));
    assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
    assert!(info.contains("dir%20with%20space/%C3%A9%20a.txt"), "{info}");
    assert!(info.contains("DeletionDate="));

    let trashed = trash.join("files/é a.txt");
    assert_eq!(r.run(Op::Restore { trashed: vec![trashed.clone()] }).created(), vec![a.clone()]);
    assert_eq!(read(&a), "a");
    assert!(names(&trash.join("files")).is_empty() && names(&trash.join("info")).is_empty());
}

#[test]
fn restore_over_a_taken_name_keeps_both() {
    let r = rig();
    let a = r.write("a.txt", "old");
    r.run(Op::Trash { paths: vec![a.clone()] }).created();
    r.write("a.txt", "new");
    let trashed = r.data_home.join("Trash/files/a.txt");
    assert_eq!(r.run(Op::Restore { trashed: vec![trashed] }).created(), vec![r.p("a 2.txt")]);
    assert_eq!((read(&a).as_str(), read(&r.p("a 2.txt")).as_str()), ("new", "old"));
}

#[test]
fn trashing_same_names_twice_does_not_overwrite() {
    let r = rig();
    r.write("x/a.txt", "one");
    r.run(Op::Trash { paths: vec![r.p("x/a.txt")] }).created();
    r.write("x/a.txt", "two");
    r.run(Op::Trash { paths: vec![r.p("x/a.txt")] }).created();
    assert_eq!(names(&r.data_home.join("Trash/files")), ["a 2.txt", "a.txt"]);
    assert_eq!(names(&r.data_home.join("Trash/info")), ["a 2.txt.trashinfo", "a.txt.trashinfo"]);
}

#[test]
fn empty_trash_clears_home_and_volume_trashes() {
    let Ok(vol) = tempfile::tempdir_in("/dev/shm") else { return };
    let r = rig_with(Some(vec![vol.path().to_path_buf()]));
    if fs::metadata(vol.path()).unwrap().dev() == fs::metadata(&r.data_home).unwrap().dev() {
        return;
    }
    let on_home = r.write("h.txt", "h");
    let on_vol = vol.path().join("v.txt");
    fs::write(&on_vol, "v").unwrap();
    r.run(Op::Trash { paths: vec![on_home.clone()] }).created();
    r.run(Op::Trash { paths: vec![on_vol.clone()] }).created();
    let uid = unsafe_uid();
    let vol_trash = vol.path().join(format!(".Trash-{uid}"));
    assert_eq!(names(&vol_trash.join("files")), ["v.txt"]);
    let info = read(&vol_trash.join("info/v.txt.trashinfo"));
    assert!(info.contains("Path=v.txt\n"), "relative to the volume: {info}");

    assert_eq!(r.undo().created(), vec![on_vol.clone()]);
    assert_eq!(read(&on_vol), "v");
    r.run(Op::Trash { paths: vec![on_vol.clone()] }).created();
    r.run(Op::EmptyTrash).created();
    assert!(names(&vol_trash.join("files")).is_empty() && names(&vol_trash.join("info")).is_empty());
    assert!(names(&r.data_home.join("Trash/files")).is_empty());
}

#[test]
fn trash_on_a_volume_without_a_trash_dir_fails_honestly() {
    let Ok(vol) = tempfile::tempdir_in("/dev/shm") else { return };
    let r = rig_with(Some(vec![vol.path().to_path_buf()]));
    if fs::metadata(vol.path()).unwrap().dev() == fs::metadata(&r.data_home).unwrap().dev() {
        return;
    }
    let f = vol.path().join("v.txt");
    fs::write(&f, "v").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(vol.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let e = r.run(Op::Trash { paths: vec![f.clone()] }).error();
    fs::set_permissions(vol.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(e.starts_with("Cannot move to Trash on this volume"), "{e}");
    assert!(f.exists());
}

fn make_tree(r: &Rig) {
    r.write("tree/a.txt", "alpha");
    r.write("tree/sub/b.txt", "beta");
    r.dir("tree/empty");
    symlink("a.txt", r.p("tree/link")).unwrap();
}

#[test]
fn zip_round_trip() {
    let r = rig();
    make_tree(&r);
    let created = r.run(Op::Compress { paths: vec![r.p("tree")], dest: r.work.path().to_path_buf() }).created();
    assert_eq!(created, vec![r.p("tree.zip")]);
    let out = r.dir("out");
    let created = r.run(Op::Extract { archive: r.p("tree.zip"), dest_dir: out.clone() }).created();
    assert_eq!(created, vec![out.join("tree")]);
    assert_eq!(read(&out.join("tree/a.txt")), "alpha");
    assert_eq!(read(&out.join("tree/sub/b.txt")), "beta");
    assert!(out.join("tree/empty").is_dir());
    assert_eq!(fs::read_link(out.join("tree/link")).unwrap(), PathBuf::from("a.txt"));
    assert_eq!(names(&out), ["tree"]);

    let created = r.run(Op::Extract { archive: r.p("tree.zip"), dest_dir: out.clone() }).created();
    assert_eq!(created, vec![out.join("tree 2")]);
    r.undo().created();
    assert_eq!(names(&out), ["tree"]);
}

#[test]
fn zip_of_several_items_extracts_into_a_named_folder() {
    let r = rig();
    let a = r.write("a.txt", "a");
    let b = r.write("b.txt", "b");
    let zip = r.p("bundle.zip");
    assert_eq!(r.run(Op::Compress { paths: vec![a, b], dest: zip.clone() }).created(), vec![zip.clone()]);
    assert_eq!(
        r.run(Op::Compress { paths: vec![r.p("a.txt")], dest: zip.clone() }).created(),
        vec![r.p("bundle 2.zip")]
    );
    let out = r.dir("out");
    let created = r.run(Op::Extract { archive: zip, dest_dir: out.clone() }).created();
    assert_eq!(created, vec![out.join("bundle")]);
    assert_eq!(names(&out.join("bundle")), ["a.txt", "b.txt"]);
    assert!(names(&out).iter().all(|n| !n.starts_with('.')));
}

#[test]
fn zip_entries_never_write_through_the_archives_own_symlinks() {
    use std::io::Write;
    let r = rig();
    let outside = r.dir("outside");
    let mut zip = zip::ZipWriter::new(fs::File::create(r.p("evil.zip")).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    zip.add_symlink("link", outside.to_string_lossy(), opts).unwrap();
    zip.start_file("link/.bashrc", opts).unwrap();
    zip.write_all(b"pwned").unwrap();
    zip.finish().unwrap();
    let out = r.dir("out");
    let e = r.run(Op::Extract { archive: r.p("evil.zip"), dest_dir: out.clone() }).error();
    assert_eq!(e, "Unsafe path in archive: link/.bashrc");
    assert!(names(&outside).is_empty(), "nothing written outside staging");
    assert!(names(&out).is_empty(), "no staging left behind");
}

#[test]
fn extract_tar_gz_with_the_system_tar() {
    let r = rig();
    if std::env::var_os("PATH").is_none_or(|p| !std::env::split_paths(&p).any(|d| d.join("tar").is_file())) {
        return;
    }
    r.write("src/one.txt", "1");
    r.write("src/two.txt", "2");
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(r.p("pack.tar.gz"))
        .arg("-C")
        .arg(r.p("src"))
        .args(["one.txt", "two.txt"])
        .status()
        .unwrap();
    assert!(status.success());
    let out = r.dir("out");
    let created = r.run(Op::Extract { archive: r.p("pack.tar.gz"), dest_dir: out.clone() }).created();
    assert_eq!(created, vec![out.join("pack")]);
    assert_eq!(read(&out.join("pack/two.txt")), "2");
}

#[test]
fn extract_of_an_unknown_format_names_the_missing_tool() {
    let r = rig();
    r.write("x.7z", "not really");
    let out = r.dir("out");
    let e = r.run(Op::Extract { archive: r.p("x.7z"), dest_dir: out.clone() }).error();
    if std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("bsdtar").is_file())) {
        assert!(e.starts_with("Extract failed"), "{e}");
    } else {
        assert!(e.contains("bsdtar is not installed"), "{e}");
    }
    assert!(names(&out).is_empty(), "no staging left behind");
}

#[test]
fn delete_removes_trees() {
    let r = rig();
    make_tree(&r);
    r.run(Op::Delete { paths: vec![r.p("tree")] }).created();
    assert!(!r.p("tree").exists());
    r.run(Op::Delete { paths: vec![r.p("nope")] }).error();
}
