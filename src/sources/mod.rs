//! Provider adapters for the local activity stores of AI coding harnesses.
//!
//! Every adapter converts a provider's own records into normalized
//! [`ParsedEvent`] values. Only safe metadata leaves an adapter: the
//! workspace folder name, an inferred language, a file basename, a normalized
//! activity label, a model id and timing. Prompts, assistant responses, tool
//! arguments, tool output and full paths are never forwarded.
//!
//! Harnesses expose their activity in three different shapes, and each shape
//! needs its own ingestion path:
//!
//! - [`SessionStore::LogFiles`]: append-only, newline-delimited files that the
//!   worker tails incrementally.
//! - [`SessionStore::JsonFiles`]: a JSON document that is rewritten in place;
//!   the worker re-reads it and reports only records it has not seen before.
//! - [`SessionStore::Sqlite`]: a database that the worker polls for new rows.

pub mod aider;
pub mod amp;
pub mod cline;
pub mod continue_dev;
pub mod copilot_cli;
pub mod crush;
pub mod cursor;
pub mod droid;
pub mod gemini_cli;
pub mod goose;
pub mod jetbrains;
pub mod kilo_code;
pub mod kiro;
pub mod opencode;
pub mod qwen_code;
pub mod roo_code;
pub mod windsurf;
pub mod zed;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::events::ParsedEvent;
use crate::provider::Provider;

/// An activity event together with the session it belongs to.
///
/// Database stores hold many sessions in one file, so every event carries the
/// session key that identifies the conversation it came from.
#[derive(Debug, Clone)]
pub struct StoreEvent {
    pub session: String,
    pub event: ParsedEvent,
}

/// Parse one line of an append-only provider log.
///
/// Harnesses without a dedicated adapter fall back to the shared parsers in
/// [`crate::events`].
pub fn parse_log_line(provider: Provider, line: &str) -> Vec<ParsedEvent> {
    match provider {
        Provider::GeminiCli => gemini_cli::parse_line(line),
        Provider::QwenCode => qwen_code::parse_line(line),
        Provider::CopilotCli => copilot_cli::parse_line(line),
        Provider::FactoryDroid => droid::parse_line(line),
        Provider::Kiro => kiro::parse_line(line),
        Provider::Cursor => cursor::parse_line(line),
        Provider::Windsurf => windsurf::parse_line(line),
        Provider::JetBrains => jetbrains::parse_line(line),
        Provider::Amp => amp::parse_line(line),
        _ => crate::events::parse_line_for(provider, line),
    }
}

/// Parse one line of a provider log with the path of the session file.
///
/// Some harnesses keep the workspace of a session outside of the log, in the
/// name or the location of the file. Those adapters need the path to resolve
/// it.
pub fn parse_log_line_at(provider: Provider, path: &Path, line: &str) -> Vec<ParsedEvent> {
    match provider {
        Provider::Aider => aider::parse_line_at(path, line),
        _ => parse_log_line(provider, line),
    }
}

/// Parse a JSON document that the provider rewrites in place.
///
/// `seen` holds the record keys that were already reported for this document.
/// The adapter must only emit events for records that are not in `seen`, and
/// must add the keys it reports.
pub fn parse_json_document(
    provider: Provider,
    doc: &Value,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    match provider {
        Provider::Continue => continue_dev::parse_document(doc, seen),
        Provider::RooCode => roo_code::parse_document(doc, seen),
        Provider::Cline => cline::parse_document(doc, seen),
        Provider::Kiro => kiro::parse_document(doc, seen),
        _ => Vec::new(),
    }
}

/// Parse a JSON lines file where a line can hold a snapshot of the whole
/// conversation.
///
/// `seen` holds the record keys that were already reported for this file.
pub fn parse_jsonl_snapshot(
    provider: Provider,
    text: &str,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    match provider {
        Provider::GeminiCli => gemini_cli::parse_snapshot(text, seen),
        _ => Vec::new(),
    }
}

/// Parse a JSON lines snapshot with the path of the session file.
pub fn parse_jsonl_snapshot_at(
    provider: Provider,
    path: &Path,
    text: &str,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    match provider {
        Provider::GeminiCli => gemini_cli::parse_snapshot_at(path, text, seen),
        _ => parse_jsonl_snapshot(provider, text, seen),
    }
}

/// Poll a provider database for rows that were written since the last poll.
///
/// `seen` holds the row keys that were already reported for this database.
pub fn poll_database(provider: Provider, db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    match provider {
        Provider::Goose => goose::poll(db, seen),
        Provider::Crush => crush::poll(db, seen),
        Provider::OpenCode => opencode::poll(db, seen),
        Provider::Zed => zed::poll(db, seen),
        Provider::KiloCode => kilo_code::poll(db, seen),
        _ => Vec::new(),
    }
}

/// Databases that hold the activity of the provider below the given root.
pub fn databases(provider: Provider, root: &Path) -> Vec<PathBuf> {
    match provider {
        Provider::Goose => goose::databases(root),
        Provider::Crush => crush::databases(root),
        Provider::OpenCode => opencode::databases(root),
        Provider::Zed => zed::databases(root),
        Provider::KiloCode => kilo_code::databases(root),
        _ => Vec::new(),
    }
}

/// Open a provider database for reading.
///
/// Provider databases are usually in WAL mode and are written by the harness
/// while Discodex reads them. A read-only handle can fail when the shared
/// memory file is missing, so fall back to a read-write handle that never
/// creates the database.
pub(crate) fn open_database(db: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let readonly = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    );
    let connection = match readonly {
        Ok(connection) => connection,
        Err(_) => rusqlite::Connection::open_with_flags(
            db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?,
    };
    let _ = connection.busy_timeout(std::time::Duration::from_millis(2_000));
    let _ = connection.pragma_update(None, "query_only", true);
    Ok(connection)
}

/// Read a column as text, regardless of the declared column type.
pub(crate) fn column_text(row: &rusqlite::Row<'_>, index: usize) -> Option<String> {
    match row.get_ref(index) {
        Ok(rusqlite::types::ValueRef::Text(bytes)) => {
            Some(String::from_utf8_lossy(bytes).to_string())
        }
        Ok(rusqlite::types::ValueRef::Integer(value)) => Some(value.to_string()),
        Ok(rusqlite::types::ValueRef::Real(value)) => Some(value.to_string()),
        Ok(rusqlite::types::ValueRef::Null) => None,
        Ok(rusqlite::types::ValueRef::Blob(bytes)) => {
            Some(String::from_utf8_lossy(bytes).to_string())
        }
        Err(_) => None,
    }
}

/// Read a column as text without failing when the column does not exist.
pub(crate) fn column_text_opt(row: &rusqlite::Row<'_>, name: &str) -> Option<String> {
    let index = row.as_ref().column_index(name).ok()?;
    column_text(row, index)
}

/// Read a column as a JSON value, parsing JSON text columns.
pub(crate) fn column_json(row: &rusqlite::Row<'_>, index: usize) -> Option<Value> {
    let text = column_text(row, index)?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod live_tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn newest_file(root: &Path, matches: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
        let mut files = Vec::new();
        collect(root, &mut files, 0);
        files.retain(|path| matches(path));
        files.sort_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });
        files.pop()
    }

    fn collect(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
        if depth > 5 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, files, depth + 1);
            } else {
                files.push(path);
            }
        }
    }

    fn home() -> PathBuf {
        dirs::home_dir().expect("home directory")
    }

    /// Parse the real session files of the installed harnesses.
    ///
    /// The test is ignored by default because it depends on the machine. Run it
    /// with `cargo test -- --ignored live_` to check the adapters against the
    /// sessions that are really on disk.
    #[test]
    #[ignore = "depends on the harnesses installed on this machine"]
    fn live_sessions_produce_events() {
        let mut checked = 0;

        if let Some(path) = newest_file(&home().join(".gemini").join("tmp"), &|path| {
            path.to_string_lossy().contains("chats")
                && path.extension().is_some_and(|ext| ext == "jsonl")
        }) {
            let text = std::fs::read_to_string(&path).expect("read session");
            let mut seen = HashSet::new();
            let events = gemini_cli::parse_snapshot(&text, &mut seen);
            println!("gemini: {} -> {} events", path.display(), events.len());
            assert!(!events.is_empty(), "gemini cli session produced no events");
            checked += 1;
        }

        if let Some(path) = newest_file(&home().join(".qwen").join("projects"), &|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
        }) {
            let text = std::fs::read_to_string(&path).expect("read session");
            let events = qwen_code::parse_line(text.lines().next().unwrap_or_default());
            println!("qwen: {} -> {} events", path.display(), events.len());
            assert!(!events.is_empty(), "qwen code session produced no events");
            checked += 1;
        }

        if let Some(path) = newest_file(&home().join(".factory").join("sessions"), &|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
        }) {
            let text = std::fs::read_to_string(&path).expect("read session");
            let events: Vec<ParsedEvent> = text.lines().flat_map(droid::parse_line).collect();
            println!("droid: {} -> {} events", path.display(), events.len());
            assert!(!events.is_empty(), "droid session produced no events");
            checked += 1;
        }

        if let Some(path) = newest_file(
            &home().join(".local").join("share").join("opencode"),
            &|path| path.extension().is_some_and(|ext| ext == "db"),
        ) {
            let mut seen = HashSet::new();
            let events = opencode::poll(&path, &mut seen);
            println!("opencode: {} -> {} events", path.display(), events.len());
            // An installation that never ran a session has no rows. That is
            // not a failure of the adapter.
            checked += 1;
        }

        assert!(checked > 0, "no harness session was found on this machine");
    }
}
