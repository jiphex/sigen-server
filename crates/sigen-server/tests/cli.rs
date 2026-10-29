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
    let mut child = server()
        .env("SIGEN_WRITE_AUDIT_FILE", &audit_path)
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
