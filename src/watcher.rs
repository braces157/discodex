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
        let bytes = if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zstd"))
        {
            let (decoded, consumed) = decode_complete_frames(&bytes)?;
            cursor.offset += consumed as u64;
            decoded
        } else {
            cursor.offset = length;
            bytes
        };

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

// Harness appends independently checksummed Zstandard frames, not one continuous
// stream. Commit only complete frames, leaving a write in progress for the next
// notification. The cursor counts compressed bytes, including across restarts.
fn decode_complete_frames(bytes: &[u8]) -> io::Result<(Vec<u8>, usize)> {
    const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
    let mut consumed = 0;
    let mut decoded = Vec::new();
    while consumed < bytes.len() {
        let remaining = &bytes[consumed..];
        if remaining.len() >= 4 && remaining[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Zstandard frame",
            ));
        }
        let Ok(frame_len) = zstd::zstd_safe::find_frame_compressed_size(remaining) else {
            // A partial trailing frame is common while the provider is writing.
            break;
        };
        let frame = zstd::bulk::decompress(&remaining[..frame_len], MAX_FRAME_BYTES)?;
        decoded.extend_from_slice(&frame);
        consumed += frame_len;
    }
    Ok((decoded, consumed))
}

pub fn recent_logs(root: &Path, provider: Provider, limit: usize) -> Vec<PathBuf> {
    recent_logs_at_depth(root, provider, limit, 5)
}

/// Session logs below the root, with a bounded directory depth.
///
/// Aider writes its transcript into the working tree of a repository, so the
/// scan is kept shallow and only a few known workspace folders are searched.
pub fn recent_logs_at_depth(
    root: &Path,
    provider: Provider,
    limit: usize,
    max_depth: usize,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_logs(root, provider, &mut files, 0, max_depth);
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let keep_from = files.len().saturating_sub(limit);
    files.drain(keep_from..).collect()
}

/// Session documents that the provider rewrites in place.
pub fn recent_documents(root: &Path, provider: Provider, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_documents(root, provider, &mut files, 0);
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    let keep_from = files.len().saturating_sub(limit);
    files.drain(keep_from..).collect()
}

/// Collect rewritten session documents.
///
/// Unlike the log scan this keeps `tasks` folders, because editor extensions
/// store one session document per task.
fn collect_documents(dir: &Path, provider: Provider, files: &mut Vec<PathBuf>, depth: usize) {
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
            {
                continue;
            }
            collect_documents(&path, provider, files, depth + 1);
        } else if provider::is_session_document(provider, &path) {
            files.push(path);
        }
    }
}

fn collect_logs(
    dir: &Path,
    provider: Provider,
    files: &mut Vec<PathBuf>,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
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
            collect_logs(&path, provider, files, depth + 1, max_depth);
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
    fn tails_concatenated_zstd_frames_partial_writes_and_rotation() {
        let dir = std::env::temp_dir().join(format!("discodex-zstd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.v4.jsonl.zstd");
        let first = zstd::stream::encode_all(&b"header\none\n"[..], 0).unwrap();
        let second = zstd::stream::encode_all(&b"two\npar"[..], 0).unwrap();
        let third = zstd::stream::encode_all(&b"tial\nthree\n"[..], 0).unwrap();
        let split = second.len() / 2;
        fs::write(&path, [&first[..], &second[..split]].concat()).unwrap();
        let mut tailer = LogTailer::default();
        assert_eq!(
            tailer.read_from_start(&path).unwrap(),
            vec!["header", "one"]
        );
        assert!(tailer.read_new(&path).unwrap().is_empty());
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&second[split..]).unwrap();
        file.flush().unwrap();
        assert_eq!(tailer.read_new(&path).unwrap(), vec!["two"]);
        file.write_all(&third).unwrap();
        file.flush().unwrap();
        assert_eq!(tailer.read_new(&path).unwrap(), vec!["partial", "three"]);
        assert!(tailer.read_new(&path).unwrap().is_empty());
        drop(file);
        fs::write(&path, zstd::stream::encode_all(&b"new\n"[..], 0).unwrap()).unwrap();
        assert_eq!(tailer.read_new(&path).unwrap(), vec!["new"]);
        assert_eq!(tailer.read_from_start(&path).unwrap(), vec!["new"]);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn zstd_rejects_corrupt_or_oversized_frames_without_advancing() {
        assert!(decode_complete_frames(b"not a frame").is_err());
        let large = zstd::stream::encode_all(&vec![b'x'; 16 * 1024 * 1024 + 1][..], 0).unwrap();
        assert!(decode_complete_frames(&large).is_err());
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 0).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.write_all(b"line\n").unwrap();
        let mut frame = encoder.finish().unwrap();
        *frame.last_mut().unwrap() ^= 1;
        assert!(decode_complete_frames(&frame).is_err());
    }

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
