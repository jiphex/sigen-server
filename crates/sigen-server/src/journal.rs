//! Durable event history, independent of telemetry and the fail-closed wire audit.
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Server,
    Connectivity,
    Alert,
    Control,
    Settings,
}
impl Category {
    fn key(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Connectivity => "connectivity",
            Self::Alert => "alert",
            Self::Control => "control",
            Self::Settings => "settings",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub id: i64,
    pub timestamp_unix_ms: i64,
    pub session: String,
    pub target: String,
    pub unit: u8,
    pub serial_number: Option<String>,
    pub category: String,
    pub kind: String,
    pub details: Value,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Health {
    pub written_total: u64,
    pub dropped_total: u64,
    /// Sticky for this process: successful later writes do not conceal a history gap.
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub category: Option<Category>,
    pub before: Option<i64>,
    pub limit: Option<u16>,
}
impl Query {
    pub fn validate(&self) -> Result<(), String> {
        if self.before.is_some_and(|id| id <= 0)
            || self.limit.is_some_and(|limit| limit == 0 || limit > 100)
        {
            return Err("before must be positive and limit must be between 1 and 100".into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub events: Vec<Event>,
    pub next_before: Option<i64>,
    pub health: Health,
}

#[derive(Debug)]
enum Work {
    Record(Event),
    Query(Query, oneshot::Sender<Result<Vec<Event>, String>>),
    Stop(Option<Box<Event>>, oneshot::Sender<Result<(), String>>),
}

#[derive(Clone, Debug)]
pub struct Journal {
    sender: mpsc::SyncSender<Work>,
    health: Arc<Mutex<Health>>,
    session: Arc<String>,
}
impl Serialize for Journal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.health().serialize(serializer)
    }
}

impl Journal {
    /// Open and validate storage before starting hardware. All subsequent SQL runs
    /// on one OS thread; producers never wait for disk or queue space.
    pub fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        // Restrictive creation permissions also govern SQLite's WAL sidecar files.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(options.open(path)?);
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            return Err("event journal schema is newer than this server supports".into());
        }
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp_unix_ms INTEGER NOT NULL,
                 session TEXT NOT NULL,
                 target TEXT NOT NULL,
                 unit INTEGER NOT NULL,
                 serial_number TEXT,
                 category TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 details TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_category_id ON events(category, id);
             PRAGMA user_version=1;
             COMMIT;",
        )?;
        let (sender, receiver) = mpsc::sync_channel(256);
        let health = Arc::new(Mutex::new(Health::default()));
        let worker_health = health.clone();
        thread::Builder::new()
            .name("event-journal".into())
            .spawn(move || {
                while let Ok(work) = receiver.recv() {
                    match work {
                        Work::Record(event) => match insert(&connection, &event) {
                            Ok(()) => {
                                worker_health.lock().expect("journal health").written_total += 1
                            }
                            Err(error) => lost(&worker_health, &error.to_string()),
                        },
                        Work::Query(query, reply) => {
                            if !reply.is_closed() {
                                let _ = reply
                                    .send(read(&connection, &query).map_err(|e| e.to_string()));
                            }
                        }
                        Work::Stop(final_event, reply) => {
                            // FIFO guarantees all earlier accepted events were committed or reported lost.
                            let result = if let Some(event) = final_event {
                                insert(&connection, &event)
                                    .map_err(|error| {
                                        lost(&worker_health, &error.to_string());
                                        error.to_string()
                                    })
                                    .map(|()| {
                                        worker_health
                                            .lock()
                                            .expect("journal health")
                                            .written_total += 1
                                    })
                            } else {
                                Ok(())
                            };
                            drop(connection);
                            let _ = reply.send(result);
                            return;
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            health,
            session: Arc::new(format!("{}-{}", timestamp(), std::process::id())),
        })
    }

    pub fn health(&self) -> Health {
        self.health.lock().expect("journal health").clone()
    }

    pub fn record(
        &self,
        status: &crate::model::Status,
        category: Category,
        kind: &str,
        details: Value,
    ) {
        let event = self.event(status, category, kind, details);
        if self.sender.try_send(Work::Record(event)).is_err() {
            lost(
                &self.health,
                "Event journal queue full or worker unavailable",
            );
        }
    }

    fn event(
        &self,
        status: &crate::model::Status,
        category: Category,
        kind: &str,
        details: Value,
    ) -> Event {
        Event {
            id: 0,
            timestamp_unix_ms: timestamp(),
            session: self.session.as_ref().clone(),
            target: status.target.clone(),
            unit: status.unit,
            serial_number: status.device.as_ref().map(|d| d.serial_number.clone()),
            category: category.key().into(),
            kind: kind.into(),
            details,
        }
    }

    /// Cursor pagination uses insertion IDs, so new events and clock changes do not
    /// shift older pages. SQL parameters and result counts are bounded at the API.
    pub async fn query(&self, query: Query) -> Result<Page, String> {
        query.validate()?;
        let limit = usize::from(query.limit.unwrap_or(50));
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Work::Query(query, reply))
            .map_err(|_| "Event journal is busy or unavailable".to_owned())?;
        let mut events = tokio::time::timeout(Duration::from_secs(3), response)
            .await
            .map_err(|_| "Event journal query timed out".to_owned())?
            .map_err(|_| "Event journal worker stopped".to_owned())??;
        let next_before = if events.len() > limit {
            events.truncate(limit);
            events.last().map(|event| event.id)
        } else {
            None
        };
        Ok(Page {
            events,
            next_before,
            health: self.health(),
        })
    }

    /// Call after stopping producers. The terminal event travels with the stop
    /// request, so a full queue cannot discard the graceful-shutdown record.
    pub async fn finish(&self, status: &crate::model::Status, kind: &str) -> Result<(), String> {
        self.stop(Some(self.event(
            status,
            Category::Server,
            kind,
            serde_json::json!({}),
        )))
        .await
    }

    #[cfg(test)]
    pub async fn shutdown(&self) -> Result<(), String> {
        self.stop(None).await
    }

    /// Wait asynchronously for queue space and completion; no blocking channel
    /// send runs on a Tokio worker.
    async fn stop(&self, final_event: Option<Event>) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            sender
                .send(Work::Stop(final_event.map(Box::new), reply))
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        response.await.map_err(|e| e.to_string())?
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn lost(health: &Mutex<Health>, error: &str) {
    let mut health = health.lock().expect("journal health");
    health.dropped_total += 1;
    if health.last_error.as_deref() != Some(error) {
        tracing::error!(%error, "Event journal lost an event");
    }
    health.last_error = Some(error.into());
}

fn insert(connection: &Connection, event: &Event) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO events (timestamp_unix_ms, session, target, unit, serial_number, category, kind, details)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![event.timestamp_unix_ms, event.session, event.target, event.unit,
            event.serial_number, event.category, event.kind, event.details.to_string()],
    )?;
    Ok(())
}

fn read(connection: &Connection, query: &Query) -> rusqlite::Result<Vec<Event>> {
    // Separate plans let category filtering use its index even for a long-lived journal.
    let (sql, category) = match query.category {
        Some(category) => (
            "SELECT id,timestamp_unix_ms,session,target,unit,serial_number,category,kind,details
             FROM events WHERE id < ?1 AND category = ?2 ORDER BY id DESC LIMIT ?3",
            category.key(),
        ),
        None => (
            "SELECT id,timestamp_unix_ms,session,target,unit,serial_number,category,kind,details
             FROM events WHERE id < ?1 AND ?2 = '' ORDER BY id DESC LIMIT ?3",
            "",
        ),
    };
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map(
            params![
                query.before.unwrap_or(i64::MAX),
                category,
                i64::from(query.limit.unwrap_or(50)) + 1
            ],
            |row| {
                let details: String = row.get(8)?;
                Ok(Event {
                    id: row.get(0)?,
                    timestamp_unix_ms: row.get(1)?,
                    session: row.get(2)?,
                    target: row.get(3)?,
                    unit: row.get(4)?,
                    serial_number: row.get(5)?,
                    category: row.get(6)?,
                    kind: row.get(7)?,
                    details: serde_json::from_str(&details).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            },
        )?
        .collect()
}

#[cfg(test)]
pub(crate) struct TestJournal {
    pub journal: Journal,
    pub path: std::path::PathBuf,
}
#[cfg(test)]
impl TestJournal {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "sigen-journal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("events.sqlite3");
        Self {
            journal: Journal::open(&path).unwrap(),
            path,
        }
    }
}
#[cfg(test)]
impl Drop for TestJournal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Device, Status};
    use serde_json::json;

    #[tokio::test]
    async fn restart_preserves_identity_details_and_cursor_order_despite_clock_changes() {
        let fixture = TestJournal::new();
        let journal = &fixture.journal;
        let mut status = Status::new("127.0.0.1:502".into(), 2);
        status.device = Some(Device {
            model: "Sigen".into(),
            serial_number: "serial-1".into(),
            firmware_version: "1".into(),
        });
        journal.record(&status, Category::Server, "server_started", json!({}));
        journal.record(
            &status,
            Category::Control,
            "control_confirmed",
            json!({"exact":u64::MAX}),
        );
        let first = journal
            .query(Query {
                limit: Some(1),
                ..Query::default()
            })
            .await
            .unwrap();
        assert_eq!(first.events.len(), 1);
        let event = &first.events[0];
        assert_eq!(event.kind, "control_confirmed");
        assert_eq!(event.details["exact"].as_u64(), Some(u64::MAX));
        assert_eq!(event.target, status.target);
        assert_eq!(event.unit, 2);
        assert_eq!(event.serial_number.as_deref(), Some("serial-1"));
        let session = event.session.clone();
        // A wall-clock jump must not reorder pagination or create duplicate entries.
        Connection::open(&fixture.path)
            .unwrap()
            .execute(
                "UPDATE events SET timestamp_unix_ms=1 WHERE id=?1",
                [event.id],
            )
            .unwrap();
        journal.record(&status, Category::Server, "another_event", json!({}));
        let older = journal
            .query(Query {
                before: first.next_before,
                ..Query::default()
            })
            .await
            .unwrap();
        assert_eq!(older.events.len(), 1);
        assert_eq!(older.events[0].kind, "server_started");
        assert!(older.next_before.is_none());
        // Stop is a barrier, including events not yet read by any API request.
        journal.finish(&status, "server_stopped").await.unwrap();
        let reopened = Journal::open(&fixture.path).unwrap();
        let page = reopened.query(Query::default()).await.unwrap();
        assert_eq!(page.events.len(), 4);
        assert_eq!(page.events[0].kind, "server_stopped");
        assert_eq!(page.events[0].session, session);
        assert_eq!(reopened.health().written_total, 0);
        let filtered = reopened
            .query(Query {
                category: Some(Category::Control),
                ..Query::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.events.len(), 1);
        assert_eq!(filtered.events[0].timestamp_unix_ms, 1);
        assert!(!status.controls_ready());
        reopened.shutdown().await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&fixture.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn write_failures_are_visible_and_do_not_prevent_later_recording() {
        let fixture = TestJournal::new();
        let connection = Connection::open(&fixture.path).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_write BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'test storage failure'); END;").unwrap();
        let status = Status::new("127.0.0.1:502".into(), 1);
        fixture
            .journal
            .record(&status, Category::Server, "lost", json!({}));
        let page = fixture.journal.query(Query::default()).await.unwrap();
        assert!(page.events.is_empty());
        assert_eq!(page.health.dropped_total, 1);
        assert!(
            page.health
                .last_error
                .unwrap()
                .contains("test storage failure")
        );
        connection.execute_batch("DROP TRIGGER fail_write").unwrap();
        fixture
            .journal
            .record(&status, Category::Server, "recovered", json!({}));
        let page = fixture.journal.query(Query::default()).await.unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.health.written_total, 1);
        assert_eq!(page.health.dropped_total, 1);
        assert!(page.health.last_error.is_some());
        fixture.journal.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn full_or_disconnected_queue_reports_loss_without_blocking_producers() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let journal = Journal {
            sender,
            health: Arc::new(Mutex::new(Health::default())),
            session: Arc::new("test".into()),
        };
        let status = Status::new("127.0.0.1:502".into(), 1);
        journal.record(&status, Category::Server, "first", json!({}));
        journal.record(&status, Category::Server, "overflow", json!({}));
        assert_eq!(journal.health().dropped_total, 1);
        assert!(journal.query(Query::default()).await.is_err());
        drop(receiver);
        journal.record(&status, Category::Server, "disconnected", json!({}));
        assert_eq!(journal.health().dropped_total, 2);
    }

    #[tokio::test]
    async fn connection_events_only_record_transitions_and_keep_outage_duration() {
        let fixture = TestJournal::new();
        let mut status = Status::new("127.0.0.1:502".into(), 1);
        status.journal = Some(fixture.journal.clone());
        status.connection_event(false, Some("connection refused"));
        status.connection_event(false, Some("different repeated failure"));
        status.connection_event(true, None);
        status.connection_event(true, None);
        let page = fixture.journal.query(Query::default()).await.unwrap();
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.events[0].kind, "hardware_connected");
        assert!(page.events[0].details["outage_seconds"].is_u64());
        assert_eq!(page.events[1].details["error"], "connection refused");
        fixture.journal.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_queries_and_incompatible_or_corrupt_databases() {
        let fixture = TestJournal::new();
        for query in [
            Query {
                limit: Some(0),
                ..Query::default()
            },
            Query {
                limit: Some(101),
                ..Query::default()
            },
            Query {
                before: Some(-1),
                ..Query::default()
            },
        ] {
            assert!(fixture.journal.query(query).await.is_err());
        }
        fixture.journal.shutdown().await.unwrap();
        Connection::open(&fixture.path)
            .unwrap()
            .execute_batch("PRAGMA user_version=2")
            .unwrap();
        assert!(
            Journal::open(&fixture.path)
                .unwrap_err()
                .to_string()
                .contains("newer")
        );
        std::fs::write(&fixture.path, b"not a database").unwrap();
        assert!(Journal::open(&fixture.path).is_err());
    }
}
