use rusqlite::Connection;
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    path: PathBuf,
    connection: Option<Connection>,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "sigen-journal-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("events.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("PRAGMA journal_mode=WAL;
            PRAGMA user_version=1;
            CREATE TABLE events (id INTEGER PRIMARY KEY, timestamp_unix_ms INTEGER NOT NULL,
                session TEXT NOT NULL, target TEXT NOT NULL, unit INTEGER NOT NULL,
                serial_number TEXT, category TEXT NOT NULL, kind TEXT NOT NULL, details TEXT NOT NULL);").unwrap();
        for (id, category, kind) in [
            (1, "server", "server_started"),
            (2, "alert", "alert_activated"),
            (3, "control", "control_requested"),
            (4, "control", "control_confirmed"),
        ] {
            connection.execute("INSERT INTO events VALUES (?1,1700000000000,'session-1','127.0.0.1:502',1,'serial-1',?2,?3,?4)",
                rusqlite::params![id,category,kind,serde_json::json!({"request_id":"request-1", "message":"quoted ' text", "exact":u64::MAX}).to_string()]).unwrap();
        }
        Self {
            path,
            connection: Some(connection),
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sigen-journal"))
            .env_clear()
            .env("SIGEN_JOURNAL_FILE", &self.path)
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.connection.take();
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}

#[test]
fn reads_live_wal_in_newest_order_and_supports_filters_cursors_and_jsonl() {
    let fixture = Fixture::new();
    // Keep the writer open: recent rows can still reside in the WAL.
    let output = fixture.run(&["--limit", "2"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.starts_with("#4 2023-11-14T22:13:20.000Z [control] control_confirmed"),
        "{text}"
    );
    assert!(text.contains("#3 "));
    assert!(text.contains("--before 3"));
    let output = fixture.run(&["--category", "control", "--before", "4", "--json"]);
    assert!(output.status.success());
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], 3);
    assert_eq!(events[0]["serial_number"], "serial-1");
    assert_eq!(events[0]["session"], "session-1");
    assert_eq!(events[0]["details"]["exact"].as_u64(), Some(u64::MAX));
    assert_eq!(events[0]["details"]["message"], "quoted ' text");
    let empty = fixture.run(&["--category", "settings", "--json"]);
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let empty = fixture.run(&["--before", "1"]);
    assert_eq!(
        String::from_utf8(empty.stdout).unwrap(),
        "No events found.\n"
    );
    let count: i64 = fixture
        .connection
        .as_ref()
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 4);
}

#[test]
fn bad_paths_and_schemas_fail_without_creating_or_migrating_a_database() {
    let fixture = Fixture::new();
    let missing = fixture.path.with_file_name("missing.sqlite3");
    let output = fixture.run(&["--journal-file", missing.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot open journal"));
    assert!(!missing.exists());
    fixture
        .connection
        .as_ref()
        .unwrap()
        .execute_batch("PRAGMA user_version=2")
        .unwrap();
    let output = fixture.run(&[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Unsupported journal schema version 2")
    );
    let version: i64 = fixture
        .connection
        .as_ref()
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let corrupt = fixture.path.with_file_name("corrupt.sqlite3");
    std::fs::write(&corrupt, "not a database").unwrap();
    assert!(
        !fixture
            .run(&["--journal-file", corrupt.to_str().unwrap()])
            .status
            .success()
    );
}

#[test]
fn validates_options_and_explicit_path_overrides_environment() {
    let fixture = Fixture::new();
    for args in [
        ["--limit", "0"],
        ["--limit", "101"],
        ["--before", "0"],
        ["--category", "unknown"],
    ] {
        assert_eq!(fixture.run(&args).status.code(), Some(2));
    }
    let output = fixture.run(&["--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--journal-file"));
    let output = Command::new(env!("CARGO_BIN_EXE_sigen-journal"))
        .env_clear()
        .env("SIGEN_JOURNAL_FILE", "does-not-exist.sqlite3")
        .arg("--journal-file")
        .arg(&fixture.path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                assert!(event.is_object());
                event
            })
            .count(),
        4
    );
}
