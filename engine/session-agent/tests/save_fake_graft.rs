//! The pod checkpoint against a fake `graft` (WI-4267): a shell stub that
//! records its argv, serves `status`, `diff`, `clone`, `workspace` and
//! `commit` from fixture files, and at the moment `commit` runs scans the
//! save copy for anything secret.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use vogt_engine_contract::{
    salvage::SkipReason,
    saved_work::{SaveTrigger, SavedWorkReport},
};
use vogt_session_agent::save::{Checkpoint, GraftCli, SaveSettings, Saver};

const FAKE_GRAFT: &str = r#"#!/bin/sh
set -eu
F="$FAKE_GRAFT_DIR"
dir=$(pwd)
while [ $# -gt 0 ]; do
  case "$1" in
    --token-file) shift 2 ;;
    -C) dir=$2; shift 2 ;;
    *) break ;;
  esac
done
cd "$dir"
printf '%s\n' "$*" >>"$F/argv.log"
cmd=$1; shift
case "$cmd" in
  status)
    if [ -f .fake-save ]; then
      head=null
      [ -f "$F/head" ] && head="\"$(cat "$F/head")\""
      printf '{"line":"rust-2","base":"B1","head":%s,"changes":[{"path":"x","status":"modified"}],"clean":false}\n' "$head"
    else
      cat "$F/status.json"
    fi ;;
  diff) cat "$F/stack.json" ;;
  clone)
    target=$2
    mkdir -p "$target/.graft"
    cp -R "$F/base/." "$target/"
    touch "$target/.fake-save" ;;
  workspace) echo '{}' ;;
  commit)
    if [ -f "$F/fail-commit" ]; then echo 'graft: the server is unavailable' >&2; exit 7; fi
    find . -path ./.graft -prune -o \( -type f -o -type l \) -print | while read -r p; do
      case "$p" in
        */.env|*.token) echo "name $p" >>"$F/leaks" ;;
      esac
      if [ -L "$p" ]; then
        case "$(readlink "$p")" in /*|../*) echo "link $p" >>"$F/leaks" ;; esac
      elif grep -q 'LEAKME\|ghp_' "$p"; then
        echo "content $p" >>"$F/leaks"
      fi
    done
    n=$(( $(cat "$F/commits" 2>/dev/null || echo 0) + 1 ))
    echo "$n" >"$F/commits"
    echo "draft-$n" >"$F/head"
    rm -rf "$F/snap-$n"
    cp -a . "$F/snap-$n"
    printf '{"id":"draft-%s"}\n' "$n" ;;
  *) echo "fake graft: unknown $cmd" >&2; exit 2 ;;
esac
"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    fake: PathBuf,
    work: PathBuf,
    save: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("fake");
        let work = tmp.path().join("work");
        let save = tmp.path().join("save");
        let outside = tmp.path().join("outside");
        for dir in [&fake, &work, &outside, &fake.join("base/deploy")] {
            fs::create_dir_all(dir).unwrap();
        }
        let program = fake.join("graft");
        fs::write(&program, FAKE_GRAFT).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        // The line base: what `clone --at` lays out.
        let base = fake.join("base");
        fs::write(base.join("keep.txt"), "one\n").unwrap();
        fs::write(base.join("gone.txt"), "bye\n").unwrap();
        fs::write(base.join("run.sh"), "#!/bin/sh\n").unwrap();
        fs::write(base.join("stack.rs"), "base\n").unwrap();
        fs::write(base.join("deploy/app.pem"), "PUBLIC-BASE\n").unwrap();
        // The session's checkout starts as the base.
        fs::create_dir_all(work.join(".graft")).unwrap();
        fs::write(work.join(".graft/config"), "repo = \"vogt\"\n").unwrap();
        copy_dir(&base, &work);
        fs::write(
            fake.join("stack.json"),
            r#"{"from":"B1","to":"H1","files":[{"path":"stack.rs","op":"put"}],"next":null}"#,
        )
        .unwrap();
        Self {
            _tmp: tmp,
            fake,
            work,
            save,
            outside,
        }
    }

    fn status(&self, head: Option<&str>, changes: &[(&str, &str)]) {
        let changes: Vec<String> = changes
            .iter()
            .map(|(p, s)| format!(r#"{{"path":"{p}","status":"{s}"}}"#))
            .collect();
        let head = head.map_or("null".to_owned(), |h| format!("\"{h}\""));
        fs::write(
            self.fake.join("status.json"),
            format!(
                r#"{{"line":"rust-2","base":"B1","head":{head},"clean":false,"changes":[{}]}}"#,
                changes.join(",")
            ),
        )
        .unwrap();
    }

    fn saver(&self, max_file_bytes: u64) -> Saver {
        let settings = SaveSettings {
            save_dir: self.save.clone(),
            max_file_bytes,
            ..SaveSettings::default()
        };
        Saver::new(
            self.work.clone(),
            &settings,
            "s1",
            GraftCli {
                program: self.fake.join("graft"),
                token_file: None,
                env: vec![(
                    "FAKE_GRAFT_DIR".into(),
                    self.fake.to_string_lossy().into_owned(),
                )],
            },
            &[],
            &[],
        )
        .unwrap()
    }

    fn argv(&self) -> String {
        fs::read_to_string(self.fake.join("argv.log")).unwrap_or_default()
    }

    fn leaks(&self) -> String {
        fs::read_to_string(self.fake.join("leaks")).unwrap_or_default()
    }

    fn snap(&self, n: u32) -> PathBuf {
        self.fake.join(format!("snap-{n}"))
    }
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn saved(result: Result<Checkpoint, String>) -> SavedWorkReport {
    match result {
        Ok(Checkpoint::Saved(report)) => report,
        other => panic!("expected a saved checkpoint, got {other:?}"),
    }
}

/// A checkout with every kind of change the save must keep or refuse.
fn dirty(fx: &Fixture) {
    let w = &fx.work;
    fs::write(w.join("keep.txt"), "two\n").unwrap();
    fs::remove_file(w.join("gone.txt")).unwrap();
    fs::write(w.join("new.txt"), "new\n").unwrap();
    fs::write(w.join("bin.dat"), [0u8, 159, 146, 150, 255]).unwrap();
    fs::set_permissions(w.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(w.join("stack.rs"), "stacked\n").unwrap();
    fs::write(w.join(".env"), "TOKEN=LEAKME\n").unwrap();
    fs::write(w.join("secret.token"), "LEAKME\n").unwrap();
    fs::write(w.join("deploy/app.pem"), "LEAKME\n").unwrap();
    fs::write(
        w.join("notes.txt"),
        format!("pasted ghp_{}\n", "A".repeat(36)),
    )
    .unwrap();
    fs::create_dir_all(w.join("target")).unwrap();
    fs::write(w.join("target/x.o"), "LEAKME\n").unwrap();
    std::os::unix::fs::symlink("keep.txt", w.join("link-ok")).unwrap();
    std::os::unix::fs::symlink(fx.outside.join("sa"), w.join("link-out")).unwrap();
    fs::write(fx.outside.join("sa"), "LEAKME\n").unwrap();
    fs::hard_link(fx.outside.join("sa"), w.join("hard.txt")).unwrap();
    fs::write(w.join("big.bin"), vec![b'b'; 4096]).unwrap();
    fx.status(
        Some("H1"),
        &[
            ("keep.txt", "modified"),
            ("gone.txt", "deleted"),
            ("new.txt", "added"),
            ("bin.dat", "added"),
            ("run.sh", "modified"),
            (".env", "added"),
            ("secret.token", "added"),
            ("deploy/app.pem", "modified"),
            ("notes.txt", "added"),
            ("target/x.o", "added"),
            ("link-ok", "added"),
            ("link-out", "added"),
            ("hard.txt", "added"),
            ("big.bin", "added"),
        ],
    );
}

#[test]
fn no_secret_is_in_the_save_copy_when_commit_runs() {
    let fx = Fixture::new();
    dirty(&fx);
    let mut saver = fx.saver(1024);
    let report = saved(saver.checkpoint(SaveTrigger::Periodic));

    assert_eq!(
        fx.leaks(),
        "",
        "secrets reached the save copy at commit time"
    );
    assert_eq!(report.draft.as_deref(), Some("draft-1"));
    assert_eq!((report.seq, report.gen), (1, 0));
    assert_eq!(report.workspace, "vogt-save-s1-0");
    assert_eq!(
        (report.repo.as_str(), report.line.as_str()),
        ("vogt", "rust-2")
    );
    assert_eq!(report.base, "B1");
    assert!(!report.complete);
    let skipped: Vec<(&str, SkipReason)> = report
        .skipped
        .iter()
        .map(|s| (s.path.as_str(), s.reason))
        .collect();
    for expected in [
        (".env", SkipReason::Secret),
        ("secret.token", SkipReason::Secret),
        ("deploy/app.pem", SkipReason::Secret),
        ("notes.txt", SkipReason::Secret),
        ("link-out", SkipReason::Secret),
        ("hard.txt", SkipReason::Secret),
        ("big.bin", SkipReason::TooLarge),
        ("target/", SkipReason::Ignored),
    ] {
        assert!(
            skipped.contains(&expected),
            "{expected:?} not in {skipped:?}"
        );
    }
    assert_eq!(skipped.len(), 8, "{skipped:?}");

    let snap = fx.snap(1);
    assert_eq!(fs::read_to_string(snap.join("keep.txt")).unwrap(), "two\n");
    assert!(!snap.join("gone.txt").exists());
    assert_eq!(fs::read_to_string(snap.join("new.txt")).unwrap(), "new\n");
    assert_eq!(
        fs::read(snap.join("bin.dat")).unwrap(),
        [0u8, 159, 146, 150, 255]
    );
    assert_eq!(
        fs::metadata(snap.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert_eq!(
        fs::read_to_string(snap.join("stack.rs")).unwrap(),
        "stacked\n"
    );
    // A refused tracked file keeps its base content; it is never deleted.
    assert_eq!(
        fs::read_to_string(snap.join("deploy/app.pem")).unwrap(),
        "PUBLIC-BASE\n"
    );
    assert_eq!(
        fs::read_link(snap.join("link-ok")).unwrap(),
        PathBuf::from("keep.txt")
    );
    for absent in [
        ".env",
        "secret.token",
        "notes.txt",
        "target",
        "link-out",
        "hard.txt",
        "big.bin",
    ] {
        assert!(
            fs::symlink_metadata(snap.join(absent)).is_err(),
            "{absent} is in the save copy"
        );
    }

    let argv = fx.argv();
    let tree = fx.save.join("tree");
    assert!(
        argv.contains(&format!(
            "clone vogt {} --line rust-2 --at B1",
            tree.display()
        )),
        "{argv}"
    );
    assert!(argv.contains("workspace open vogt-save-s1-0"), "{argv}");
    assert!(argv.contains("diff --json --limit 1000 B1 H1"), "{argv}");
    assert!(
        argv.contains(
            "--meta vogt.saved=unlanded --meta vogt.save_seq=1 --meta vogt.trigger=periodic"
        ),
        "{argv}"
    );
    assert!(!argv.contains("--amend"), "{argv}");
    assert!(argv.contains("workspace renew --lease 86400"), "{argv}");
}

#[test]
fn an_unchanged_tree_makes_no_graft_write_and_a_change_amends() {
    let fx = Fixture::new();
    dirty(&fx);
    let mut saver = fx.saver(1024);
    let first = saved(saver.checkpoint(SaveTrigger::Periodic));
    let calls = fx.argv().lines().count();

    match saver.checkpoint(SaveTrigger::Periodic) {
        Ok(Checkpoint::Unchanged(report)) => assert_eq!(report, first),
        other => panic!("expected unchanged, got {other:?}"),
    }
    let after: Vec<String> = fx.argv().lines().skip(calls).map(str::to_owned).collect();
    assert!(
        after
            .iter()
            .all(|l| l.starts_with("status") || l.starts_with("diff")),
        "an unchanged checkpoint wrote to graft: {after:?}"
    );

    // keep.txt goes back to the base and new.txt changes again.
    fs::write(fx.work.join("keep.txt"), "one\n").unwrap();
    fs::write(fx.work.join("new.txt"), "newer\n").unwrap();
    fx.status(
        Some("H1"),
        &[
            ("gone.txt", "deleted"),
            ("new.txt", "added"),
            ("run.sh", "modified"),
        ],
    );
    let second = saved(saver.checkpoint(SaveTrigger::Idle));
    assert_eq!(second.seq, 2);
    assert_eq!(second.draft.as_deref(), Some("draft-2"));
    assert!(second.complete, "{:?}", second.skipped);
    let argv = fx.argv();
    assert!(argv.contains("commit --json --amend"), "{argv}");
    assert!(argv.contains("--meta vogt.trigger=idle"), "{argv}");
    assert_eq!(
        argv.matches("clone ").count(),
        1,
        "the save copy is cloned once"
    );
    let snap = fx.snap(2);
    assert_eq!(fs::read_to_string(snap.join("keep.txt")).unwrap(), "one\n");
    assert_eq!(fs::read_to_string(snap.join("new.txt")).unwrap(), "newer\n");
    assert!(!snap.join("bin.dat").exists());
    assert!(!snap.join("link-ok").exists());
    assert_eq!(
        fs::read_to_string(snap.join("stack.rs")).unwrap(),
        "stacked\n"
    );
    assert_eq!(fx.leaks(), "");
}

#[test]
fn a_clean_tree_reports_no_draft_and_complete() {
    let fx = Fixture::new();
    fx.status(None, &[]);
    let mut saver = fx.saver(1024);
    let report = saved(saver.checkpoint(SaveTrigger::Periodic));
    assert_eq!(report.draft, None);
    assert!(report.complete);
    assert_eq!((report.files, report.seq), (0, 1));
    assert!(!fx.argv().contains("clone"), "a clean pod clones nothing");
}

#[test]
fn a_graft_failure_is_an_error_not_a_panic() {
    let fx = Fixture::new();
    dirty(&fx);
    fs::write(fx.fake.join("fail-commit"), "").unwrap();
    let mut saver = fx.saver(1024);
    let error = saver.checkpoint(SaveTrigger::Hibernate).unwrap_err();
    assert!(error.contains("the server is unavailable"), "{error}");
    // The failed attempt is not remembered as a save: the next one retries.
    fs::remove_file(fx.fake.join("fail-commit")).unwrap();
    let report = saved(saver.checkpoint(SaveTrigger::Hibernate));
    assert_eq!(report.seq, 1);
    assert_eq!(fx.leaks(), "");
}

#[test]
fn a_restarted_agent_resumes_the_same_save_copy() {
    let fx = Fixture::new();
    dirty(&fx);
    saved(fx.saver(1024).checkpoint(SaveTrigger::Periodic));
    fs::write(fx.work.join("new.txt"), "again\n").unwrap();
    let report = saved(fx.saver(1024).checkpoint(SaveTrigger::Periodic));
    assert_eq!(report.seq, 2);
    assert_eq!(fx.argv().matches("clone ").count(), 1);
    assert!(fx.argv().contains("--amend"));
}

#[test]
fn a_git_checkout_is_not_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = SaveSettings {
        save_dir: tmp.path().join("save"),
        ..SaveSettings::default()
    };
    let mut saver = Saver::new(
        tmp.path().to_path_buf(),
        &settings,
        "s1",
        GraftCli {
            program: PathBuf::from("/nonexistent/graft"),
            token_file: None,
            env: Vec::new(),
        },
        &[],
        &[],
    )
    .unwrap();
    assert!(matches!(
        saver.checkpoint(SaveTrigger::Periodic),
        Ok(Checkpoint::Unsupported(_))
    ));
}
