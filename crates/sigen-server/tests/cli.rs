use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

fn server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sigen-server"));
    command.env_clear();
    command
}

#[test]
fn help_hides_credentials_and_environment_options_can_be_overridden() {
    let output = server()
        .env("SIGEN_MQTT_PASSWORD", "test-secret-password")
        .env("SIGEN_MQTT_USERNAME", "test-secret-username")
        .env("SIGEN_PUSHOVER_TOKEN", "test-private-token")
        .env("SIGEN_PUSHOVER_USER", "test-private-user")
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--address"));
    assert!(help.contains("--mqtt-tls"));
    assert!(help.contains("--dangerously-enable-modbus-writes"));
    assert!(help.contains("DANGER"));
    assert!(!help.contains("--read-only"));
    assert!(!help.contains("test-secret-password"));
    assert!(!help.contains("test-secret-username"));
    assert!(!help.contains("test-private-token"));
    assert!(!help.contains("test-private-user"));
    assert!(help.contains("--pushover-token"));
    let output = server()
        .env("SIGEN_ADDRESS", "invalid-env-address")
        .env("SIGEN_UNIT_ID", "999")
        .args([
            "--address",
            "127.0.0.1:502",
            "--unit-id",
            "2",
            "--read-function",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("--read-function"));
    assert!(!error.contains("invalid-env-address"));
    assert!(!error.contains("999"));
}

#[test]
fn environment_only_configuration_starts_and_logs_full_bound_url() {
    let audit_path =
        std::env::temp_dir().join(format!("sigen-startup-audit-{}.jsonl", std::process::id()));
    let journal_path = audit_path.with_extension("sqlite3");
    let mut child = server()
        .env("SIGEN_WRITE_AUDIT_FILE", &audit_path)
        .env("SIGEN_JOURNAL_FILE", &journal_path)
        .env("SIGEN_ADDRESS", "127.0.0.1:1")
        .env("SIGEN_BIND", "127.0.0.1:0")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line.contains("Server started:") {
                let _ = tx.send(line);
                break;
            }
        }
    });
    let line = rx.recv_timeout(Duration::from_secs(5));
    // Always reap the subprocess before asserting, including on startup failure.
    let _ = child.kill();
    let _ = child.wait();
    reader.join().unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", journal_path.display()));
    }
    let line = line.expect("server did not log startup URL");
    assert!(line.contains("Server started: http://127.0.0.1:"), "{line}");
    assert!(!line.contains("http://127.0.0.1:0"));
    let audit = std::fs::read_to_string(&audit_path).unwrap();
    std::fs::remove_file(audit_path).unwrap();
    let entry: serde_json::Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(entry["event"], "server_started");
    assert_eq!(entry["target"], "127.0.0.1:1");
    assert_eq!(entry["monitored_unit_id"], 1);
    assert!(line.contains(entry["url"].as_str().unwrap()));
    assert!(entry["timestamp_unix_ms"].as_u64().unwrap() > 0);
}

#[cfg(unix)]
#[test]
fn graceful_shutdown_flushes_journal_and_restart_preserves_history() {
    use std::{
        io::{Read, Write},
        net::TcpStream,
        time::Instant,
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory =
        std::env::temp_dir().join(format!("sigen-journal-lifecycle-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("events.sqlite3");
    for run in 0..2 {
        let mut command = server();
        // Environment settings cannot opt into writes; the CLI flag is required.
        command
            .env("SIGEN_READ_ONLY", "false")
            .env("SIGEN_DANGEROUSLY_ENABLE_MODBUS_WRITES", "true")
            .env("SIGEN_MQTT_CONTROLS", "true");
        if run == 1 {
            command.arg("--dangerously-enable-modbus-writes");
        }
        let mut child = ChildGuard(
            command
                .env("SIGEN_JOURNAL_FILE", &path)
                .env("SIGEN_WRITE_AUDIT_FILE", directory.join("writes.jsonl"))
                .env("SIGEN_ADDRESS", "127.0.0.1:1")
                .env("SIGEN_BIND", "127.0.0.1:0")
                .env("RUST_LOG", "info")
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stderr = child.0.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                if let Some((_, url)) = line.split_once("Server started: http://") {
                    // The tracing formatter may append ANSI resets or structured fields.
                    let address: String = url
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | ':'))
                        .collect();
                    let _ = tx.send(address);
                }
            }
        });
        let address = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // An HTTP response also establishes that the shutdown signal listener is active.
        let mut stream = TcpStream::connect(&address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("server_started"));
        let mut stream = TcpStream::connect(&address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            response.contains(if run == 0 {
                "\"read_only\":true"
            } else {
                "\"read_only\":false"
            }),
            "{response}"
        );
        assert!(
            Command::new("kill")
                .args(["-TERM", &child.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                assert!(exit.success());
                break;
            }
            assert!(Instant::now() < deadline, "graceful shutdown timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        reader.join().unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM events WHERE kind='server_started'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, run + 1);
        let last: String = connection
            .query_row(
                "SELECT kind FROM events ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last, "server_stopped");
        let requested: i64 = connection
            .query_row(
                "SELECT count(*) FROM events WHERE kind='shutdown_requested'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(requested, run + 1);
    }
    std::fs::remove_dir_all(directory).unwrap();
}
