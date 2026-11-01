//! Shared event decoding and queries for the server and read-only journal CLI.
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Server,
    Connectivity,
    Alert,
    Control,
    Settings,
}
impl Category {
    pub(super) fn key(self) -> &'static str {
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

/// Read one extra event so callers can determine whether another page exists.
pub fn read(connection: &Connection, query: &Query) -> rusqlite::Result<Vec<Event>> {
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
