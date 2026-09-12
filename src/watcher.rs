use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::provider::{self, Provider};

#[derive(Debug, Default)]
struct Cursor {
    offset: u64,
    pending: String,
}

#[derive(Debug, Default)]
pub struct LogTailer {
    cursors: HashMap<PathBuf, Cursor>,
}

impl LogTailer {
    pub fn read_from_start(&mut self, path: &Path) -> io::Result<Vec<String>> {
        self.cursors.insert(path.to_path_buf(), Cursor::default());
        self.read_new(path)
    }

    pub fn read_new(&mut self, path: &Path) -> io::Result<Vec<String>> {
        let cursor = self.cursors.entry(path.to_path_buf()).or_default();
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        if length < cursor.offset {
            cursor.offset = 0;
            cursor.pending.clear();
        }
        if length == cursor.offset {
            return Ok(Vec::new());
        }

        file.seek(SeekFrom::Start(cursor.offset))?;
        let mut bytes = Vec::with_capacity((length - cursor.offset) as usize);
        file.read_to_end(&mut bytes)?;
        cursor.offset = length;

        let chunk = String::from_utf8_lossy(&bytes);
        let mut text = std::mem::take(&mut cursor.pending);
        text.push_str(&chunk);
        let ends_with_newline = text.ends_with('\n');
        let mut parts: Vec<&str> = text.split('\n').collect();
        if !ends_with_newline {
            if let Some(last) = parts.pop() {
                cursor.pending = last.to_string();
            }
        } else if parts.last() == Some(&"") {
            parts.pop();
        }

        Ok(parts
            .into_iter()
            .map(|line| line.trim_end_matches('\r').to_string())
            .filter(|line| !line.is_empty())
            .collect())
    }
}

pub fn recent_logs(root: &Path, provider: Provider, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_logs(root, provider, &mut files, 0);
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let keep_from = files.len().saturating_sub(limit);
    files.drain(keep_from..).collect()
}

fn collect_logs(dir: &Path, provider: Provider, files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if (name_str.starts_with('.') && name_str != ".system_generated")
                || name_str == "node_modules"
                || name_str == "target"
                || name_str == "scratch"
                || name_str == "steps"
                || name_str == "tasks"
            {
                continue;
            }
            collect_logs(&path, provider, files, depth + 1);
        } else if provider::is_session_log(provider, &path) {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tails_appends_rotation_and_incomplete_lines() {
        let dir = std::env::temp_dir().join(format!("discodex-tailer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-test.jsonl");
        fs::write(&path, "one\ntwo\npar").unwrap();

        let mut tailer = LogTailer::default();
        assert_eq!(tailer.read_from_start(&path).unwrap(), vec!["one", "two"]);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "tial\nthree\n").unwrap();
        file.flush().unwrap();
        assert_eq!(tailer.read_new(&path).unwrap(), vec!["partial", "three"]);
        drop(file);

        fs::write(&path, "rotated\n").unwrap();
        assert_eq!(tailer.read_new(&path).unwrap(), vec!["rotated"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_from_start_reads_initial_lines_in_large_logs() {
        let dir = std::env::temp_dir().join(format!("discodex-large-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.jsonl");

        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "{{\"step_index\":0,\"content\":\"/boost task\"}}").unwrap();
        // Write ~300KB of content
        let padding = "x".repeat(1000);
        for i in 1..=300 {
            writeln!(file, "{{\"step_index\":{i},\"data\":\"{padding}\"}}").unwrap();
        }
        file.flush().unwrap();
        drop(file);

        let mut tailer = LogTailer::default();
        let lines = tailer.read_from_start(&path).unwrap();
        assert_eq!(lines.len(), 301);
        assert_eq!(lines[0], "{\"step_index\":0,\"content\":\"/boost task\"}");

        let _ = fs::remove_dir_all(&dir);
    }
}
