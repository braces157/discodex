use crate::provider::{Provider, SessionSource, SessionStore};
use chrono::{Days, Local, TimeZone};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 200_000;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_SCAN_RECORDS: usize = 500_000;

#[derive(Debug, Clone, Default)]
pub struct UsageSnapshot {
    pub rows: Vec<UsageRow>,
    pub daily: Vec<UsageDay>,
    pub pricing_checked_at: String,
    pub scanned_files: usize,
    pub warnings: Vec<String>,
    pub refreshed_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UsageDay {
    pub date: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsagePeriod {
    Today,
    Week,
    Month,
    All,
}

impl UsagePeriod {
    pub fn since_ms(self, now_ms: i64) -> Option<i64> {
        let days = match self {
            Self::Today => 0,
            Self::Week => 6,
            Self::Month => 29,
            Self::All => return None,
        };
        let day = Local.timestamp_millis_opt(now_ms).single()?.date_naive();
        let midnight = day
            .checked_sub_days(Days::new(days))?
            .and_hms_opt(0, 0, 0)?;
        // Some time zones advance at midnight. Use the first valid local instant that day.
        (0..=180).find_map(|minutes| {
            Local
                .from_local_datetime(&(midnight + chrono::Duration::minutes(minutes)))
                .earliest()
                .map(|time| time.timestamp_millis())
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct UsageRow {
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub task_time_ms: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub sessions: usize,
    pub requests: u64,
    pub last_seen_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl TokenUsage {
    fn codex(value: &Value) -> Self {
        Self {
            input_tokens: number(value, "input_tokens"),
            cached_input_tokens: number(value, "cached_input_tokens"),
            output_tokens: number(value, "output_tokens"),
            reasoning_tokens: number(value, "reasoning_output_tokens"),
            cache_write_tokens: number(value, "cache_write_input_tokens"),
        }
    }

    fn claude(value: &Value) -> Self {
        let cached = number(value, "cache_read_input_tokens");
        let written = number(value, "cache_creation_input_tokens").or_else(|| {
            number(&value["cache_creation"], "ephemeral_5m_input_tokens")?.checked_add(number(
                &value["cache_creation"],
                "ephemeral_1h_input_tokens",
            )?)
        });
        Self {
            input_tokens: number(value, "input_tokens")
                .and_then(|n| n.checked_add(cached?))
                .and_then(|n| n.checked_add(written?)),
            cached_input_tokens: cached,
            cache_write_tokens: written,
            output_tokens: number(value, "output_tokens"),
            reasoning_tokens: number(value, "reasoning_output_tokens"),
        }
    }

    fn delta(self, previous: Self) -> Self {
        fn delta(current: Option<u64>, previous: Option<u64>) -> Option<u64> {
            current?.checked_sub(previous?)
        }
        Self {
            input_tokens: delta(self.input_tokens, previous.input_tokens),
            cached_input_tokens: delta(self.cached_input_tokens, previous.cached_input_tokens),
            cache_write_tokens: delta(self.cache_write_tokens, previous.cache_write_tokens),
            output_tokens: delta(self.output_tokens, previous.output_tokens),
            reasoning_tokens: delta(self.reasoning_tokens, previous.reasoning_tokens),
        }
    }

    fn add(self, other: Self) -> Self {
        fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            a?.checked_add(b?)
        }
        Self {
            input_tokens: add(self.input_tokens, other.input_tokens),
            cached_input_tokens: add(self.cached_input_tokens, other.cached_input_tokens),
            cache_write_tokens: add(self.cache_write_tokens, other.cache_write_tokens),
            output_tokens: add(self.output_tokens, other.output_tokens),
            reasoning_tokens: add(self.reasoning_tokens, other.reasoning_tokens),
        }
    }

    fn regresses(self, previous: Self) -> bool {
        [
            (self.input_tokens, previous.input_tokens),
            (self.cached_input_tokens, previous.cached_input_tokens),
            (self.cache_write_tokens, previous.cache_write_tokens),
            (self.output_tokens, previous.output_tokens),
            (self.reasoning_tokens, previous.reasoning_tokens),
        ]
        .iter()
        .any(|(a, b)| matches!((a,b), (Some(a),Some(b)) if a < b))
    }

    fn any(self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.reasoning_tokens.is_some()
    }

    fn valid(self) -> bool {
        !matches!((self.input_tokens,self.cached_input_tokens), (Some(i),Some(c)) if c > i)
            && !matches!((self.output_tokens,self.reasoning_tokens), (Some(o),Some(r)) if r > o)
            && !matches!((self.input_tokens,self.cached_input_tokens,self.cache_write_tokens),
                (Some(i),Some(c),Some(w)) if c.checked_add(w).is_none_or(|n| n > i))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Context {
    model: String,
    effort: String,
}

#[derive(Debug, Clone)]
enum RecordKind {
    CodexUsage {
        total: Option<TokenUsage>,
        last: Option<TokenUsage>,
    },
    CodexBaseline(TokenUsage),
    ClaudeUsage {
        id: String,
        usage: TokenUsage,
        write_1h: Option<u64>,
    },
    Start {
        id: String,
    },
    Complete {
        id: String,
    },
}

#[derive(Debug, Clone)]
struct Record {
    session: String,
    context: Context,
    time: i64,
    sequence: usize,
    forked: bool,
    kind: RecordKind,
}

#[derive(Default)]
struct ParsedFile {
    records: Vec<Record>,
    warnings: BTreeSet<String>,
}

#[derive(PartialEq, Eq)]
struct Fingerprint {
    bytes: u64,
    modified: Option<SystemTime>,
}

struct CachedFile {
    fingerprint: Fingerprint,
    parsed: ParsedFile,
}

pub struct UsageScanner {
    sources: Vec<SessionSource>,
    rates_path: PathBuf,
    cache: BTreeMap<(String, PathBuf), CachedFile>,
}

impl UsageScanner {
    pub fn new(sources: Vec<SessionSource>, rates_path: PathBuf) -> Self {
        Self {
            sources: expand_sources(sources),
            rates_path,
            cache: BTreeMap::new(),
        }
    }

    pub fn scan(&mut self) -> UsageSnapshot {
        self.scan_period(UsagePeriod::All)
    }

    pub fn scan_period(&mut self, period: UsagePeriod) -> UsageSnapshot {
        self.scan_at(period, now_ms())
    }

    fn scan_at(&mut self, period: UsagePeriod, refreshed_at_ms: i64) -> UsageSnapshot {
        let mut warnings = BTreeSet::from([
            "Coverage includes local Codex session/archive JSONL and Claude Code project JSONL only. Deleted logs are not retained; other providers and remote history are not included.".to_owned(),
            "Task time includes matched explicit task/turn starts and completions only; it is not active work time. Claude message timestamps do not measure task time.".to_owned(),
            "Costs are approximate standard API base-rate estimates, not subscription bills. Missing basic token categories and unknown model IDs remain unpriced. Long-context, speed/service-tier, region and unrecorded cache-write premiums are excluded.".to_owned(),
        ]);
        let mut files = BTreeMap::new();
        let mut entries = 0;
        for source in &self.sources {
            let Some(provider) = supported(source.provider) else {
                continue;
            };
            collect_files(
                &source.root,
                provider,
                &mut files,
                &mut entries,
                &mut warnings,
            );
        }
        if files.len() > MAX_FILES {
            warnings.insert(format!(
                "Coverage capped at {MAX_FILES} files in deterministic path order."
            ));
        }
        let mut used_bytes = 0u64;
        let mut retained = BTreeSet::new();
        let mut scanned_files = 0;
        let mut retained_records = 0usize;
        for (key, fingerprint) in files.into_iter().take(MAX_FILES) {
            if fingerprint.bytes > MAX_FILE_BYTES {
                warnings.insert(format!(
                    "Coverage excludes files larger than {} MiB.",
                    MAX_FILE_BYTES / 1024 / 1024
                ));
                continue;
            }
            used_bytes = used_bytes.saturating_add(fingerprint.bytes);
            if used_bytes > MAX_SCAN_BYTES {
                warnings.insert(
                    "Coverage capped at 2 GiB of source files in deterministic path order."
                        .to_owned(),
                );
                break;
            }
            if !self.cache.get(&key).is_some_and(|cached| {
                fingerprint.modified.is_some() && cached.fingerprint == fingerprint
            }) {
                match parse_file(&key.1, &key.0) {
                    Ok(parsed) => {
                        self.cache.insert(
                            key.clone(),
                            CachedFile {
                                fingerprint,
                                parsed,
                            },
                        );
                    }
                    Err(_) => {
                        warnings.insert(format!(
                            "{} source files could not be read; coverage is incomplete.",
                            key.0
                        ));
                        continue;
                    }
                }
            }
            retained_records =
                retained_records.saturating_add(self.cache[&key].parsed.records.len());
            if retained_records > MAX_SCAN_RECORDS {
                warnings.insert(format!("Coverage capped at {MAX_SCAN_RECORDS} usage/task metadata records in deterministic file order."));
                break;
            }
            retained.insert(key);
            scanned_files += 1;
        }
        self.cache.retain(|key, _| retained.contains(key));
        let mut records = BTreeMap::<String, Vec<(&str, &Record)>>::new();
        for ((provider, _), cached) in &self.cache {
            warnings.extend(cached.parsed.warnings.iter().cloned());
            for record in &cached.parsed.records {
                records
                    .entry(format!("{provider}/{}", record.session))
                    .or_default()
                    .push((provider, record));
            }
        }
        let rates = load_rates(&self.rates_path, &mut warnings);
        let mut totals = Totals {
            rows: BTreeMap::new(),
            daily: BTreeMap::new(),
            since: period.since_ms(refreshed_at_ms),
            rates: &rates,
        };
        for (session, mut records) in records {
            records.sort_by_key(|(_, r)| (r.time, r.sequence));
            aggregate_session(&session, &records, &mut totals, &mut warnings);
        }
        let rows = totals
            .rows
            .into_values()
            .map(|mut aggregate: Aggregate| {
                aggregate.row.sessions = aggregate.sessions.len();
                aggregate.row
            })
            .collect();
        UsageSnapshot {
            rows,
            daily: totals.daily.into_values().collect(),
            pricing_checked_at: "2026-10-29".to_owned(),
            scanned_files,
            warnings: warnings.into_iter().collect(),
            refreshed_at_ms,
        }
    }
}

fn supported(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Codex => Some("codex"),
        Provider::ClaudeCode => Some("claude-code"),
        _ => None,
    }
}

fn expand_sources(sources: Vec<SessionSource>) -> Vec<SessionSource> {
    let mut expanded = Vec::new();
    for mut source in sources {
        if source.provider == Provider::Codex {
            let is_default =
                dirs::home_dir().is_some_and(|home| source.root == home.join(".codex/sessions"));
            if is_default
                && let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty())
            {
                source.root = PathBuf::from(home).join("sessions");
            }
            if source.root.file_name().is_some_and(|s| s == "sessions") {
                let mut archive = source.clone();
                archive.root.set_file_name("archived_sessions");
                expanded.push(archive);
            }
        }
        if source.provider == Provider::ClaudeCode {
            let is_default =
                dirs::home_dir().is_some_and(|home| source.root == home.join(".claude/projects"));
            if is_default
                && let Some(home) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty())
            {
                source.root = PathBuf::from(home).join("projects");
            }
        }
        if supported(source.provider).is_some() && source.store == SessionStore::LogFiles {
            expanded.push(source);
        }
    }
    expanded.sort_by(|a, b| a.root.cmp(&b.root));
    expanded.dedup_by(|a, b| a.provider == b.provider && a.root == b.root);
    expanded
}

fn collect_files(
    root: &Path,
    provider: &str,
    files: &mut BTreeMap<(String, PathBuf), Fingerprint>,
    entries: &mut usize,
    warnings: &mut BTreeSet<String>,
) {
    if !root.exists() {
        warnings.insert(format!(
            "{provider} source directory is missing; local coverage may be incomplete."
        ));
        return;
    }
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let children = match fs::read_dir(directory) {
            Ok(children) => children,
            Err(_) => {
                warnings.insert(format!(
                    "{provider} directories could not be read; coverage is incomplete."
                ));
                continue;
            }
        };
        let limit = MAX_ENTRIES.saturating_sub(*entries) + 1;
        let mut ordered = BTreeMap::new();
        for entry in children {
            match entry {
                Ok(entry) => {
                    ordered.insert(entry.path(), entry);
                    if ordered.len() > limit {
                        ordered.pop_last();
                    }
                }
                Err(_) => {
                    warnings.insert(format!(
                        "{provider} directory entries could not be read; coverage is incomplete."
                    ));
                }
            }
        }
        for child in ordered.into_values() {
            *entries += 1;
            if *entries > MAX_ENTRIES {
                warnings.insert(format!(
                    "Coverage traversal capped at {MAX_ENTRIES} directory entries."
                ));
                return;
            }
            let Ok(kind) = child.file_type() else {
                warnings.insert(format!(
                    "{provider} file metadata could not be read; coverage is incomplete."
                ));
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(child.path());
            }
            if kind.is_file()
                && child
                    .path()
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("jsonl"))
            {
                // Windows directory entries can carry stale sizes while a log writer is open.
                match fs::metadata(child.path()) {
                    Ok(meta) => {
                        files.insert(
                            (provider.to_owned(), child.path()),
                            Fingerprint {
                                bytes: meta.len(),
                                modified: meta.modified().ok(),
                            },
                        );
                    }
                    Err(_) => {
                        warnings.insert(format!(
                            "{provider} file metadata could not be read; coverage is incomplete."
                        ));
                    }
                }
            }
        }
    }
}

fn parse_file(path: &Path, provider: &str) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let mut reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let mut buffer = Vec::new();
    let mut session = None;
    let mut context = Context::default();
    let mut sequence = 0;
    let mut forked = false;
    let mut fork_time = None;
    loop {
        buffer.clear();
        let read = reader
            .by_ref()
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        sequence += 1;
        if buffer.len() > MAX_LINE_BYTES {
            parsed.warnings.insert(format!(
                "{provider} lines larger than 2 MiB were skipped; coverage is incomplete."
            ));
            while !buffer.ends_with(b"\n") {
                buffer.clear();
                if reader
                    .by_ref()
                    .take((MAX_LINE_BYTES + 1) as u64)
                    .read_until(b'\n', &mut buffer)?
                    == 0
                {
                    break;
                }
            }
            continue;
        }
        let value: Value = match serde_json::from_slice(&buffer) {
            Ok(value) => value,
            Err(_) => {
                parsed.warnings.insert(format!("{provider} malformed or unfinished JSONL records were skipped; coverage is incomplete."));
                continue;
            }
        };
        let time = value.get("timestamp").and_then(timestamp_ms).unwrap_or(0);
        let kind = if provider == "codex" {
            let payload = &value["payload"];
            match value["type"].as_str().unwrap_or("") {
                "session_meta" => {
                    if let Some(id) = identity(&payload["id"]) {
                        session = Some(id);
                    }
                    forked = identity(&payload["forked_from_id"]).is_some();
                    fork_time = payload
                        .get("timestamp")
                        .and_then(timestamp_ms)
                        .or((time > 0).then_some(time));
                    if forked {
                        parsed.warnings.insert("codex forked history excludes inherited records before the fork timestamp. The first post-fork cumulative record requires a baseline or explicit last-request usage; otherwise its attribution remains unknown.".to_owned());
                    }
                    None
                }
                "turn_context" => {
                    context = Context {
                        model: label(&payload["model"], 128),
                        effort: label(
                            payload
                                .get("effort")
                                .or_else(|| payload.get("reasoning_effort"))
                                .or_else(|| {
                                    payload.pointer("/collaboration_mode/settings/reasoning_effort")
                                })
                                .unwrap_or(&Value::Null),
                            32,
                        ),
                    };
                    None
                }
                "event_msg" => match payload["type"].as_str().unwrap_or("") {
                    "token_count" if payload["info"].is_object() => {
                        let info = &payload["info"];
                        Some(RecordKind::CodexUsage {
                            total: info
                                .get("total_token_usage")
                                .filter(|v| v.is_object())
                                .map(TokenUsage::codex),
                            last: info
                                .get("last_token_usage")
                                .filter(|v| v.is_object())
                                .map(TokenUsage::codex),
                        })
                    }
                    "task_started" | "turn_started" => Some(RecordKind::Start {
                        id: task_id(payload),
                    }),
                    "task_complete" | "task_completed" | "turn_complete" | "turn_completed" => {
                        Some(RecordKind::Complete {
                            id: task_id(payload),
                        })
                    }
                    _ => None,
                },
                _ => None,
            }
        } else {
            if let Some(id) = identity(&value["sessionId"]) {
                session = Some(id);
            }
            if value["type"] == "assistant" && value["message"]["usage"].is_object() {
                context = Context {
                    model: label(&value["message"]["model"], 128),
                    effort: label(
                        value
                            .get("effort")
                            .or_else(|| value.pointer("/message/effort"))
                            .unwrap_or(&Value::Null),
                        32,
                    ),
                };
                match identity(&value["message"]["id"]) {
                    Some(id) => {
                        let usage = TokenUsage::claude(&value["message"]["usage"]);
                        let details = &value["message"]["usage"]["cache_creation"];
                        let write_1h =
                            number(details, "ephemeral_5m_input_tokens").and_then(|five| {
                                let hour = number(details, "ephemeral_1h_input_tokens")?;
                                (five.checked_add(hour) == usage.cache_write_tokens).then_some(hour)
                            });
                        Some(RecordKind::ClaudeUsage {
                            id,
                            usage,
                            write_1h,
                        })
                    }
                    None => {
                        parsed.warnings.insert("claude-code assistant usage without message IDs was skipped because it cannot be deduplicated safely.".to_owned());
                        None
                    }
                }
            } else {
                None
            }
        };
        if let Some(kind) = kind {
            let Some(session) = &session else {
                parsed.warnings.insert(format!("{provider} records without a session ID were excluded because sessions and duplicated usage cannot be identified safely."));
                continue;
            };
            if parsed.records.len() >= MAX_RECORDS {
                parsed.warnings.insert(format!("{provider} metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                break;
            }
            let event_time = if matches!(kind, RecordKind::Start { .. }) {
                value
                    .pointer("/payload/started_at")
                    .and_then(timestamp_ms)
                    .unwrap_or(time)
            } else {
                time
            };
            let kind = if forked
                && fork_time.is_some_and(|boundary| event_time > 0 && event_time < boundary)
            {
                match kind {
                    RecordKind::CodexUsage {
                        total: Some(total), ..
                    } => RecordKind::CodexBaseline(total),
                    _ => continue,
                }
            } else if forked && (fork_time.is_none() || event_time <= 0) {
                parsed.warnings.insert("codex fork records without a reliable creation/event timestamp were excluded because inherited usage cannot be attributed safely.".to_owned());
                continue;
            } else {
                kind
            };
            parsed.records.push(Record {
                session: session.clone(),
                context: context.clone(),
                time: event_time,
                sequence,
                forked,
                kind,
            });
        }
    }
    Ok(parsed)
}

fn label(value: &Value, max: usize) -> String {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= max
                && !s.starts_with('/')
                && s.as_bytes().get(1) != Some(&b':')
                && !s.contains("://")
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.:/@".contains(&c))
        })
        .unwrap_or("")
        .to_owned()
}

fn identity(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .map(str::to_owned)
}

fn task_id(payload: &Value) -> String {
    identity(&payload["turn_id"])
        .or_else(|| identity(&payload["task_id"]))
        .unwrap_or_default()
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    if let Some(s) = value.as_str() {
        return chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|date| date.timestamp_millis());
    }
    let n = value.as_i64()?;
    if n < 0 {
        None
    } else if n < 100_000_000_000 {
        n.checked_mul(1000)
    } else {
        Some(n)
    }
}

#[derive(Default)]
struct Aggregate {
    row: UsageRow,
    sessions: BTreeSet<String>,
    has_usage: bool,
}

type Rows = BTreeMap<(String, String, String), Aggregate>;

struct Totals<'a> {
    rows: Rows,
    daily: BTreeMap<String, UsageDay>,
    since: Option<i64>,
    rates: &'a BTreeMap<(String, String), Rate>,
}

fn aggregate_session(
    session: &str,
    records: &[(&str, &Record)],
    totals: &mut Totals<'_>,
    warnings: &mut BTreeSet<String>,
) {
    let mut previous = None;
    let mut seen_totals = BTreeSet::new();
    let mut seen_last = BTreeSet::new();
    let mut claude = BTreeMap::new();
    let mut pending = HashMap::<String, &Record>::new();
    let mut seen_tasks = BTreeSet::new();
    for &(provider, record) in records {
        match &record.kind {
            RecordKind::CodexUsage { total, last } => {
                let usage = if let Some(total) = total.filter(|t| t.any()) {
                    if let Some(last) = last {
                        seen_last.insert((record.time, *last));
                    }
                    if !seen_totals.insert((record.time, total)) || previous == Some(total) {
                        continue;
                    }
                    let usage = match previous {
                        Some(before) if total.regresses(before) => {
                            warnings.insert("codex cumulative counters decreased. Only explicit last-request usage was counted for that reset; missing categories remain unknown.".to_owned());
                            last.unwrap_or_default()
                        }
                        Some(before) => total.delta(before),
                        None if record.forked => last.unwrap_or_default(),
                        None => total,
                    };
                    previous = Some(total);
                    usage
                } else if let Some(last) = last {
                    if record.time == 0 || !seen_last.insert((record.time, *last)) {
                        warnings.insert("codex last-request usage without a distinct timestamp could not be deduplicated safely.".to_owned());
                        continue;
                    }
                    previous = Some(previous.map_or(*last, |before| before.add(*last)));
                    *last
                } else {
                    continue;
                };
                add_usage(totals, provider, session, record, usage, None, warnings);
            }
            RecordKind::CodexBaseline(total) => {
                previous = Some(*total);
            }
            RecordKind::ClaudeUsage {
                id,
                usage,
                write_1h,
            } => {
                claude.insert(id, (provider, record, *usage, *write_1h));
            }
            RecordKind::Start { id } => {
                if record.time > 0
                    && pending
                        .insert(id.clone(), record)
                        .is_some_and(|old| old.time != record.time)
                {
                    warnings.insert(
                        "Superseded incomplete task/turn starts were excluded from task time."
                            .to_owned(),
                    );
                }
            }
            RecordKind::Complete { id } => {
                if let Some(start) = pending.remove(id)
                    && record.time >= start.time
                    && seen_tasks.insert((id.clone(), start.time, record.time))
                {
                    let start_time = start.time.max(totals.since.unwrap_or(start.time));
                    if record.time < start_time {
                        continue;
                    }
                    let context = &record.context;
                    let aggregate =
                        row_for(&mut totals.rows, provider, session, context, record.time);
                    aggregate.row.task_time_ms = aggregate
                        .row
                        .task_time_ms
                        .unwrap_or(0)
                        .checked_add((record.time - start_time) as u64);
                }
            }
        }
    }
    if !pending.is_empty() {
        warnings.insert("Incomplete task/turn intervals were excluded from task time.".to_owned());
    }
    for (_, (provider, record, usage, write_1h)) in claude {
        add_usage(totals, provider, session, record, usage, write_1h, warnings);
    }
}

fn row_for<'a>(
    rows: &'a mut Rows,
    provider: &str,
    session: &str,
    context: &Context,
    time: i64,
) -> &'a mut Aggregate {
    let key = (
        provider.to_owned(),
        context.model.clone(),
        context.effort.clone(),
    );
    let aggregate = rows.entry(key).or_insert_with(|| Aggregate {
        row: UsageRow {
            provider: provider.to_owned(),
            model: context.model.clone(),
            effort: context.effort.clone(),
            ..UsageRow::default()
        },
        ..Aggregate::default()
    });
    aggregate.sessions.insert(session.to_owned());
    aggregate.row.last_seen_ms = aggregate.row.last_seen_ms.max(time);
    aggregate
}

fn add_usage(
    totals: &mut Totals<'_>,
    provider: &str,
    session: &str,
    record: &Record,
    usage: TokenUsage,
    write_1h: Option<u64>,
    warnings: &mut BTreeSet<String>,
) {
    if record.time <= 0 {
        warnings.insert(
            "Usage without a reliable timestamp is excluded from bounded periods and daily charts."
                .to_owned(),
        );
        if totals.since.is_some() {
            return;
        }
    }
    if totals.since.is_some_and(|since| record.time < since) {
        return;
    }
    if !usage.valid() {
        warnings.insert(format!(
            "{provider} invalid token subsets were skipped; coverage is incomplete."
        ));
        return;
    }
    if usage.input_tokens.is_none()
        || usage.output_tokens.is_none()
        || usage.cached_input_tokens.is_none()
    {
        warnings.insert(format!("{provider} records have incomplete token categories; affected totals and estimates remain unknown."));
    }
    let cost = totals
        .rates
        .get(&(provider.to_owned(), record.context.model.clone()))
        .and_then(|rate| estimate_usage(usage, write_1h, rate, warnings));
    let aggregate = row_for(
        &mut totals.rows,
        provider,
        session,
        &record.context,
        record.time,
    );
    fn sum(target: &mut Option<u64>, value: Option<u64>, first: bool) {
        *target = if first {
            value
        } else {
            target.and_then(|a| a.checked_add(value?))
        };
    }
    sum(
        &mut aggregate.row.input_tokens,
        usage.input_tokens,
        !aggregate.has_usage,
    );
    sum(
        &mut aggregate.row.cached_input_tokens,
        usage.cached_input_tokens,
        !aggregate.has_usage,
    );
    sum(
        &mut aggregate.row.cache_write_tokens,
        usage.cache_write_tokens,
        !aggregate.has_usage,
    );
    sum(
        &mut aggregate.row.output_tokens,
        usage.output_tokens,
        !aggregate.has_usage,
    );
    sum(
        &mut aggregate.row.reasoning_tokens,
        usage.reasoning_tokens,
        !aggregate.has_usage,
    );
    aggregate.row.estimated_cost_usd =
        sum_cost(aggregate.row.estimated_cost_usd, cost, !aggregate.has_usage);
    aggregate.row.requests = aggregate.row.requests.saturating_add(1);
    aggregate.has_usage = true;
    if record.time > 0
        && let Some(date) = Local.timestamp_millis_opt(record.time).single()
    {
        let date = date.format("%Y-%m-%d").to_string();
        let day = totals
            .daily
            .entry(date.clone())
            .or_insert_with(|| UsageDay {
                date,
                ..UsageDay::default()
            });
        let first = day.requests == 0;
        sum(&mut day.input_tokens, usage.input_tokens, first);
        sum(&mut day.output_tokens, usage.output_tokens, first);
        sum(
            &mut day.cached_input_tokens,
            usage.cached_input_tokens,
            first,
        );
        day.estimated_cost_usd = sum_cost(day.estimated_cost_usd, cost, first);
        day.requests = day.requests.saturating_add(1);
    }
}

fn sum_cost(previous: Option<f64>, cost: Option<f64>, first: bool) -> Option<f64> {
    let sum = if first { cost? } else { previous? + cost? };
    sum.is_finite().then_some(sum)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rate {
    provider: String,
    model: String,
    input_per_million: f64,
    cached_input_per_million: f64,
    #[serde(default)]
    cache_write_per_million: Option<f64>,
    output_per_million: f64,
    #[serde(skip)]
    builtin: bool,
    #[serde(default)]
    cache_write_1h_per_million: Option<f64>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RateFile {
    #[serde(default)]
    rates: Vec<Rate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    checked_at: String,
    openai_source: String,
    claude_source: String,
    rates: Vec<Rate>,
}

pub fn ensure_rates_file(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = match File::options().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    file.write_all(b"# Optional overrides for bundled standard API base rates, USD per million tokens.\n# Built-in official prices were checked 2026-10-29. Unknown exact model IDs stay unpriced.\n# Sources: https://developers.openai.com/api/docs/pricing\n# https://platform.claude.com/docs/en/about-claude/pricing\n# Estimates are not subscription bills. Context, speed, region and unrecorded write premiums are excluded.\n# Provider must be exactly codex or claude-code. Model must match the displayed ID.\n# Cached input and cache writes are subsets of inclusive input.\n# A custom cache-write rate is an effective price and overrides automatic TTL pricing.\n# Uncomment and replace each placeholder with a finite nonnegative number.\n# [[rates]]\n# provider = \"codex\"\n# model = \"exact-model-id\"\n# input_per_million = YOUR_RATE\n# cached_input_per_million = YOUR_RATE\n# output_per_million = YOUR_RATE\n# cache_write_per_million = YOUR_RATE # required for nonzero writes\n")
}

fn load_rates(path: &Path, warnings: &mut BTreeSet<String>) -> BTreeMap<(String, String), Rate> {
    let catalog: Catalog = toml::from_str(include_str!("model_prices.toml"))
        .expect("bundled official model prices must be valid TOML");
    debug_assert_eq!(catalog.checked_at, "2026-10-29");
    debug_assert!(
        catalog
            .openai_source
            .starts_with("https://developers.openai.com/")
    );
    debug_assert!(
        catalog
            .claude_source
            .starts_with("https://platform.claude.com/")
    );
    let mut rates = BTreeMap::new();
    for mut rate in catalog.rates {
        rate.builtin = true;
        rates.insert((rate.provider.clone(), rate.model.clone()), rate);
    }
    rates.extend(load_custom_rates(path, warnings));
    rates
}

fn load_custom_rates(
    path: &Path,
    warnings: &mut BTreeSet<String>,
) -> BTreeMap<(String, String), Rate> {
    let mut rates = BTreeMap::new();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return rates,
        Err(_) => {
            warnings
                .insert("Custom rates could not be read. Bundled rates remain in use.".to_owned());
            return rates;
        }
    };
    let mut text = String::new();
    if file.take(65_537).read_to_string(&mut text).is_err() || text.len() > 65_536 {
        warnings.insert(
            "Custom rates exceed 64 KiB or are unreadable. Bundled rates remain in use.".to_owned(),
        );
        return rates;
    }
    let parsed = match toml::from_str::<RateFile>(&text) {
        Ok(parsed) => parsed,
        Err(_) => {
            warnings
                .insert("Custom rates TOML is invalid. Bundled rates remain in use.".to_owned());
            return rates;
        }
    };
    let mut duplicates = BTreeSet::new();
    for rate in parsed.rates {
        let valid = [
            rate.input_per_million,
            rate.cached_input_per_million,
            rate.output_per_million,
        ]
        .into_iter()
        .chain(rate.cache_write_per_million)
        .chain(rate.cache_write_1h_per_million)
        .all(|n| n.is_finite() && n >= 0.0);
        if !valid
            || !matches!(rate.provider.as_str(), "codex" | "claude-code")
            || label(&Value::String(rate.model.clone()), 128).is_empty()
        {
            warnings.insert("Invalid custom rates were ignored. Rates must use exact supported provider/model IDs and finite nonnegative prices.".to_owned());
            continue;
        }
        let key = (rate.provider.clone(), rate.model.clone());
        if rates.insert(key.clone(), rate).is_some() {
            duplicates.insert(key);
        }
    }
    for key in duplicates {
        rates.remove(&key);
        warnings.insert(
            "Duplicate provider/model rates were ignored to avoid ambiguous estimates.".to_owned(),
        );
    }
    rates
}

fn estimate_usage(
    usage: TokenUsage,
    write_1h: Option<u64>,
    rate: &Rate,
    warnings: &mut BTreeSet<String>,
) -> Option<f64> {
    let input = usage.input_tokens?;
    let cached = usage.cached_input_tokens?;
    let output = usage.output_tokens?;
    let written = match usage.cache_write_tokens {
        Some(written) => written,
        None if rate.provider == "codex" => {
            if !rate.builtin
                || rate
                    .cache_write_per_million
                    .is_some_and(|price| price > rate.input_per_million)
            {
                warnings.insert("Codex cache-write counts are missing. Base-rate approximation excludes unrecorded write premiums, long-context, speed/service-tier and region premiums.".to_owned());
            }
            0
        }
        None => return None,
    };
    let uncached = input.checked_sub(cached)?.checked_sub(written)?;
    let write_cost = if written == 0 {
        0.0
    } else {
        let default_price = rate.cache_write_per_million?;
        if rate.builtin && rate.provider == "claude-code" {
            if let Some(hours) = write_1h.filter(|hours| *hours <= written) {
                (written - hours) as f64 * default_price
                    + hours as f64 * rate.cache_write_1h_per_million?
            } else {
                warnings.insert("Claude cache-write lifetimes are missing or incomplete. Base-rate approximation uses 5-minute writes and excludes additional long-write, context, speed and region premiums.".to_owned());
                written as f64 * default_price
            }
        } else {
            if rate.builtin && default_price > rate.input_per_million {
                warnings.insert("OpenAI cache writes use the default 30-minute API base rate. Longer write lifetimes, long-context, speed/service-tier and region premiums are excluded.".to_owned());
            }
            written as f64 * default_price
        }
    };
    let cost = (uncached as f64 * rate.input_per_million
        + cached as f64 * rate.cached_input_per_million
        + write_cost
        + output as f64 * rate.output_per_million)
        / 1_000_000.0;
    cost.is_finite().then_some(cost)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "discodex-usage-{}-{}-{}",
                std::process::id(),
                now_ms(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, values: &[Value]) {
            let text = values
                .iter()
                .map(|v| serde_json::to_string(v).unwrap() + "\n")
                .collect::<String>();
            fs::write(self.0.join(name), text).unwrap();
        }
        fn scanner(&self, provider: Provider) -> UsageScanner {
            UsageScanner::new(
                vec![SessionSource {
                    provider,
                    root: self.0.clone(),
                    store: SessionStore::LogFiles,
                }],
                self.0.join("rates.toml"),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn context(model: &str, effort: &str) -> Value {
        json!({"type":"turn_context","payload":{"model":model,"effort":effort}})
    }
    fn total(time: &str, input: u64, output: u64) -> Value {
        json!({"timestamp":time,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":input/2,"cache_write_input_tokens":0,"output_tokens":output,"reasoning_output_tokens":output/2}}}})
    }
    fn meta() -> Value {
        json!({"type":"session_meta","payload":{"id":"synthetic-session"}})
    }
    fn claude(id: &str, output: u64) -> Value {
        json!({"type":"assistant","sessionId":"synthetic-claude","timestamp":"2026-01-01T00:00:00Z","message":{"id":id,"model":"test-model","usage":{"input_tokens":100,"cache_read_input_tokens":200,"cache_creation_input_tokens":50,"output_tokens":output}}})
    }

    #[test]
    fn cumulative_deltas_dedup_and_model_switch_survive_refresh_restart_and_deletion() {
        let f = Fixture::new();
        let values = vec![
            meta(),
            context("model-a", "high"),
            total("2026-01-01T00:00:01Z", 100, 20),
            total("2026-01-01T00:00:02Z", 100, 20),
            context("model-b", "low"),
            total("2026-01-01T00:00:03Z", 160, 40),
        ];
        f.write("one.jsonl", &values);
        f.write("duplicate.jsonl", &values);
        let mut scanner = f.scanner(Provider::Codex);
        for snapshot in [
            scanner.scan(),
            scanner.scan(),
            f.scanner(Provider::Codex).scan(),
        ] {
            assert_eq!(snapshot.scanned_files, 2);
            assert_eq!(snapshot.rows.len(), 2);
            assert_eq!(snapshot.rows[0].input_tokens, Some(100));
            assert_eq!(snapshot.rows[0].output_tokens, Some(20));
            assert_eq!(snapshot.rows[1].input_tokens, Some(60));
            assert_eq!(snapshot.rows[1].output_tokens, Some(20));
            assert_eq!(snapshot.rows[1].sessions, 1);
        }
        fs::remove_file(f.0.join("one.jsonl")).unwrap();
        fs::remove_file(f.0.join("duplicate.jsonl")).unwrap();
        assert!(scanner.scan().rows.is_empty());
    }

    #[test]
    fn claude_message_revisions_replace_instead_of_add_and_normalize_cache_input() {
        let f = Fixture::new();
        f.write(
            "one.jsonl",
            &[
                claude("message-a", 1),
                claude("message-a", 30),
                claude("message-b", 20),
            ],
        );
        let snapshot = f.scanner(Provider::ClaudeCode).scan();
        let row = &snapshot.rows[0];
        assert_eq!(row.input_tokens, Some(700));
        assert_eq!(row.cached_input_tokens, Some(400));
        assert_eq!(row.cache_write_tokens, Some(100));
        assert_eq!(row.output_tokens, Some(50));
        assert_eq!(row.reasoning_tokens, None);
        assert_eq!(row.task_time_ms, None);
    }

    #[test]
    fn only_matched_explicit_intervals_measure_task_time() {
        let f = Fixture::new();
        f.write("one.jsonl",&[meta(),
            json!({"timestamp":"2026-01-01T00:00:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"one"}}),
            context("test-model","high"),
            json!({"timestamp":"2026-01-01T00:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"wrong"}}),
            json!({"timestamp":"2026-01-01T00:00:07Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"one"}}),
            json!({"timestamp":"2026-01-01T00:00:09Z","type":"event_msg","payload":{"type":"task_started","turn_id":"unfinished"}}),
        ]);
        let snapshot = f.scanner(Provider::Codex).scan();
        assert_eq!(snapshot.rows[0].task_time_ms, Some(7000));
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("Incomplete task"))
        );
    }

    #[test]
    fn unknown_fields_and_malformed_records_stay_unknown_without_prose() {
        let f = Fixture::new();
        f.write("one.jsonl",&[meta(),context("not a model containing private prose","high"),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"output_tokens":10}}}})]);
        let mut file = File::options()
            .append(true)
            .open(f.0.join("one.jsonl"))
            .unwrap();
        file.write_all(b"{unfinished\n").unwrap();
        let snapshot = f.scanner(Provider::Codex).scan();
        assert_eq!(snapshot.rows[0].input_tokens, None);
        assert_eq!(snapshot.rows[0].output_tokens, Some(10));
        assert!(snapshot.rows[0].model.is_empty());
        assert_eq!(snapshot.rows[0].estimated_cost_usd, None);
        assert!(snapshot.warnings.iter().any(|w| w.contains("malformed")));
        assert!(!format!("{snapshot:?}").contains("private prose"));
        assert!(!format!("{snapshot:?}").contains(&f.0.to_string_lossy().to_string()));
    }

    #[test]
    fn path_and_url_shaped_model_metadata_never_reaches_the_snapshot() {
        let fixture = Fixture::new();
        for model in [
            "/home/test-user/private-project",
            "C:/Users/test-user/private-project",
            "//server/private-project",
            "https://private.example/project",
        ] {
            fixture.write(
                "one.jsonl",
                &[
                    meta(),
                    context(model, "high"),
                    total("2026-01-01T00:00:01Z", 100, 20),
                ],
            );
            let snapshot = fixture.scanner(Provider::Codex).scan();
            assert!(snapshot.rows[0].model.is_empty(), "{model}");
            assert!(!format!("{snapshot:?}").contains(model));
        }
        assert_eq!(label(&json!("openai/gpt-test"), 128), "openai/gpt-test");
    }

    #[test]
    fn custom_rates_are_exact_and_math_excludes_cache_subsets() {
        let f = Fixture::new();
        f.write("one.jsonl", &[claude("a", 100)]);
        fs::write(f.0.join("rates.toml"),"[[rates]]\nprovider='claude-code'\nmodel='test-model'\ninput_per_million=2.0\ncached_input_per_million=0.5\ncache_write_per_million=3.0\noutput_per_million=10.0\n").unwrap();
        let mut scanner = f.scanner(Provider::ClaudeCode);
        assert!((scanner.scan().rows[0].estimated_cost_usd.unwrap() - 0.00145).abs() < 1e-12);
        fs::write(f.0.join("rates.toml"),"[[rates]]\nprovider='claude-code'\nmodel='other-model'\ninput_per_million=2.0\ncached_input_per_million=0.5\noutput_per_million=10.0\n").unwrap();
        assert_eq!(scanner.scan().rows[0].estimated_cost_usd, None);
    }

    #[test]
    fn invalid_prices_and_duplicate_rates_never_produce_costs() {
        let f = Fixture::new();
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("test-model", ""),
                total("2026-01-01T00:00:01Z", 100, 20),
            ],
        );
        let mut scanner = f.scanner(Provider::Codex);
        for price in ["-1.0", "nan", "inf"] {
            fs::write(f.0.join("rates.toml"),format!("[[rates]]\nprovider='codex'\nmodel='test-model'\ninput_per_million={price}\ncached_input_per_million=0.0\noutput_per_million=0.0\n")).unwrap();
            assert_eq!(scanner.scan().rows[0].estimated_cost_usd, None);
        }
        let rate = "[[rates]]\nprovider='codex'\nmodel='test-model'\ninput_per_million=1.0\ncached_input_per_million=0.0\noutput_per_million=0.0\n";
        fs::write(f.0.join("rates.toml"), format!("{rate}{rate}")).unwrap();
        assert_eq!(scanner.scan().rows[0].estimated_cost_usd, None);
    }

    #[test]
    fn counter_reset_uses_explicit_last_usage_and_does_not_invent_delta() {
        let f = Fixture::new();
        let mut reset = total("2026-01-01T00:00:03Z", 10, 4);
        reset["payload"]["info"]["last_token_usage"] = json!({"input_tokens":10,"cached_input_tokens":5,"output_tokens":4,"reasoning_output_tokens":2});
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("test-model", "high"),
                total("2026-01-01T00:00:01Z", 100, 20),
                reset,
            ],
        );
        assert_eq!(
            f.scanner(Provider::Codex).scan().rows[0].input_tokens,
            Some(110)
        );
    }

    #[test]
    fn rates_template_is_valid_empty_toml_and_never_overwrites() {
        let f = Fixture::new();
        let path = f.0.join("rates.toml");
        ensure_rates_file(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(toml::from_str::<RateFile>(&text).unwrap().rates.is_empty());
        fs::write(&path, "# retained").unwrap();
        ensure_rates_file(&path).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "# retained");
    }

    #[test]
    fn forks_exclude_inherited_counters_and_use_their_baseline() {
        let f = Fixture::new();
        let mut fork = meta();
        fork["payload"]["forked_from_id"] = json!("synthetic-parent");
        fork["payload"]["timestamp"] = json!("2026-01-01T00:00:02Z");
        f.write(
            "one.jsonl",
            &[
                fork.clone(),
                context("test-model", "high"),
                total("2026-01-01T00:00:01Z", 100, 20),
                total("2026-01-01T00:00:03Z", 160, 40),
            ],
        );
        let row = &f.scanner(Provider::Codex).scan().rows[0];
        assert_eq!(row.input_tokens, Some(60));
        assert_eq!(row.output_tokens, Some(20));
        let mut first = total("2026-01-01T00:00:03Z", 160, 40);
        first["payload"]["info"]["last_token_usage"] = json!({"input_tokens":10,"cached_input_tokens":5,"cache_write_input_tokens":0,"output_tokens":4,"reasoning_output_tokens":2});
        f.write("one.jsonl", &[fork, context("test-model", "high"), first]);
        assert_eq!(
            f.scanner(Provider::Codex).scan().rows[0].input_tokens,
            Some(10)
        );
    }

    #[test]
    fn repeated_counter_values_after_a_reset_still_count_new_usage() {
        let f = Fixture::new();
        let mut reset = total("2026-01-01T00:00:02Z", 10, 4);
        reset["payload"]["info"]["last_token_usage"] = json!({"input_tokens":10,"cached_input_tokens":5,"cache_write_input_tokens":0,"output_tokens":4,"reasoning_output_tokens":2});
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("test-model", "high"),
                total("2026-01-01T00:00:01Z", 100, 20),
                reset,
                total("2026-01-01T00:00:03Z", 100, 20),
            ],
        );
        assert_eq!(
            f.scanner(Provider::Codex).scan().rows[0].input_tokens,
            Some(200)
        );
    }

    #[test]
    fn missing_writes_allow_disclosed_base_estimates_but_missing_basic_categories_do_not() {
        let f = Fixture::new();
        let mut legacy = total("2026-01-01T00:00:01Z", 100, 20);
        legacy["payload"]["info"]["total_token_usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
        f.write(
            "one.jsonl",
            &[meta(), context("test-model", "high"), legacy],
        );
        fs::write(f.0.join("rates.toml"),"[[rates]]\nprovider='codex'\nmodel='test-model'\ninput_per_million=2.0\ncached_input_per_million=0.5\noutput_per_million=10.0\n").unwrap();
        let snapshot = f.scanner(Provider::Codex).scan();
        let row = &snapshot.rows[0];
        assert_eq!(row.input_tokens, Some(100));
        assert_eq!(row.output_tokens, Some(20));
        assert_eq!(row.cache_write_tokens, None);
        assert!((row.estimated_cost_usd.unwrap() - 0.000325).abs() < 1e-12);
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("cache-write counts are missing"))
        );
        let mut incomplete = claude("a", 10);
        incomplete["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_read_input_tokens");
        f.write("one.jsonl", &[claude("b", 20), incomplete]);
        let snapshot = f.scanner(Provider::ClaudeCode).scan();
        assert_eq!(snapshot.rows[0].input_tokens, None);
        assert_eq!(snapshot.rows[0].output_tokens, Some(30));
        assert_eq!(snapshot.rows[0].estimated_cost_usd, None);
    }

    #[test]
    fn oversized_lines_skip_to_the_next_record_without_leaking_content() {
        let f = Fixture::new();
        f.write("one.jsonl", &[meta(), context("test-model", "high")]);
        let mut file = File::options()
            .append(true)
            .open(f.0.join("one.jsonl"))
            .unwrap();
        file.write_all(&vec![b'x'; MAX_LINE_BYTES + 10]).unwrap();
        file.write_all(b"\n").unwrap();
        writeln!(file, "{}", total("2026-01-01T00:00:01Z", 100, 20)).unwrap();
        let snapshot = f.scanner(Provider::Codex).scan();
        assert_eq!(snapshot.rows[0].input_tokens, Some(100));
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("larger than 2 MiB"))
        );
    }

    #[test]
    fn archive_sources_are_scanned_and_changed_content_is_rebuilt() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("sessions")).unwrap();
        fs::create_dir(f.0.join("archived_sessions")).unwrap();
        f.write(
            "archived_sessions/one.jsonl",
            &[
                meta(),
                context("test-model", "high"),
                total("2026-01-01T00:00:01Z", 100, 20),
            ],
        );
        let mut scanner = UsageScanner::new(
            vec![SessionSource {
                provider: Provider::Codex,
                root: f.0.join("sessions"),
                store: SessionStore::LogFiles,
            }],
            f.0.join("rates.toml"),
        );
        assert_eq!(scanner.scan().rows[0].input_tokens, Some(100));
        let mut file = File::options()
            .append(true)
            .open(f.0.join("archived_sessions/one.jsonl"))
            .unwrap();
        writeln!(file, "{}", total("2026-01-01T00:00:02Z", 180, 40)).unwrap();
        assert_eq!(scanner.scan().rows[0].input_tokens, Some(180));
    }

    #[test]
    fn records_without_session_ids_do_not_invent_session_counts() {
        let f = Fixture::new();
        f.write(
            "one.jsonl",
            &[
                context("test-model", "high"),
                total("2026-01-01T00:00:01Z", 100, 20),
            ],
        );
        let snapshot = f.scanner(Provider::Codex).scan();
        assert!(snapshot.rows.is_empty());
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("without a session ID"))
        );
    }

    #[test]
    fn last_request_only_events_and_later_cumulative_counters_do_not_double_count() {
        let f = Fixture::new();
        let mut last_only = total("2026-01-01T00:00:01Z", 100, 20);
        let usage = last_only["payload"]["info"]["total_token_usage"].take();
        last_only["payload"]["info"]["last_token_usage"] = usage;
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("test-model", "high"),
                last_only,
                total("2026-01-01T00:00:02Z", 160, 40),
            ],
        );
        let row = &f.scanner(Provider::Codex).scan().rows[0];
        assert_eq!(row.input_tokens, Some(160));
        assert_eq!(row.output_tokens, Some(40));
    }

    fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn at_ms(mut event: Value, time: i64) -> Value {
        event["timestamp"] = json!(time);
        event
    }

    #[test]
    fn periods_use_inclusive_local_calendar_days() {
        let now = local_ms(2026, 1, 8, 0, 1);
        assert_eq!(
            UsagePeriod::Today.since_ms(now),
            Some(local_ms(2026, 1, 8, 0, 0))
        );
        assert_eq!(
            UsagePeriod::Week.since_ms(now),
            Some(local_ms(2026, 1, 2, 0, 0))
        );
        assert_eq!(
            UsagePeriod::Month.since_ms(now),
            Some(local_ms(2025, 12, 10, 0, 0))
        );
        assert_eq!(UsagePeriod::All.since_ms(now), None);
        assert_eq!(
            UsagePeriod::Today.since_ms(local_ms(2026, 1, 7, 23, 59)),
            Some(local_ms(2026, 1, 7, 0, 0))
        );
    }

    #[test]
    fn period_selection_reconciles_baselines_before_cutoff_and_clips_intervals() {
        let f = Fixture::new();
        let before = local_ms(2026, 1, 7, 23, 59);
        let after = local_ms(2026, 1, 8, 0, 1);
        let values = vec![
            meta(),
            context("gpt-6.1-sol", "high"),
            at_ms(total("", 100, 20), before),
            json!({"timestamp":before,"type":"event_msg","payload":{"type":"task_started","turn_id":"a"}}),
            at_ms(total("", 160, 40), after),
            json!({"timestamp":after,"type":"event_msg","payload":{"type":"task_complete","turn_id":"a"}}),
        ];
        f.write("one.jsonl", &values);
        f.write("duplicate.jsonl", &values);
        let mut scanner = f.scanner(Provider::Codex);
        let today = scanner.scan_at(UsagePeriod::Today, after);
        assert_eq!(today.rows.len(), 1);
        assert_eq!(today.rows[0].input_tokens, Some(60));
        assert_eq!(today.rows[0].output_tokens, Some(20));
        assert_eq!(today.rows[0].task_time_ms, Some(60_000));
        assert_eq!(today.rows[0].requests, 1);
        assert_eq!(today.daily.len(), 1);
        assert_eq!(today.daily[0].date, "2026-01-08");
        assert_eq!(today.daily[0].input_tokens, Some(60));
        let all = scanner.scan();
        assert_eq!(all.rows[0].requests, 2);
        assert_eq!(all.rows[0].input_tokens, Some(160));
        assert_eq!(all.daily.len(), 2);
        assert_eq!(all.rows[0].task_time_ms, Some(120_000));
    }

    #[test]
    fn models_and_days_share_observations_and_cost_without_double_counting_subsets() {
        let f = Fixture::new();
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("gpt-6.1-sol", "high"),
                at_ms(total("", 100, 20), local_ms(2026, 1, 1, 12, 0)),
                context("gpt-6.1-terra", "low"),
                at_ms(total("", 160, 40), local_ms(2026, 1, 2, 12, 0)),
            ],
        );
        let snapshot = f.scanner(Provider::Codex).scan();
        assert_eq!(snapshot.rows.len(), 2);
        assert_eq!(snapshot.rows.iter().map(|r| r.requests).sum::<u64>(), 2);
        assert_eq!(snapshot.daily.iter().map(|d| d.requests).sum::<u64>(), 2);
        assert_eq!(snapshot.daily[0].input_tokens, Some(100));
        assert_eq!(snapshot.daily[1].input_tokens, Some(60));
        let cost: f64 = snapshot
            .rows
            .iter()
            .map(|r| r.estimated_cost_usd.unwrap())
            .sum();
        assert!((cost - 0.0004365).abs() < 1e-12);
        assert!(
            (snapshot
                .daily
                .iter()
                .map(|d| d.estimated_cost_usd.unwrap())
                .sum::<f64>()
                - cost)
                .abs()
                < 1e-12
        );
        assert_eq!(snapshot.pricing_checked_at, "2026-10-29");
    }

    #[test]
    fn official_prices_missing_writes_old_models_exact_overrides_and_unknown_ids() {
        let f = Fixture::new();
        let mut event = total("2026-01-01T00:00:01Z", 100, 20);
        event["payload"]["info"]["total_token_usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
        f.write(
            "one.jsonl",
            &[meta(), context("gpt-6.1-sol", ""), event.clone()],
        );
        let mut scanner = f.scanner(Provider::Codex);
        let snapshot = scanner.scan();
        assert!((snapshot.rows[0].estimated_cost_usd.unwrap() - 0.000305).abs() < 1e-12);
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("unrecorded write premiums"))
        );
        fs::write(f.0.join("rates.toml"), "[[rates]]\nprovider='codex'\nmodel='gpt-6.1-sol'\ninput_per_million=1.0\ncached_input_per_million=0.0\noutput_per_million=0.0\n").unwrap();
        assert!((scanner.scan().rows[0].estimated_cost_usd.unwrap() - 0.00005).abs() < 1e-12);
        f.write(
            "one.jsonl",
            &[meta(), context("gpt-6.1-sol-unverified", ""), event.clone()],
        );
        assert_eq!(scanner.scan().rows[0].estimated_cost_usd, None);
        f.write("one.jsonl", &[meta(), context("gpt-5.4", ""), event]);
        let old = scanner.scan();
        assert!((old.rows[0].estimated_cost_usd.unwrap() - 0.0004375).abs() < 1e-12);
        assert!(
            !old.warnings
                .iter()
                .any(|w| w.contains("Codex cache-write counts"))
        );
    }

    #[test]
    fn writes_are_charged_once_and_claude_recorded_ttls_beat_default_but_not_custom_rates() {
        let f = Fixture::new();
        let mut event = total("2026-01-01T00:00:01Z", 100, 20);
        event["payload"]["info"]["total_token_usage"]["cache_write_input_tokens"] = json!(20);
        f.write("one.jsonl", &[meta(), context("gpt-6.1-sol", ""), event]);
        assert!(
            (f.scanner(Provider::Codex).scan().rows[0]
                .estimated_cost_usd
                .unwrap()
                - 0.000315)
                .abs()
                < 1e-12
        );
        let mut event = claude("one", 100);
        event["message"]["model"] = json!("claude-sonnet-4-6");
        event["message"]["usage"]["cache_creation"] =
            json!({"ephemeral_5m_input_tokens":20,"ephemeral_1h_input_tokens":30});
        f.write("one.jsonl", &[event.clone()]);
        let mut scanner = f.scanner(Provider::ClaudeCode);
        let snapshot = scanner.scan();
        assert!((snapshot.rows[0].estimated_cost_usd.unwrap() - 0.002115).abs() < 1e-12);
        assert_eq!(snapshot.rows[0].requests, 1);
        fs::write(f.0.join("rates.toml"), "[[rates]]\nprovider='claude-code'\nmodel='claude-sonnet-4-6'\ninput_per_million=3.0\ncached_input_per_million=0.3\noutput_per_million=15.0\ncache_write_per_million=4.0\n").unwrap();
        assert!((scanner.scan().rows[0].estimated_cost_usd.unwrap() - 0.00206).abs() < 1e-12);
        fs::remove_file(f.0.join("rates.toml")).unwrap();
        event["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_creation");
        f.write("one.jsonl", &[event]);
        let default = scanner.scan();
        assert!((default.rows[0].estimated_cost_usd.unwrap() - 0.0020475).abs() < 1e-12);
        assert!(
            default
                .warnings
                .iter()
                .any(|w| w.contains("uses 5-minute writes"))
        );
    }

    #[test]
    fn missing_timestamps_are_not_assigned_to_today_or_daily_charts() {
        let f = Fixture::new();
        let mut event = total("", 100, 20);
        event.as_object_mut().unwrap().remove("timestamp");
        f.write("one.jsonl", &[meta(), context("gpt-6.1-sol", ""), event]);
        let mut scanner = f.scanner(Provider::Codex);
        let today = scanner.scan_at(UsagePeriod::Today, local_ms(2026, 1, 8, 12, 0));
        assert!(today.rows.is_empty());
        assert!(today.daily.is_empty());
        let all = scanner.scan();
        assert_eq!(all.rows[0].requests, 1);
        assert!(all.daily.is_empty());
        assert!(
            all.warnings
                .iter()
                .any(|w| w.contains("without a reliable timestamp"))
        );
    }
}
