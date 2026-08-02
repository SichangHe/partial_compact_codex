use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use partial_compact_codex::storage::Store;
use rusqlite::Connection;
use tempfile::tempdir;

fn pcodx(args: &[&str], input: Option<&str>) -> Output {
    pcodx_in_dir(args, input, None)
}

fn pcodx_in_dir(args: &[&str], input: Option<&str>, cwd: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pcodx"));
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn resume_reopens_selected_compacted_ledger_after_prior_processes_exit() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("pcodx.sqlite3");
    let db = db_path.to_str().unwrap();
    let session = "resume-ledger";

    assert!(pcodx(&["--db", db, "--session", session, "init"], None)
        .status
        .success());
    assert!(pcodx(
        &[
            "--db",
            db,
            "--session",
            session,
            "record",
            "--role",
            "assistant",
            "--text",
            "obsolete exact ledger text",
        ],
        None,
    )
    .status
    .success());
    assert!(pcodx(
        &[
            "--db",
            db,
            "--session",
            session,
            "record",
            "--role",
            "assistant",
            "--text",
            "durable retained detail",
        ],
        None,
    )
    .status
    .success());
    assert!(pcodx(
        &[
            "--db",
            db,
            "--session",
            session,
            "compact",
            "--from",
            "msg1",
            "--to",
            "msg1",
            "--summary",
            "retained compacted summary",
        ],
        None,
    )
    .status
    .success());

    let resumed = pcodx(
        &[
            "--db",
            db,
            "--session",
            session,
            "resume",
            "--text",
            "follow-up after restart",
        ],
        Some("/show\n/exit\n"),
    );
    assert!(resumed.status.success());
    let resumed_stdout = String::from_utf8(resumed.stdout).unwrap();
    assert!(resumed_stdout.contains("retained compacted summary\n<aboveturn id=\"cmp1\"/>"));
    assert!(resumed_stdout.contains("durable retained detail\n<aboveturn id=\"msg2\"/>"));
    assert!(resumed_stdout.contains("follow-up after restart\n<aboveturn id=\"msg3\"/>"));
    assert!(!resumed_stdout.contains("obsolete exact ledger text"));

    let after_resume = pcodx(&["--db", db, "--session", session, "show"], None);
    assert!(after_resume.status.success());
    let after_resume_stdout = String::from_utf8(after_resume.stdout).unwrap();
    assert!(after_resume_stdout.contains("retained compacted summary"));
    assert!(after_resume_stdout.contains("durable retained detail"));
    assert!(after_resume_stdout.contains("follow-up after restart"));
    assert!(!after_resume_stdout.contains("obsolete exact ledger text"));
}

#[test]
fn resume_rejects_ambiguous_session_selection() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("pcodx.sqlite3");
    let output = pcodx(
        &[
            "--db",
            db_path.to_str().unwrap(),
            "--session",
            "selected",
            "resume",
            "--last",
        ],
        None,
    );

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("pass exactly one of --session or --last to resume"));
}

#[test]
fn resume_without_selector_does_not_initialize_or_migrate_storage() {
    let temp = tempdir().unwrap();
    let absent_db = temp.path().join("absent").join("pcodx.sqlite3");
    let absent_output = pcodx(&["--db", absent_db.to_str().unwrap(), "resume"], None);
    assert!(!absent_output.status.success());
    assert!(!absent_db.exists());
    assert!(!absent_db.parent().unwrap().exists());

    let legacy_db = temp.path().join("legacy.sqlite3");
    let conn = Connection::open(&legacy_db).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE sessions(
          id TEXT PRIMARY KEY,
          cwd TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          upstream_session_id TEXT,
          kv_cache_boundary TEXT NOT NULL DEFAULT 'future_turn_only'
        );
        INSERT INTO sessions(id, cwd, created_at_ms, updated_at_ms)
        VALUES ('legacy', '/tmp', 1, 1);
        ",
    )
    .unwrap();
    drop(conn);
    let legacy_before = std::fs::read(&legacy_db).unwrap();

    let legacy_output = pcodx(&["--db", legacy_db.to_str().unwrap(), "resume"], None);
    assert!(!legacy_output.status.success());
    assert_eq!(std::fs::read(&legacy_db).unwrap(), legacy_before);
}

#[test]
fn resume_last_rejects_ambiguous_legacy_write_order_without_migration() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("legacy-tied.sqlite3");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "
        CREATE TABLE sessions(
          id TEXT PRIMARY KEY,
          cwd TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          upstream_session_id TEXT,
          kv_cache_boundary TEXT NOT NULL DEFAULT 'future_turn_only'
        );
        INSERT INTO sessions(id, cwd, created_at_ms, updated_at_ms) VALUES
          ('z-first', '/tmp', 1, 2),
          ('a-second', '/tmp', 1, 2);
        ",
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&db_path).unwrap();

    let output = pcodx(
        &["--db", db_path.to_str().unwrap(), "resume", "--last"],
        None,
    );

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("cannot safely resume --last because legacy session write order is ambiguous"));
    assert_eq!(std::fs::read(&db_path).unwrap(), before);
}

#[test]
fn resume_without_selector_rejects_multiple_sessions_without_mutating_a_ledger() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("pcodx.sqlite3");
    let db = db_path.to_str().unwrap();
    for (session, text) in [
        ("older", "older retained turn"),
        ("newer", "newer retained turn"),
    ] {
        assert!(pcodx(&["--db", db, "--session", session, "init"], None)
            .status
            .success());
        assert!(pcodx(
            &[
                "--db",
                db,
                "--session",
                session,
                "record",
                "--role",
                "assistant",
                "--text",
                text,
            ],
            None,
        )
        .status
        .success());
    }

    let before = Store::open(&db_path).unwrap();
    assert_eq!(before.last_session_id().unwrap().as_deref(), Some("newer"));
    assert_eq!(
        before.messages("older").unwrap()[0].text,
        "older retained turn"
    );
    assert_eq!(
        before.messages("newer").unwrap()[0].text,
        "newer retained turn"
    );
    drop(before);

    let output = pcodx(&["--db", db, "resume"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("pass exactly one of --session or --last to resume"));

    let after = Store::open(&db_path).unwrap();
    assert_eq!(after.last_session_id().unwrap().as_deref(), Some("newer"));
    assert_eq!(
        after.messages("older").unwrap()[0].text,
        "older retained turn"
    );
    assert_eq!(
        after.messages("newer").unwrap()[0].text,
        "newer retained turn"
    );
}

#[test]
fn resume_uses_stored_session_cwd_for_relative_record_files() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("pcodx.sqlite3");
    let db = db_path.to_str().unwrap();
    let stored_cwd = temp.path().join("stored-cwd");
    let other_cwd = temp.path().join("other-cwd");
    std::fs::create_dir(&stored_cwd).unwrap();
    std::fs::create_dir(&other_cwd).unwrap();
    std::fs::write(
        stored_cwd.join("retained.txt"),
        "stored working directory text",
    )
    .unwrap();

    assert!(pcodx_in_dir(
        &[
            "--db",
            db,
            "--session",
            "cwd-session",
            "--cwd",
            "stored-cwd",
            "init",
        ],
        None,
        Some(temp.path()),
    )
    .status
    .success());

    let resumed = pcodx_in_dir(
        &["--db", db, "--session", "cwd-session", "resume"],
        Some("/record-file assistant retained.txt\n/show\n/exit\n"),
        Some(&other_cwd),
    );
    assert!(resumed.status.success());
    let stdout = String::from_utf8(resumed.stdout).unwrap();
    assert!(stdout.contains(&format!(
        "text_file={}",
        stored_cwd.join("retained.txt").display()
    )));
    assert!(stdout.contains("stored working directory text\n<aboveturn id=\"msg1\"/>"));
}

#[test]
fn resume_rejects_legacy_relative_cwd_without_override() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("pcodx.sqlite3");
    let db = db_path.to_str().unwrap();
    let session = "legacy-cwd";
    let mut store = Store::open(&db_path).unwrap();
    store.create_session(Some(session), temp.path()).unwrap();
    drop(store);
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE sessions SET cwd = 'legacy-relative' WHERE id = ?1",
        [session],
    )
    .unwrap();
    drop(conn);

    let process_cwd = temp.path().join("new-process-cwd");
    let unintended_cwd = process_cwd.join("legacy-relative");
    std::fs::create_dir(&process_cwd).unwrap();
    std::fs::create_dir(&unintended_cwd).unwrap();
    std::fs::write(unintended_cwd.join("input.txt"), "unintended input").unwrap();

    let rejected = pcodx_in_dir(
        &["--db", db, "--session", session, "resume"],
        Some("/record-file assistant input.txt\n/exit\n"),
        Some(&process_cwd),
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8(rejected.stderr)
        .unwrap()
        .contains("stores a relative working directory; pass --cwd DIR to resume"));
    let store = Store::open(&db_path).unwrap();
    assert!(store.messages(session).unwrap().is_empty());
    drop(store);

    let override_cwd = temp.path().join("override-cwd");
    std::fs::create_dir(&override_cwd).unwrap();
    std::fs::write(override_cwd.join("input.txt"), "override input").unwrap();
    let overridden = pcodx_in_dir(
        &[
            "--db",
            db,
            "--session",
            session,
            "--cwd",
            override_cwd.to_str().unwrap(),
            "resume",
        ],
        Some("/record-file assistant input.txt\n/exit\n"),
        Some(&process_cwd),
    );
    assert!(overridden.status.success());
    assert_eq!(
        Store::open(&db_path).unwrap().messages(session).unwrap()[0].text,
        "override input"
    );
}
