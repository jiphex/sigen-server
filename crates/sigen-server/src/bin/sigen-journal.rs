#[path = "../journal/query.rs"]
mod query;

use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

/// List persistent Sigen journal events, newest first. The server need not be running.
#[derive(Parser)]
#[command(name = "sigen-journal", version)]
struct Args {
    /// Existing SQLite journal file
    #[arg(
        long,
        env = "SIGEN_JOURNAL_FILE",
        default_value = "sigen-events.sqlite3"
    )]
    journal_file: PathBuf,
    /// Show only this category
    #[arg(long, value_enum)]
    category: Option<query::Category>,
    /// Maximum number of events
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
    limit: u16,
    /// Show events older than this event ID
    #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
    before: Option<i64>,
    /// Print one JSON object per line (JSONL), including all event fields
    #[arg(long)]
    json: bool,
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    // Do not use Journal::open: querying must neither create a file nor migrate it.
    let connection =
        Connection::open_with_flags(&args.journal_file, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(
            |error| {
                io::Error::other(format!(
                    "Cannot open journal {}: {error}",
                    args.journal_file.display()
                ))
            },
        )?;
    connection.busy_timeout(Duration::from_secs(1))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 1 {
        return Err(format!("Unsupported journal schema version {version}; expected 1").into());
    }
    let query = query::Query {
        category: args.category,
        before: args.before,
        limit: Some(args.limit),
    };
    query.validate().map_err(io::Error::other)?;
    let mut events = query::read(&connection, &query)?;
    let more = events.len() > usize::from(args.limit);
    events.truncate(usize::from(args.limit));
    let mut output = io::BufWriter::new(io::stdout().lock());
    if args.json {
        for event in &events {
            serde_json::to_writer(&mut output, event)?;
            writeln!(output)?;
        }
    } else if events.is_empty() {
        writeln!(output, "No events found.")?;
    } else {
        for event in &events {
            let timestamp: Option<String> = connection.query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1 / 1000.0, 'unixepoch')",
                [event.timestamp_unix_ms],
                |row| row.get(0),
            )?;
            writeln!(
                output,
                "#{} {} [{}] {} {} inverter={} {}",
                event.id,
                timestamp.unwrap_or_else(|| event.timestamp_unix_ms.to_string()),
                event.category,
                event.kind,
                event.target,
                event.unit,
                event.details
            )?;
        }
        if more && let Some(last) = events.last() {
            writeln!(output, "More events available; use --before {}.", last.id)?;
        }
    }
    output.flush()?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
            {
                return std::process::ExitCode::SUCCESS;
            }
            eprintln!("sigen-journal: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
