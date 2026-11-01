use sigen_modbus::{WriteAuditor, WriteRecord};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub struct WriteAuditLog {
    file: Mutex<File>,
}

impl WriteAuditLog {
    pub fn server_started(
        &self,
        listen: std::net::SocketAddr,
        target: std::net::SocketAddr,
        unit: u8,
    ) -> io::Result<()> {
        self.append(serde_json::json!({
            "event":"server_started", "url":format!("http://{listen}"),
            "target":target.to_string(), "monitored_unit_id":unit,
            "version":env!("CARGO_PKG_VERSION")
        }))
    }

    /// Serialize complete JSONL records and sync each one before reporting success.
    /// Errors propagate to the write path rather than silently losing audit records.
    fn append(&self, mut entry: serde_json::Value) -> io::Result<()> {
        entry["timestamp_unix_ms"] = serde_json::json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_millis()
        );
        entry["process_id"] = serde_json::json!(std::process::id());
        let mut bytes = serde_json::to_vec(&entry)?;
        bytes.push(b'\n');
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("write audit lock poisoned"))?;
        file.write_all(&bytes)?;
        file.sync_data()
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: Mutex::new(options.open(path)?),
        })
    }
}

impl WriteAuditor for WriteAuditLog {
    fn record(&self, r: &WriteRecord) -> io::Result<()> {
        let entry = serde_json::json!({
            "phase":r.phase, "target":r.target.to_string(),
            "unit_id":r.unit, "transaction_id":r.transaction, "function":r.function,
            "register":r.register, "values":r.values, "frame_hex":r.frame_hex, "source":r.source
        });
        self.append(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_appends_json_records_and_preserves_source_and_wire_data() {
        let path =
            std::env::temp_dir().join(format!("sigen-write-audit-{}.jsonl", std::process::id()));
        let record = WriteRecord {
            phase: "attempt",
            target: "127.0.0.1:502".parse().unwrap(),
            unit: 247,
            transaction: 1,
            function: 6,
            register: 40029,
            values: vec![1],
            frame_hex: "000100000006f7069c5d0001".into(),
            source: "http POST /api/control; requested=RemoteEms".into(),
        };
        let audit = WriteAuditLog::open(&path).unwrap();
        audit.record(&record).unwrap();
        drop(audit);
        let audit = WriteAuditLog::open(&path).unwrap();
        audit.record(&record).unwrap();
        drop(audit);
        let text = std::fs::read_to_string(&path).unwrap();
        let entries: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["source"], record.source);
        assert_eq!(entries[0]["target"], "127.0.0.1:502");
        assert_eq!(entries[0]["values"][0], 1);
        assert_eq!(entries[0]["frame_hex"], record.frame_hex);
        assert!(entries[0]["timestamp_unix_ms"].as_u64().unwrap() > 0);
        std::fs::remove_file(path).unwrap();
    }
}
