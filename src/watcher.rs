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
    collect_logs(root, provider, &mut files);
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let keep_from = files.len().saturating_sub(limit);
    files.drain(keep_from..).collect()
}

fn collect_logs(dir: &Path, provider: Provider, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_logs(&path, provider, files);
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
}
