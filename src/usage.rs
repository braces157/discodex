use crate::provider::{Provider, SessionSource, SessionStore};
use chrono::{Days, Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 200_000;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_SCAN_RECORDS: usize = 500_000;
const MAX_ZED_BLOB_BYTES: usize = 32 * 1024 * 1024;
const AIDER_MAX_DIR_DEPTH: usize = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub rows: Vec<UsageRow>,
    pub daily: Vec<UsageDay>,
    pub daily_by_provider: BTreeMap<String, Vec<UsageDay>>,
    pub pricing_checked_at: String,
    pub scanned_files: usize,
    pub warnings: Vec<String>,
    pub refreshed_at_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageBundle {
    pub today: UsageSnapshot,
    pub week: UsageSnapshot,
    pub month: UsageSnapshot,
    pub all: UsageSnapshot,
}

impl UsageBundle {
    /// Returns the snapshot for the requested period.
    pub fn for_period(&self, period: UsagePeriod) -> &UsageSnapshot {
        match period {
            UsagePeriod::Today => &self.today,
            UsagePeriod::Week => &self.week,
            UsagePeriod::Month => &self.month,
            UsagePeriod::All => &self.all,
        }
    }

    /// Returns the timestamp at which this bundle was refreshed.
    pub fn refreshed_at_ms(&self) -> i64 {
        self.week.refreshed_at_ms
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageDay {
    pub date: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
            reasoning_tokens: number(value, "reasoning_output_tokens").or_else(|| {
                value
                    .pointer("/output_tokens_details/thinking_tokens")
                    .and_then(Value::as_u64)
            }),
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
        request_id: Option<String>,
        usage: TokenUsage,
        write_1h: Option<u64>,
    },
    MessageUsage {
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

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ClaudeResponseId<'a> {
    Request(&'a str, &'a str),
    Session(&'a str, &'a str),
}

struct ClaudeResponse<'a> {
    first: &'a Record,
    revision: &'a Record,
}

impl<'a> ClaudeResponse<'a> {
    fn observe(&mut self, record: &'a Record) {
        if record.time > 0 && (self.first.time <= 0 || record.time < self.first.time) {
            self.first = record;
        }
        let RecordKind::ClaudeUsage {
            usage, write_1h, ..
        } = &record.kind
        else {
            return;
        };
        let RecordKind::ClaudeUsage {
            usage: previous,
            write_1h: previous_write_1h,
            ..
        } = &self.revision.kind
        else {
            return;
        };
        let quality = |usage: TokenUsage, write_1h: Option<u64>| {
            (
                usage.any() && usage.valid(),
                [
                    usage.input_tokens,
                    usage.cached_input_tokens,
                    usage.cache_write_tokens,
                    usage.output_tokens,
                    usage.reasoning_tokens,
                    write_1h,
                ]
                .iter()
                .filter(|value| value.is_some())
                .count(),
            )
        };
        let current_quality = quality(*usage, *write_1h);
        let previous_quality = quality(*previous, *previous_write_1h);
        let order = current_quality.cmp(&previous_quality);
        let replace = if current_quality.0 != previous_quality.0 {
            current_quality.0
        } else if usage.regresses(*previous) != previous.regresses(*usage) {
            previous.regresses(*usage)
        } else {
            order.is_gt()
                || (order.is_eq()
                    && (record.time, record.sequence)
                        > (self.revision.time, self.revision.sequence))
        };
        if replace {
            self.revision = record;
        }
    }

    fn canonical_record(self) -> Record {
        Record {
            session: self.first.session.clone(),
            time: self.first.time,
            ..self.revision.clone()
        }
    }
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
    extra_bytes: u64,
    extra_modified: Option<SystemTime>,
}

struct DiscoveredSource {
    fingerprint: Fingerprint,
    root: PathBuf,
    store: SessionStore,
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

struct PreparedScan {
    base_warnings: BTreeSet<String>,
    scanned_files: usize,
    pricing_checked_at: String,
    rates: RateIndex,
}

impl UsageScanner {
    pub fn new(sources: Vec<SessionSource>, rates_path: PathBuf) -> Self {
        Self {
            sources: expand_sources(sources),
            rates_path,
            cache: BTreeMap::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scan(&mut self) -> UsageSnapshot {
        self.scan_period(UsagePeriod::All)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scan_period(&mut self, period: UsagePeriod) -> UsageSnapshot {
        self.scan_at(period, now_ms())
    }

    /// Scans all four usage periods (`Today`, `Week`, `Month`, `All`) in a single pass.
    pub fn scan_all_periods(&mut self) -> UsageBundle {
        self.scan_all_periods_at(now_ms(), None)
            .expect("uncancelled usage scan must produce a bundle")
    }

    /// Scans all four usage periods in a single pass, aborting early if `cancel` is set.
    pub fn scan_all_periods_cancellable(&mut self, cancel: &AtomicBool) -> Option<UsageBundle> {
        self.scan_all_periods_at(now_ms(), Some(cancel))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn scan_at(&mut self, period: UsagePeriod, refreshed_at_ms: i64) -> UsageSnapshot {
        let prepared = self
            .prepare_scan(None)
            .expect("uncancelled usage scan must prepare");
        let claude_records = reconcile_claude(&self.cache);
        let sessions = collect_sorted_sessions(&self.cache, &claude_records);
        aggregate_period(
            &sessions,
            period,
            refreshed_at_ms,
            prepared.scanned_files,
            &prepared.pricing_checked_at,
            &prepared.rates,
            prepared.base_warnings,
        )
    }

    fn scan_all_periods_at(
        &mut self,
        refreshed_at_ms: i64,
        cancel: Option<&AtomicBool>,
    ) -> Option<UsageBundle> {
        let prepared = self.prepare_scan(cancel)?;
        if is_cancelled(cancel) {
            return None;
        }
        let claude_records = reconcile_claude(&self.cache);
        let sessions = collect_sorted_sessions(&self.cache, &claude_records);
        let periods = [
            UsagePeriod::Today,
            UsagePeriod::Week,
            UsagePeriod::Month,
            UsagePeriod::All,
        ];
        let mut snapshots = Vec::with_capacity(periods.len());
        for period in periods {
            if is_cancelled(cancel) {
                return None;
            }
            snapshots.push(aggregate_period(
                &sessions,
                period,
                refreshed_at_ms,
                prepared.scanned_files,
                &prepared.pricing_checked_at,
                &prepared.rates,
                prepared.base_warnings.clone(),
            ));
        }
        let mut iter = snapshots.into_iter();
        Some(UsageBundle {
            today: iter.next().unwrap_or_default(),
            week: iter.next().unwrap_or_default(),
            month: iter.next().unwrap_or_default(),
            all: iter.next().unwrap_or_default(),
        })
    }

    fn prepare_scan(&mut self, cancel: Option<&AtomicBool>) -> Option<PreparedScan> {
        let mut base_warnings = BTreeSet::from([
            "Coverage includes local session logs and databases for supported coding harnesses only. Deleted logs, remote history, and focused-window-only apps are not included.".to_owned(),
            "Task time includes matched explicit task/turn starts and completions only; it is not active work time. Message-only stores do not measure task time.".to_owned(),
            "Costs are approximate standard API base-rate estimates, not subscription bills. Missing basic token categories and unknown model IDs remain unpriced. Long-context, speed/service-tier, time-of-day, region and unrecorded cache-write premiums are excluded.".to_owned(),
        ]);
        let mut files = BTreeMap::new();
        let mut entries = 0;
        for source in &self.sources {
            if is_cancelled(cancel) {
                return None;
            }
            let Some(provider) = supported(source.provider) else {
                continue;
            };
            collect_source(
                source,
                provider,
                &mut files,
                &mut entries,
                &mut base_warnings,
            );
        }
        if files.len() > MAX_FILES {
            base_warnings.insert(format!(
                "Coverage capped at {MAX_FILES} files in deterministic path order."
            ));
        }
        let mut used_bytes = 0u64;
        let mut retained = BTreeSet::new();
        let mut scanned_files = 0;
        let mut retained_records = 0usize;
        for (key, discovered) in files.into_iter().take(MAX_FILES) {
            if is_cancelled(cancel) {
                return None;
            }
            let fingerprint = discovered.fingerprint;
            if fingerprint.bytes > MAX_FILE_BYTES {
                base_warnings.insert(format!(
                    "Coverage excludes files larger than {} MiB.",
                    MAX_FILE_BYTES / 1024 / 1024
                ));
                continue;
            }
            used_bytes = used_bytes.saturating_add(fingerprint.bytes);
            if used_bytes > MAX_SCAN_BYTES {
                base_warnings.insert(
                    "Coverage capped at 2 GiB of source files in deterministic path order."
                        .to_owned(),
                );
                break;
            }
            if !self.cache.get(&key).is_some_and(|cached| {
                fingerprint.modified.is_some() && cached.fingerprint == fingerprint
            }) {
                match parse_source(&key.1, &discovered.root, &key.0, discovered.store) {
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
                        base_warnings.insert(format!(
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
                base_warnings.insert(format!("Coverage capped at {MAX_SCAN_RECORDS} usage/task metadata records in deterministic file order."));
                break;
            }
            retained.insert(key);
            scanned_files += 1;
        }
        self.cache.retain(|key, _| retained.contains(key));
        for cached in self.cache.values() {
            base_warnings.extend(cached.parsed.warnings.iter().cloned());
        }
        let (pricing_checked_at, rates) = load_rates(&self.rates_path, &mut base_warnings);
        Some(PreparedScan {
            base_warnings,
            scanned_files,
            pricing_checked_at,
            rates,
        })
    }
}

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

fn reconcile_claude(cache: &BTreeMap<(String, PathBuf), CachedFile>) -> Vec<Record> {
    let mut claude = BTreeMap::<ClaudeResponseId<'_>, ClaudeResponse<'_>>::new();
    for cached in cache.values() {
        for record in &cached.parsed.records {
            if let RecordKind::ClaudeUsage { id, request_id, .. } = &record.kind {
                let identity = match request_id {
                    Some(request) => ClaudeResponseId::Request(id, request),
                    None => ClaudeResponseId::Session(&record.session, id),
                };
                claude
                    .entry(identity)
                    .or_insert(ClaudeResponse {
                        first: record,
                        revision: record,
                    })
                    .observe(record);
            }
        }
    }
    claude
        .into_values()
        .map(ClaudeResponse::canonical_record)
        .collect()
}

fn collect_sorted_sessions<'a>(
    cache: &'a BTreeMap<(String, PathBuf), CachedFile>,
    claude_records: &'a [Record],
) -> Vec<(String, Vec<(&'a str, &'a Record)>)> {
    let mut records = BTreeMap::<String, Vec<(&'a str, &'a Record)>>::new();
    for ((provider, _), cached) in cache {
        for record in &cached.parsed.records {
            if matches!(record.kind, RecordKind::ClaudeUsage { .. }) {
                continue;
            }
            records
                .entry(format!("{provider}/{}", record.session))
                .or_default()
                .push((provider, record));
        }
    }
    for record in claude_records {
        records
            .entry(format!("claude-code/{}", record.session))
            .or_default()
            .push(("claude-code", record));
    }
    records
        .into_iter()
        .map(|(session, mut recs)| {
            recs.sort_by_key(|(_, r)| (r.time, r.sequence));
            (session, recs)
        })
        .collect()
}

fn aggregate_period(
    sessions: &[(String, Vec<(&str, &Record)>)],
    period: UsagePeriod,
    refreshed_at_ms: i64,
    scanned_files: usize,
    pricing_checked_at: &str,
    rates: &RateIndex,
    mut warnings: BTreeSet<String>,
) -> UsageSnapshot {
    let mut totals = Totals {
        rows: BTreeMap::new(),
        daily: BTreeMap::new(),
        daily_by_provider: BTreeMap::new(),
        since: period.since_ms(refreshed_at_ms),
        rates,
    };
    for (session, records) in sessions {
        aggregate_session(session, records, &mut totals, &mut warnings);
    }
    let rows = totals
        .rows
        .into_values()
        .map(|mut aggregate: Aggregate| {
            aggregate.row.sessions = aggregate.sessions.len();
            aggregate.row
        })
        .collect();
    let daily_by_provider = totals
        .daily_by_provider
        .into_iter()
        .map(|(provider, map)| (provider, map.into_values().collect()))
        .collect();
    UsageSnapshot {
        rows,
        daily: totals.daily.into_values().collect(),
        daily_by_provider,
        pricing_checked_at: pricing_checked_at.to_owned(),
        scanned_files,
        warnings: warnings.into_iter().collect(),
        refreshed_at_ms,
    }
}

fn supported(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Codex => Some("codex"),
        Provider::ClaudeCode => Some("claude-code"),
        Provider::Antigravity => Some("antigravity"),
        Provider::DeepSeekHarness => Some("deepseek-harness"),
        Provider::GeminiCli => Some("gemini-cli"),
        Provider::QwenCode => Some("qwen-code"),
        Provider::OpenCode => Some("opencode"),
        Provider::KiloCode => Some("kilo-code"),
        Provider::FactoryDroid => Some("factory-droid"),
        Provider::CopilotCli => Some("copilot-cli"),
        Provider::JetBrains => Some("junie"),
        Provider::Cline => Some("cline"),
        Provider::RooCode => Some("roo-code"),
        Provider::Zed => Some("zed"),
        Provider::Crush => Some("crush"),
        Provider::Aider => Some("aider"),
        _ => None,
    }
}

fn is_supported_provider_slug(slug: &str) -> bool {
    matches!(
        slug,
        "codex"
            | "claude-code"
            | "antigravity"
            | "deepseek-harness"
            | "gemini-cli"
            | "qwen-code"
            | "opencode"
            | "kilo-code"
            | "factory-droid"
            | "copilot-cli"
            | "junie"
            | "cline"
            | "roo-code"
            | "zed"
            | "crush"
            | "aider"
    )
}

fn store_order(store: SessionStore) -> u8 {
    match store {
        SessionStore::LogFiles => 0,
        SessionStore::JsonFiles => 1,
        SessionStore::JsonLineSnapshot => 2,
        SessionStore::Sqlite => 3,
    }
}

fn absolutize_path(path: PathBuf, base_dir: Option<&Path>) -> PathBuf {
    let joined = if path.is_relative() {
        match base_dir {
            Some(base) => base.join(&path),
            None => path,
        }
    } else {
        path
    };
    let mut normalized = PathBuf::new();
    let mut has_root = false;
    for component in joined.components() {
        match component {
            std::path::Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
            }
            std::path::Component::RootDir => {
                normalized.push(component.as_os_str());
                has_root = true;
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|c| matches!(c, std::path::Component::Normal(_)));
                if can_pop {
                    normalized.pop();
                } else if !has_root {
                    normalized.push(component.as_os_str());
                }
            }
            std::path::Component::Normal(part) => {
                normalized.push(part);
            }
        }
    }
    normalized
}

fn expand_sources(sources: Vec<SessionSource>) -> Vec<SessionSource> {
    let cwd = std::env::current_dir().ok();
    expand_sources_with_base(sources, cwd.as_deref())
}

fn expand_sources_with_base(
    sources: Vec<SessionSource>,
    base_dir: Option<&Path>,
) -> Vec<SessionSource> {
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
            source.root = absolutize_path(source.root, base_dir);
            if source.root.file_name().is_some_and(|s| s == "sessions") {
                let mut archive = source.clone();
                archive.root.set_file_name("archived_sessions");
                expanded.push(archive);
            }
        } else {
            if source.provider == Provider::ClaudeCode {
                let is_default = dirs::home_dir()
                    .is_some_and(|home| source.root == home.join(".claude/projects"));
                if is_default
                    && let Some(home) =
                        std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty())
                {
                    source.root = PathBuf::from(home).join("projects");
                }
            }
            source.root = absolutize_path(source.root, base_dir);
        }
        if supported(source.provider).is_some() {
            expanded.push(source);
        }
    }
    expanded.sort_by(|a, b| {
        a.root
            .cmp(&b.root)
            .then_with(|| supported(a.provider).cmp(&supported(b.provider)))
            .then_with(|| store_order(a.store).cmp(&store_order(b.store)))
    });
    expanded.dedup_by(|a, b| a.provider == b.provider && a.store == b.store && a.root == b.root);
    expanded
}

fn normalized_root_key(path: &Path) -> String {
    let mut s: String = path
        .to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' => '\\',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// Returns additional expanded non-Aider usage roots (such as Codex archives or env overrides)
/// that are read by `UsageScanner` beyond the base `sources` list.
pub fn extra_usage_watch_roots(sources: &[SessionSource]) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().ok();
    extra_usage_watch_roots_with_base(sources, cwd.as_deref())
}

fn extra_usage_watch_roots_with_base(
    sources: &[SessionSource],
    base_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut already_watched = HashSet::new();
    for source in sources {
        if source.provider == Provider::Aider {
            continue;
        }
        already_watched.insert(normalized_root_key(&source.root));
        already_watched.insert(normalized_root_key(&absolutize_path(
            source.root.clone(),
            base_dir,
        )));
    }
    let mut extras = Vec::new();
    let mut seen = HashSet::new();
    for source in expand_sources_with_base(sources.to_vec(), base_dir) {
        if source.provider == Provider::Aider {
            continue;
        }
        let key = normalized_root_key(&source.root);
        if !already_watched.contains(&key) && seen.insert(key) {
            extras.push(source.root);
        }
    }
    extras
}

/// Returns a stable 128-bit hex digest identifying the expanded usage sources and rates path
/// without exposing raw local filesystem paths in cached metadata.
pub fn usage_scope_key(sources: &[SessionSource], rates_path: &Path) -> String {
    let cwd = std::env::current_dir().ok();
    usage_scope_key_with_base(sources, rates_path, cwd.as_deref())
}

fn usage_scope_key_with_base(
    sources: &[SessionSource],
    rates_path: &Path,
    base_dir: Option<&Path>,
) -> String {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut feed_bytes = |tag: u8, bytes: &[u8]| {
        feed_fnv1a(&mut h1, &[tag]);
        feed_fnv1a(&mut h1, &(bytes.len() as u64).to_le_bytes());
        feed_fnv1a(&mut h1, bytes);
        feed_fnv1a(&mut h2, &[tag ^ 0xa5]);
        feed_fnv1a(&mut h2, &(bytes.len() as u64).to_be_bytes());
        for &byte in bytes {
            feed_fnv1a(&mut h2, &[byte.wrapping_add(0x3d)]);
        }
    };
    let normalized_path = |path: &Path| -> Vec<u8> { normalized_root_key(path).into_bytes() };
    let abs_rates = absolutize_path(rates_path.to_path_buf(), base_dir);
    feed_bytes(b'R', &normalized_path(&abs_rates));
    for source in expand_sources_with_base(sources.to_vec(), base_dir) {
        if let Some(provider) = supported(source.provider) {
            feed_bytes(b'P', provider.as_bytes());
            feed_bytes(b'S', &[store_order(source.store)]);
            feed_bytes(b'D', &normalized_path(&source.root));
        }
    }
    format!("{h1:016x}{h2:016x}")
}

fn collect_source(
    source: &SessionSource,
    provider: &str,
    files: &mut BTreeMap<(String, PathBuf), DiscoveredSource>,
    entries: &mut usize,
    warnings: &mut BTreeSet<String>,
) {
    match fs::metadata(&source.root) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(_) => {
            warnings.insert(format!(
                "Some {provider} session roots could not be read; coverage may be incomplete."
            ));
            return;
        }
    }
    if source.store == SessionStore::Sqlite {
        for db in crate::sources::databases(source.provider, &source.root) {
            if !db.is_file() {
                continue;
            }
            *entries += 1;
            if *entries > MAX_ENTRIES {
                warnings.insert(format!(
                    "Coverage traversal capped at {MAX_ENTRIES} directory entries."
                ));
                return;
            }
            match fs::metadata(&db) {
                Ok(meta) => {
                    let mut wal = db.as_os_str().to_os_string();
                    wal.push("-wal");
                    let (extra_bytes, extra_modified) = sidecar_meta(Path::new(&wal));
                    files.insert(
                        (provider.to_owned(), db),
                        DiscoveredSource {
                            fingerprint: Fingerprint {
                                bytes: meta.len(),
                                modified: meta.modified().ok(),
                                extra_bytes,
                                extra_modified,
                            },
                            root: source.root.clone(),
                            store: SessionStore::Sqlite,
                        },
                    );
                }
                Err(_) => {
                    warnings.insert(format!(
                        "{provider} database metadata could not be read; coverage is incomplete."
                    ));
                }
            }
        }
        return;
    }

    let max_depth = if provider == "aider" {
        AIDER_MAX_DIR_DEPTH
    } else {
        usize::MAX
    };
    let mut pending = vec![(source.root.clone(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
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
            let child_path = child.path();
            if kind.is_dir() && depth < max_depth && !should_skip_dir(provider, &child_path) {
                pending.push((child_path, depth + 1));
                continue;
            }
            if kind.is_file() && is_matching_file(provider, &child_path, &source.root) {
                // Windows directory entries can carry stale sizes while a log writer is open.
                match fs::metadata(&child_path) {
                    Ok(meta) => {
                        let (extra_bytes, extra_modified) = match provider {
                            "cline" | "roo-code" => {
                                sidecar_meta(&child_path.with_file_name("history_item.json"))
                            }
                            "factory-droid" => droid_log_path(&child_path)
                                .map(|p| sidecar_meta(&p))
                                .unwrap_or((0, None)),
                            _ => (0, None),
                        };
                        files.insert(
                            (provider.to_owned(), child_path),
                            DiscoveredSource {
                                fingerprint: Fingerprint {
                                    bytes: meta.len(),
                                    modified: meta.modified().ok(),
                                    extra_bytes,
                                    extra_modified,
                                },
                                root: source.root.clone(),
                                store: source.store,
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

fn sidecar_meta(path: &Path) -> (u64, Option<SystemTime>) {
    match fs::metadata(path) {
        Ok(meta) => (meta.len(), meta.modified().ok()),
        Err(_) => (0, None),
    }
}

fn should_skip_dir(provider: &str, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    match provider {
        "antigravity" => {
            name.eq_ignore_ascii_case("browser_sandbox")
                || name.eq_ignore_ascii_case("scratch")
                || name.eq_ignore_ascii_case("steps")
                || name.eq_ignore_ascii_case(".git")
                || name.eq_ignore_ascii_case("node_modules")
                || name.eq_ignore_ascii_case("target")
        }
        "aider" => {
            name.starts_with('.')
                || name.eq_ignore_ascii_case("AppData")
                || name.eq_ignore_ascii_case("node_modules")
                || name.eq_ignore_ascii_case("target")
        }
        _ => false,
    }
}

fn is_matching_file(provider: &str, path: &Path, root: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    match provider {
        "codex" => {
            lower.ends_with(".jsonl")
                || lower.ends_with(".jsonl.zst")
                || lower.ends_with(".jsonl.zstd")
        }
        "claude-code" | "copilot-cli" => lower.ends_with(".jsonl"),
        "antigravity" => lower == "transcript.jsonl",
        "deepseek-harness" => lower == "session.v4.jsonl" || lower == "session.v4.jsonl.zstd",
        "gemini-cli" => {
            has_relative_ancestor(path, root, "chats")
                && (lower.ends_with(".jsonl") || lower.ends_with(".json"))
        }
        "qwen-code" => has_relative_ancestor(path, root, "chats") && lower.ends_with(".jsonl"),
        "factory-droid" => lower.ends_with(".settings.json"),
        "junie" => lower == "events.jsonl",
        "cline" | "roo-code" => lower == "ui_messages.json",
        "aider" => lower == ".aider.chat.history.md",
        _ => false,
    }
}

fn has_relative_ancestor(path: &Path, root: &Path, dir_name: &str) -> bool {
    let under_root = path.strip_prefix(root).unwrap_or(path);
    under_root.parent().is_some_and(|parent| {
        parent.components().any(|c| {
            c.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(dir_name)
        })
    }) || root
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(dir_name))
}

fn parse_source(
    path: &Path,
    root: &Path,
    provider: &str,
    store: SessionStore,
) -> io::Result<ParsedFile> {
    if store == SessionStore::Sqlite {
        return match provider {
            "opencode" | "kilo-code" => parse_opencode_db(path, provider),
            "zed" => parse_zed_db(path),
            "crush" => parse_crush_db(path),
            _ => Ok(ParsedFile::default()),
        };
    }

    match provider {
        "codex" => parse_codex_file(path),
        "claude-code" => parse_claude_file(path),
        "antigravity" => parse_antigravity_file(path, root),
        "deepseek-harness" => parse_deepseek_file(path),
        "gemini-cli" => parse_gemini_file(path),
        "qwen-code" => parse_qwen_file(path),
        "factory-droid" => parse_droid_file(path),
        "copilot-cli" => parse_copilot_file(path),
        "junie" => parse_junie_file(path),
        "cline" | "roo-code" => parse_cline_like_file(path, provider),
        "aider" => parse_aider_file(path),
        _ => Ok(ParsedFile::default()),
    }
}

fn is_zstd_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zst") || ext.eq_ignore_ascii_case("zstd"))
}

fn read_capped_bytes(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn read_zstd_bytes(
    path: &Path,
    provider: &str,
    warnings: &mut BTreeSet<String>,
) -> io::Result<Option<Vec<u8>>> {
    let Some(compressed) = read_capped_bytes(path, MAX_FILE_BYTES)? else {
        warnings.insert(format!(
            "Coverage excludes files larger than {} MiB.",
            MAX_FILE_BYTES / 1024 / 1024
        ));
        return Ok(None);
    };
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < compressed.len() {
        let remaining = &compressed[offset..];
        if remaining.len() >= 4
            && remaining[1..4] == [0x2a, 0x4d, 0x18]
            && (0x50..=0x5f).contains(&remaining[0])
        {
            if remaining.len() < 8 {
                warnings.insert(format!(
                    "{provider} unfinished compressed frame was skipped; coverage may be incomplete."
                ));
                break;
            }
            let payload_len =
                u32::from_le_bytes([remaining[4], remaining[5], remaining[6], remaining[7]])
                    as usize;
            let Some(frame_len) = 8usize.checked_add(payload_len) else {
                warnings.insert(format!(
                    "{provider} malformed compressed frame was skipped; coverage is incomplete."
                ));
                break;
            };
            if frame_len > remaining.len() {
                warnings.insert(format!(
                    "{provider} unfinished compressed frame was skipped; coverage may be incomplete."
                ));
                break;
            }
            offset += frame_len;
            continue;
        }
        if remaining.len() >= 4 && remaining[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
            warnings.insert(format!(
                "{provider} malformed compressed frame was skipped; coverage is incomplete."
            ));
            break;
        }
        let Ok(frame_len) = zstd::zstd_safe::find_frame_compressed_size(remaining) else {
            warnings.insert(format!(
                "{provider} unfinished compressed frame was skipped; coverage may be incomplete."
            ));
            break;
        };
        let mut decoder = zstd::stream::read::Decoder::new(&remaining[..frame_len])?;
        let budget = (MAX_FILE_BYTES as usize).saturating_sub(out.len()) + 1;
        let mut chunk = Vec::new();
        if (&mut decoder)
            .take(budget as u64)
            .read_to_end(&mut chunk)
            .is_err()
        {
            warnings.insert(format!(
                "{provider} malformed compressed frame was skipped; coverage is incomplete."
            ));
            break;
        }
        if out.len() + chunk.len() > MAX_FILE_BYTES as usize {
            warnings.insert(format!(
                "Coverage excludes decompressed files larger than {} MiB.",
                MAX_FILE_BYTES / 1024 / 1024
            ));
            return Ok(None);
        }
        out.extend_from_slice(&chunk);
        offset += frame_len;
    }
    Ok(Some(out))
}

fn for_each_jsonl_value(
    mut reader: impl BufRead,
    provider: &str,
    warnings: &mut BTreeSet<String>,
    mut handle: impl FnMut(usize, Value, &mut BTreeSet<String>) -> bool,
) -> io::Result<()> {
    let mut buffer = Vec::new();
    let mut sequence = 0usize;
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
            warnings.insert(format!(
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
        if buffer.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&buffer) {
            Ok(value) => value,
            Err(_) => {
                warnings.insert(format!(
                    "{provider} malformed or unfinished JSONL records were skipped; coverage is incomplete."
                ));
                continue;
            }
        };
        if !handle(sequence, value, warnings) {
            break;
        }
    }
    Ok(())
}

fn push_record(parsed: &mut ParsedFile, provider: &str, record: Record) -> bool {
    if parsed.records.len() >= MAX_RECORDS {
        parsed.warnings.insert(format!(
            "{provider} metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."
        ));
        return false;
    }
    parsed.records.push(record);
    true
}

fn parse_codex_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    if is_zstd_path(path) {
        let Some(bytes) = read_zstd_bytes(path, "codex", &mut parsed.warnings)? else {
            return Ok(parsed);
        };
        parse_codex_reader(Cursor::new(bytes), &mut parsed)?;
    } else {
        let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
        parse_codex_reader(reader, &mut parsed)?;
    }
    Ok(parsed)
}

fn parse_codex_reader(reader: impl BufRead, parsed: &mut ParsedFile) -> io::Result<()> {
    let mut session = None;
    let mut context = Context::default();
    let mut forked = false;
    let mut fork_time = None;
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "codex",
        &mut warnings,
        |sequence, value, warnings| {
            let time = value.get("timestamp").and_then(timestamp_ms).unwrap_or(0);
            let payload = &value["payload"];
            let kind = match value["type"].as_str().unwrap_or("") {
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
                        warnings.insert("codex forked history excludes inherited records before the fork timestamp. The first post-fork cumulative record requires a baseline or explicit last-request usage; otherwise its attribution remains unknown.".to_owned());
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
            };
            if let Some(kind) = kind {
                let Some(session) = &session else {
                    warnings.insert("codex records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                    return true;
                };
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
                        _ => return true,
                    }
                } else if forked && (fork_time.is_none() || event_time <= 0) {
                    warnings.insert("codex fork records without a reliable creation/event timestamp were excluded because inherited usage cannot be attributed safely.".to_owned());
                    return true;
                } else {
                    kind
                };
                if parsed.records.len() >= MAX_RECORDS {
                    warnings.insert(format!(
                    "codex metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."
                ));
                    return false;
                }
                parsed.records.push(Record {
                    session: session.clone(),
                    context: context.clone(),
                    time: event_time,
                    sequence,
                    forked,
                    kind,
                });
            }
            true
        },
    )?;
    parsed.warnings = warnings;
    Ok(())
}

fn parse_claude_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let mut session = None;
    let mut last_effort = String::new();
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "claude-code",
        &mut warnings,
        |sequence, value, warnings| {
            if let Some(id) = identity(&value["sessionId"]) {
                session = Some(id);
            }
            match value["type"].as_str().unwrap_or("") {
                "api-request" => {
                    last_effort = label(
                        value
                            .pointer("/params/output_config/effort")
                            .or_else(|| value.pointer("/params/effort"))
                            .unwrap_or(&Value::Null),
                        32,
                    );
                }
                "assistant" if value["message"]["usage"].is_object() => {
                    let mut effort = label(
                        value
                            .get("effort")
                            .or_else(|| value.pointer("/message/effort"))
                            .unwrap_or(&Value::Null),
                        32,
                    );
                    if effort.is_empty() {
                        effort = last_effort.clone();
                    }
                    let context = Context {
                        model: label(&value["message"]["model"], 128),
                        effort,
                    };
                    match identity(&value["message"]["id"]) {
                        Some(id) => {
                            let Some(session) = &session else {
                                warnings.insert("claude-code records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                                return true;
                            };
                            if parsed.records.len() >= MAX_RECORDS {
                                warnings.insert(format!("claude-code metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                                return false;
                            }
                            let usage = TokenUsage::claude(&value["message"]["usage"]);
                            let details = &value["message"]["usage"]["cache_creation"];
                            let write_1h =
                                number(details, "ephemeral_5m_input_tokens").and_then(|five| {
                                    let hour = number(details, "ephemeral_1h_input_tokens")?;
                                    (five.checked_add(hour) == usage.cache_write_tokens)
                                        .then_some(hour)
                                });
                            let time = value.get("timestamp").and_then(timestamp_ms).unwrap_or(0);
                            parsed.records.push(Record {
                                session: session.clone(),
                                context,
                                time,
                                sequence,
                                forked: false,
                                kind: RecordKind::ClaudeUsage {
                                    id,
                                    request_id: identity(&value["requestId"]),
                                    usage,
                                    write_1h,
                                },
                            });
                        }
                        None => {
                            warnings.insert("claude-code assistant usage without message IDs was skipped because it cannot be deduplicated safely.".to_owned());
                        }
                    }
                }
                _ => {}
            }
            true
        },
    )?;
    parsed.warnings = warnings;
    Ok(parsed)
}

fn antigravity_session_id(path: &Path, root: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(root) {
        let mut comps = rel.components();
        if let (Some(first), Some(_)) = (comps.next(), comps.next()) {
            let s = first.as_os_str().to_string_lossy().to_string();
            if !s.is_empty() && s.len() <= 256 {
                return s;
            }
        }
    }
    "antigravity-session".to_owned()
}

fn parse_antigravity_file(path: &Path, root: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let session = antigravity_session_id(path, root);
    let mut last_model = String::new();
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "antigravity",
        &mut warnings,
        |sequence, value, warnings| {
            let kind_str = value["type"].as_str().unwrap_or("");
            if !matches!(kind_str, "PLANNER_RESPONSE" | "CHECKPOINT") {
                return true;
            }
            let m = label(&value["model"], 128);
            if !m.is_empty() {
                last_model = m;
            }
            if value.get("input_tokens").is_none()
                && value.get("output_tokens").is_none()
                && value.get("cache_read_tokens").is_none()
            {
                return true;
            }
            if parsed.records.len() >= MAX_RECORDS {
                warnings.insert(format!(
                "antigravity metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."
            ));
                return false;
            }
            let cached = number(&value, "cache_read_tokens");
            let input = number(&value, "input_tokens").and_then(|u| u.checked_add(cached?));
            let usage = TokenUsage {
                input_tokens: input,
                cached_input_tokens: cached,
                cache_write_tokens: None,
                output_tokens: number(&value, "output_tokens"),
                reasoning_tokens: None,
            };
            let id = value
                .get("step_index")
                .and_then(Value::as_u64)
                .map(|i| format!("step:{i}"))
                .unwrap_or_else(|| format!("line:{sequence}"));
            let time = value.get("created_at").and_then(timestamp_ms).unwrap_or(0);
            parsed.records.push(Record {
                session: session.clone(),
                context: Context {
                    model: last_model.clone(),
                    effort: String::new(),
                },
                time,
                sequence,
                forked: false,
                kind: RecordKind::MessageUsage {
                    id,
                    usage,
                    write_1h: None,
                },
            });
            true
        },
    )?;
    parsed.warnings = warnings;
    Ok(parsed)
}

fn parse_deepseek_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    if is_zstd_path(path) {
        let Some(bytes) = read_zstd_bytes(path, "deepseek-harness", &mut parsed.warnings)? else {
            return Ok(parsed);
        };
        parse_deepseek_reader(Cursor::new(bytes), &mut parsed)?;
    } else {
        let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
        parse_deepseek_reader(reader, &mut parsed)?;
    }
    Ok(parsed)
}

fn parse_deepseek_reader(reader: impl BufRead, parsed: &mut ParsedFile) -> io::Result<()> {
    let mut session = None;
    let mut context = Context::default();
    let mut in_seed = false;
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "deepseek-harness",
        &mut warnings,
        |sequence, value, warnings| {
            let time = value.get("time").and_then(timestamp_ms).unwrap_or(0);
            match value["type"].as_str().unwrap_or("") {
                "session" => {
                    if let Some(id) = identity(&value["id"]) {
                        session = Some(id);
                    }
                    in_seed = value["isSeeded"].as_bool() == Some(true);
                }
                "session/end-seed" => {
                    in_seed = false;
                }
                "request/header" => {
                    let cfg = &value["data"]["header"]["config"];
                    let model = label(&cfg["model"], 128);
                    if !model.is_empty() {
                        context.model = model;
                    }
                    context.effort = label(&cfg["reasoningEffort"], 32);
                }
                _ if in_seed => {}
                "turn/start" => {
                    let Some(session) = &session else {
                        warnings.insert("deepseek-harness records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                        return true;
                    };
                    if let Some(turn) = value["data"]["turn"].as_u64() {
                        if parsed.records.len() >= MAX_RECORDS {
                            warnings.insert(format!("deepseek-harness metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                            return false;
                        }
                        parsed.records.push(Record {
                            session: session.clone(),
                            context: context.clone(),
                            time,
                            sequence,
                            forked: false,
                            kind: RecordKind::Start {
                                id: format!("turn:{turn}"),
                            },
                        });
                    }
                }
                "turn/end" => {
                    let Some(session) = &session else {
                        return true;
                    };
                    if let Some(turn) = value["data"]["turn"].as_u64() {
                        if parsed.records.len() >= MAX_RECORDS {
                            warnings.insert(format!("deepseek-harness metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                            return false;
                        }
                        parsed.records.push(Record {
                            session: session.clone(),
                            context: context.clone(),
                            time,
                            sequence,
                            forked: false,
                            kind: RecordKind::Complete {
                                id: format!("turn:{turn}"),
                            },
                        });
                    }
                }
                "assistant/message" if value["data"]["usage"].is_object() => {
                    let Some(session) = &session else {
                        warnings.insert("deepseek-harness records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                        return true;
                    };
                    let u = &value["data"]["usage"];
                    let msg_model = label(&value["data"]["message"]["source"]["model"], 128);
                    let model = if msg_model.is_empty() {
                        context.model.clone()
                    } else {
                        msg_model
                    };
                    let id = identity(&value["data"]["message"]["id"])
                        .or_else(|| {
                            value
                                .get("seq")
                                .and_then(Value::as_u64)
                                .map(|s| format!("seq:{s}"))
                        })
                        .unwrap_or_else(|| format!("line:{sequence}"));
                    let cached = number(u, "cacheReadTokens").unwrap_or(0);
                    let written = number(u, "cacheWriteTokens");
                    let input = number(u, "inputTokens")
                        .and_then(|i| i.checked_add(cached))
                        .and_then(|i| i.checked_add(written.unwrap_or(0)));
                    let usage = TokenUsage {
                        input_tokens: input,
                        cached_input_tokens: Some(cached),
                        cache_write_tokens: written,
                        output_tokens: number(u, "outputTokens"),
                        reasoning_tokens: number(u, "reasoningTokens"),
                    };
                    if parsed.records.len() >= MAX_RECORDS {
                        warnings.insert(format!("deepseek-harness metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                        return false;
                    }
                    parsed.records.push(Record {
                        session: session.clone(),
                        context: Context {
                            model,
                            effort: context.effort.clone(),
                        },
                        time,
                        sequence,
                        forked: false,
                        kind: RecordKind::MessageUsage {
                            id,
                            usage,
                            write_1h: None,
                        },
                    });
                }
                _ => {}
            }
            true
        },
    )?;
    parsed.warnings = warnings;
    Ok(())
}

fn gemini_record_from_msg(msg: &Value, session: &str, sequence: usize) -> Option<(String, Record)> {
    if msg["type"] != "gemini" || !msg["tokens"].is_object() {
        return None;
    }
    let id = identity(&msg["id"])?;
    let t = &msg["tokens"];
    let thoughts = number(t, "thoughts");
    let output = number(t, "output").and_then(|o| o.checked_add(thoughts.unwrap_or(0)));
    let usage = TokenUsage {
        input_tokens: number(t, "input"),
        cached_input_tokens: number(t, "cached"),
        cache_write_tokens: None,
        output_tokens: output,
        reasoning_tokens: thoughts,
    };
    let model = label(&msg["model"], 128);
    let time = msg.get("timestamp").and_then(timestamp_ms).unwrap_or(0);
    Some((
        id.clone(),
        Record {
            session: session.to_owned(),
            context: Context {
                model,
                effort: String::new(),
            },
            time,
            sequence,
            forked: false,
            kind: RecordKind::MessageUsage {
                id,
                usage,
                write_1h: None,
            },
        },
    ))
}

fn parse_gemini_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let is_json = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    if is_json {
        let Some(bytes) = read_capped_bytes(path, MAX_FILE_BYTES)? else {
            parsed.warnings.insert(format!(
                "Coverage excludes files larger than {} MiB.",
                MAX_FILE_BYTES / 1024 / 1024
            ));
            return Ok(parsed);
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                parsed.warnings.insert("gemini-cli malformed or unfinished JSON records were skipped; coverage is incomplete.".to_owned());
                return Ok(parsed);
            }
        };
        let Some(session) = identity(&value["sessionId"]) else {
            parsed.warnings.insert("gemini-cli records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
            return Ok(parsed);
        };
        if let Some(messages) = value["messages"].as_array() {
            for (idx, msg) in messages.iter().enumerate() {
                if let Some((_, record)) = gemini_record_from_msg(msg, &session, idx + 1)
                    && !push_record(&mut parsed, "gemini-cli", record)
                {
                    break;
                }
            }
        }
        return Ok(parsed);
    }

    let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let mut session = None;
    let mut by_id = HashMap::<String, usize>::new();
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "gemini-cli",
        &mut warnings,
        |sequence, value, warnings| {
            if let Some(id) = identity(&value["sessionId"]) {
                session = Some(id);
            }
            let mut handle_msg = |msg: &Value| {
                let Some(sess) = &session else {
                    warnings.insert("gemini-cli records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                    return true;
                };
                let Some((id, record)) = gemini_record_from_msg(msg, sess, sequence) else {
                    return true;
                };
                if let Some(&slot) = by_id.get(&id) {
                    parsed.records[slot] = record;
                    return true;
                }
                if parsed.records.len() >= MAX_RECORDS {
                    warnings.insert(format!("gemini-cli metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                    return false;
                }
                by_id.insert(id, parsed.records.len());
                parsed.records.push(record);
                true
            };
            if let Some(messages) = value.pointer("/$set/messages").and_then(Value::as_array) {
                for msg in messages {
                    if !handle_msg(msg) {
                        return false;
                    }
                }
            } else if !handle_msg(&value) {
                return false;
            }
            true
        },
    )?;
    parsed.warnings = warnings;
    Ok(parsed)
}

fn parse_qwen_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let mut session = None;
    let mut telemetry = Vec::new();
    let mut fallback = Vec::new();
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "qwen-code",
        &mut warnings,
        |sequence, value, warnings| {
            if let Some(id) = identity(&value["sessionId"]) {
                session = Some(id);
            }
            let base_time = value.get("timestamp").and_then(timestamp_ms).unwrap_or(0);
            if value["type"] == "system"
                && value["subtype"] == "ui_telemetry"
                && let Some(ui) = value.pointer("/systemPayload/uiEvent")
                && ui["event.name"] == "qwen-code.api_response"
            {
                let Some(sess) = &session else {
                    warnings.insert("qwen-code records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                    return true;
                };
                if telemetry.len() >= MAX_RECORDS {
                    warnings.insert(format!("qwen-code metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                    return false;
                }
                let time = ui
                    .get("event.timestamp")
                    .and_then(timestamp_ms)
                    .unwrap_or(base_time);
                let id = identity(&ui["response_id"])
                    .or_else(|| identity(&value["uuid"]))
                    .unwrap_or_else(|| format!("line:{sequence}"));
                let thoughts = number(ui, "thoughts_token_count");
                let raw_out = number(ui, "output_token_count");
                let usage = TokenUsage {
                    input_tokens: number(ui, "input_token_count"),
                    cached_input_tokens: number(ui, "cached_content_token_count"),
                    cache_write_tokens: None,
                    output_tokens: raw_out.and_then(|o| o.checked_add(thoughts.unwrap_or(0))),
                    reasoning_tokens: thoughts,
                };
                telemetry.push(Record {
                    session: sess.clone(),
                    context: Context {
                        model: label(&ui["model"], 128),
                        effort: String::new(),
                    },
                    time,
                    sequence,
                    forked: false,
                    kind: RecordKind::MessageUsage {
                        id,
                        usage,
                        write_1h: None,
                    },
                });
            } else if value["type"] == "assistant" && value["usageMetadata"].is_object() {
                let Some(sess) = &session else {
                    warnings.insert("qwen-code records without a session ID were excluded because sessions and duplicated usage cannot be identified safely.".to_owned());
                    return true;
                };
                if fallback.len() < MAX_RECORDS {
                    let um = &value["usageMetadata"];
                    let id = identity(&value["uuid"])
                        .or_else(|| identity(&value["messageId"]))
                        .unwrap_or_else(|| format!("line:{sequence}"));
                    let mut model = label(&value["model"], 128);
                    if model.is_empty() {
                        model = label(
                            value
                                .pointer("/executionContext/modelId")
                                .unwrap_or(&Value::Null),
                            128,
                        );
                    }
                    let thoughts = number(um, "thoughtsTokenCount");
                    let raw_out = number(um, "candidatesTokenCount");
                    let usage = TokenUsage {
                        input_tokens: number(um, "promptTokenCount"),
                        cached_input_tokens: number(um, "cachedContentTokenCount"),
                        cache_write_tokens: None,
                        output_tokens: raw_out.and_then(|o| o.checked_add(thoughts.unwrap_or(0))),
                        reasoning_tokens: thoughts,
                    };
                    fallback.push(Record {
                        session: sess.clone(),
                        context: Context {
                            model,
                            effort: String::new(),
                        },
                        time: base_time,
                        sequence,
                        forked: false,
                        kind: RecordKind::MessageUsage {
                            id,
                            usage,
                            write_1h: None,
                        },
                    });
                }
            }
            true
        },
    )?;
    parsed.records = if telemetry.is_empty() {
        fallback
    } else {
        telemetry
    };
    parsed.warnings = warnings;
    Ok(parsed)
}

fn parse_flexible_timestamp(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.timestamp_millis());
    }
    let n = trimmed.parse::<i64>().ok()?;
    timestamp_ms(&Value::from(n))
}

fn parse_opencode_db(path: &Path, provider: &str) -> io::Result<ParsedFile> {
    let conn = crate::sources::open_database(path)
        .map_err(|_| io::Error::other("sqlite database open failed"))?;
    let mut parsed = ParsedFile::default();
    let mut seen_ids = HashSet::new();
    let mut sequence = 0usize;

    for (sql, is_v2) in [
        (
            "SELECT * FROM session_message WHERE type = 'assistant'",
            true,
        ),
        ("SELECT * FROM message", false),
    ] {
        let Ok(mut stmt) = conn.prepare(sql) else {
            continue;
        };
        let Ok(mut rows) = stmt.query([]) else {
            continue;
        };
        while let Some(row) = rows
            .next()
            .map_err(|_| io::Error::other("sqlite row read failed"))?
        {
            let Some(msg_id) = crate::sources::column_text_opt(row, "id")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty() && s.len() <= 256)
            else {
                continue;
            };
            if seen_ids.contains(&msg_id) {
                continue;
            }
            let Some(data_str) = crate::sources::column_text_opt(row, "data") else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<Value>(&data_str) else {
                continue;
            };
            if !is_v2 && data["role"].as_str() != Some("assistant") {
                continue;
            }
            let session_id = crate::sources::column_text_opt(row, "session_id")
                .or_else(|| crate::sources::column_text_opt(row, "aggregate_id"))
                .or_else(|| crate::sources::column_text_opt(row, "sessionID"))
                .or_else(|| data["sessionID"].as_str().map(str::to_owned))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty() && s.len() <= 256);
            let Some(session_id) = session_id else {
                continue;
            };
            let tokens = &data["tokens"];
            if !tokens.is_object() {
                continue;
            }
            let raw_in = number(tokens, "input");
            let cached = number(&tokens["cache"], "read");
            let written = number(&tokens["cache"], "write");
            let raw_out = number(tokens, "output");
            let reasoning = number(tokens, "reasoning");
            if raw_in.unwrap_or(0) == 0
                && cached.unwrap_or(0) == 0
                && written.unwrap_or(0) == 0
                && raw_out.unwrap_or(0) == 0
                && reasoning.unwrap_or(0) == 0
            {
                continue;
            }
            let input = raw_in.and_then(|i| match (cached, written) {
                (Some(c), Some(w)) => i.checked_add(c)?.checked_add(w),
                (Some(c), None) => i.checked_add(c),
                _ => None,
            });
            let output = raw_out.and_then(|o| o.checked_add(reasoning.unwrap_or(0)));
            let usage = TokenUsage {
                input_tokens: input,
                cached_input_tokens: cached,
                cache_write_tokens: written,
                output_tokens: output,
                reasoning_tokens: reasoning,
            };
            if !usage.any() || !usage.valid() {
                continue;
            }
            seen_ids.insert(msg_id.clone());
            let model = if is_v2 {
                label(data.pointer("/model/id").unwrap_or(&Value::Null), 128)
            } else {
                label(&data["modelID"], 128)
            };
            let effort = if is_v2 {
                label(data.pointer("/model/variant").unwrap_or(&Value::Null), 32)
            } else {
                label(&data["variant"], 32)
            };
            let time = data
                .pointer("/time/completed")
                .or_else(|| data.pointer("/time/created"))
                .and_then(timestamp_ms)
                .or_else(|| {
                    crate::sources::column_text_opt(row, "time_created")
                        .and_then(|s| parse_flexible_timestamp(&s))
                })
                .unwrap_or(0);
            sequence += 1;
            if !push_record(
                &mut parsed,
                provider,
                Record {
                    session: session_id,
                    context: Context { model, effort },
                    time,
                    sequence,
                    forked: false,
                    kind: RecordKind::MessageUsage {
                        id: msg_id,
                        usage,
                        write_1h: None,
                    },
                },
            ) {
                return Ok(parsed);
            }
        }
    }
    Ok(parsed)
}

fn droid_log_path(settings_path: &Path) -> Option<PathBuf> {
    let name = settings_path.file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    let stem_len = lower.strip_suffix(".settings.json")?.len();
    Some(settings_path.with_file_name(format!("{}.jsonl", &name[..stem_len])))
}

fn droid_last_timestamp(jsonl_path: &Path) -> Option<i64> {
    let mut file = File::open(jsonl_path).ok()?;
    let len = file.metadata().ok()?.len();
    let tail_start = len.saturating_sub(65_536);
    file.seek(SeekFrom::Start(tail_start)).ok()?;
    let mut buf = Vec::new();
    file.take(65_537).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        if let Ok(val) = serde_json::from_str::<Value>(line)
            && let Some(ts) = val.get("timestamp").and_then(timestamp_ms)
        {
            return Some(ts);
        }
    }
    None
}

fn system_time_ms(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
}

fn parse_droid_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let Some(bytes) = read_capped_bytes(path, MAX_FILE_BYTES)? else {
        parsed.warnings.insert(format!(
            "Coverage excludes files larger than {} MiB.",
            MAX_FILE_BYTES / 1024 / 1024
        ));
        return Ok(parsed);
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            parsed.warnings.insert("factory-droid malformed or unfinished settings JSON was skipped; coverage is incomplete.".to_owned());
            return Ok(parsed);
        }
    };
    let tu = &doc["tokenUsage"];
    if !tu.is_object() {
        return Ok(parsed);
    }
    let raw_in = number(tu, "inputTokens");
    let raw_out = number(tu, "outputTokens");
    let cached = number(tu, "cacheReadTokens");
    let written = number(tu, "cacheCreationTokens");
    if raw_in.unwrap_or(0) == 0
        && raw_out.unwrap_or(0) == 0
        && cached.unwrap_or(0) == 0
        && written.unwrap_or(0) == 0
    {
        return Ok(parsed);
    }
    let Some(session) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| {
            let lower = n.to_ascii_lowercase();
            let len = lower.strip_suffix(".settings.json")?.len();
            Some(n[..len].to_owned())
        })
        .filter(|s| !s.is_empty() && s.len() <= 256)
    else {
        return Ok(parsed);
    };
    let input = raw_in.and_then(|i| i.checked_add(cached?)?.checked_add(written?));
    let usage = TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        cache_write_tokens: written,
        output_tokens: raw_out,
        reasoning_tokens: number(tu, "thinkingTokens"),
    };
    let time = droid_log_path(path)
        .and_then(|p| droid_last_timestamp(&p))
        .or_else(|| {
            fs::metadata(path)
                .ok()?
                .modified()
                .ok()
                .and_then(system_time_ms)
        })
        .unwrap_or(0);
    parsed.warnings.insert(
        "factory-droid reports session-level totals rather than per-request records.".to_owned(),
    );
    parsed.records.push(Record {
        session,
        context: Context {
            model: label(&doc["model"], 128),
            effort: label(&doc["reasoningEffort"], 32),
        },
        time,
        sequence: 1,
        forked: false,
        kind: RecordKind::MessageUsage {
            id: "session".to_owned(),
            usage,
            write_1h: None,
        },
    });
    Ok(parsed)
}

fn copilot_usage_from_obj(u: &Value) -> TokenUsage {
    let raw_in = number(u, "inputTokens").or_else(|| number(u, "input_tokens"));
    let output = number(u, "outputTokens").or_else(|| number(u, "output_tokens"));
    let cached = number(u, "cacheReadTokens").or_else(|| number(u, "cache_read_input_tokens"));
    let written =
        number(u, "cacheWriteTokens").or_else(|| number(u, "cache_creation_input_tokens"));
    let input = raw_in.and_then(|i| {
        let add = cached.unwrap_or(0).saturating_add(written.unwrap_or(0));
        if add > i { i.checked_add(add) } else { Some(i) }
    });
    TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        cache_write_tokens: written,
        output_tokens: output,
        reasoning_tokens: None,
    }
}

fn parse_copilot_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let fallback_session = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .map(str::to_owned);
    let mut session = fallback_session;
    let mut context = Context::default();
    let mut task_records = Vec::new();
    let mut usage_records = Vec::new();
    let mut shutdown_fallback = Vec::new();
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "copilot-cli",
        &mut warnings,
        |sequence, value, warnings| {
            let kind_str = value["type"].as_str().unwrap_or("");
            let data = &value["data"];
            let time = value.get("timestamp").and_then(timestamp_ms).unwrap_or(0);
            let ephemeral = value["ephemeral"].as_bool() == Some(true);
            match kind_str {
                "session.start" => {
                    if let Some(id) = identity(&data["sessionId"]) {
                        session = Some(id);
                    }
                    let m = label(&data["selectedModel"], 128);
                    if !m.is_empty() {
                        context.model = m;
                    }
                }
                "session.model_change" => {
                    let m = label(&data["newModel"], 128);
                    if !m.is_empty() {
                        context.model = m;
                    }
                }
                "assistant.turn_start" if !ephemeral => {
                    let m = label(&data["model"], 128);
                    if !m.is_empty() {
                        context.model = m;
                    }
                    if let Some(sess) = &session
                        && task_records.len() < MAX_RECORDS
                    {
                        let id = identity(&data["turnId"]).unwrap_or_else(|| "turn".to_owned());
                        task_records.push(Record {
                            session: sess.clone(),
                            context: context.clone(),
                            time,
                            sequence,
                            forked: false,
                            kind: RecordKind::Start { id },
                        });
                    }
                }
                "assistant.turn_end" if !ephemeral => {
                    if let Some(sess) = &session
                        && task_records.len() < MAX_RECORDS
                    {
                        let id = identity(&data["turnId"]).unwrap_or_else(|| "turn".to_owned());
                        task_records.push(Record {
                            session: sess.clone(),
                            context: context.clone(),
                            time,
                            sequence,
                            forked: false,
                            kind: RecordKind::Complete { id },
                        });
                    }
                }
                "assistant.usage" => {
                    let Some(sess) = &session else {
                        return true;
                    };
                    if usage_records.len() >= MAX_RECORDS {
                        warnings.insert(format!("copilot-cli metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."));
                        return false;
                    }
                    let u = data.get("usage").filter(|v| v.is_object()).unwrap_or(data);
                    let mut model = label(&u["model"], 128);
                    if model.is_empty() {
                        model = label(&data["model"], 128);
                    }
                    if model.is_empty() {
                        model = context.model.clone();
                    }
                    let usage = copilot_usage_from_obj(u);
                    if usage.any() {
                        let id =
                            identity(&value["id"]).unwrap_or_else(|| format!("line:{sequence}"));
                        usage_records.push(Record {
                            session: sess.clone(),
                            context: Context {
                                model,
                                effort: String::new(),
                            },
                            time,
                            sequence,
                            forked: false,
                            kind: RecordKind::MessageUsage {
                                id,
                                usage,
                                write_1h: None,
                            },
                        });
                    }
                }
                "session.usage_checkpoint" | "session.shutdown" => {
                    let Some(sess) = &session else {
                        return true;
                    };
                    if let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) {
                        let mut batch = Vec::new();
                        for (idx, (model_key, entry)) in metrics.iter().enumerate() {
                            let u = entry
                                .get("usage")
                                .filter(|v| v.is_object())
                                .unwrap_or(entry);
                            let usage = copilot_usage_from_obj(u);
                            if usage.any() {
                                let model = label(&Value::String(model_key.clone()), 128);
                                batch.push(Record {
                                    session: sess.clone(),
                                    context: Context {
                                        model,
                                        effort: String::new(),
                                    },
                                    time,
                                    sequence,
                                    forked: false,
                                    kind: RecordKind::MessageUsage {
                                        id: format!("shutdown:{idx}"),
                                        usage,
                                        write_1h: None,
                                    },
                                });
                            }
                        }
                        if !batch.is_empty() {
                            shutdown_fallback = batch;
                        }
                    } else {
                        let u = data.get("usage").filter(|v| v.is_object()).unwrap_or(data);
                        let usage = copilot_usage_from_obj(u);
                        if usage.any() {
                            let mut model = label(&u["model"], 128);
                            if model.is_empty() {
                                model = context.model.clone();
                            }
                            shutdown_fallback = vec![Record {
                                session: sess.clone(),
                                context: Context {
                                    model,
                                    effort: String::new(),
                                },
                                time,
                                sequence,
                                forked: false,
                                kind: RecordKind::MessageUsage {
                                    id: "shutdown:0".to_owned(),
                                    usage,
                                    write_1h: None,
                                },
                            }];
                        }
                    }
                }
                _ => {}
            }
            true
        },
    )?;
    parsed.records.extend(task_records);
    if usage_records.is_empty() {
        parsed.records.extend(shutdown_fallback);
    } else {
        parsed.records.extend(usage_records);
    }
    if parsed.records.len() > MAX_RECORDS {
        parsed.records.truncate(MAX_RECORDS);
        warnings.insert(format!(
            "copilot-cli metadata records capped at {MAX_RECORDS} per file; coverage is incomplete."
        ));
    }
    parsed.warnings = warnings;
    Ok(parsed)
}

fn parse_junie_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let reader = BufReader::new(File::open(path)?.take(MAX_FILE_BYTES + 1));
    let dir_session = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .map(str::to_owned);
    let mut warnings = std::mem::take(&mut parsed.warnings);
    for_each_jsonl_value(
        reader,
        "junie",
        &mut warnings,
        |sequence, value, _warnings| {
            if value["kind"] != "SessionA2uxEvent" {
                return true;
            }
            let event = &value["event"];
            if event["state"].as_str() == Some("COMPLETED") {
                return true;
            }
            let agent_event = &event["agentEvent"];
            if agent_event["kind"] != "LlmResponseMetadataEvent" {
                return true;
            }
            let Some(model_usage) = agent_event["modelUsage"].as_array() else {
                return true;
            };
            let session = dir_session
                .clone()
                .or_else(|| identity(&value["taskId"]))
                .unwrap_or_else(|| "junie-session".to_owned());
            let time = value.get("timestampMs").and_then(timestamp_ms).unwrap_or(0);
            for (idx, entry) in model_usage.iter().enumerate() {
                let cached =
                    number(entry, "cacheReadTokens").or_else(|| number(entry, "cachedInputTokens"));
                let raw_in = number(entry, "inputTokens");
                let output = number(entry, "outputTokens");
                if raw_in.is_none() && output.is_none() && cached.is_none() {
                    continue;
                }
                let usage = TokenUsage {
                    input_tokens: raw_in,
                    cached_input_tokens: cached,
                    cache_write_tokens: None,
                    output_tokens: output,
                    reasoning_tokens: None,
                };
                let task_prefix = identity(&value["taskId"]).unwrap_or_else(|| session.clone());
                if !push_record(
                    &mut parsed,
                    "junie",
                    Record {
                        session: session.clone(),
                        context: Context {
                            model: label(&entry["model"], 128),
                            effort: String::new(),
                        },
                        time,
                        sequence,
                        forked: false,
                        kind: RecordKind::MessageUsage {
                            id: format!("{task_prefix}:{sequence}:{idx}"),
                            usage,
                            write_1h: None,
                        },
                    },
                ) {
                    return false;
                }
            }
            true
        },
    )?;
    parsed.warnings = warnings;
    Ok(parsed)
}

fn read_cline_history_model(ui_path: &Path) -> String {
    let history = ui_path.with_file_name("history_item.json");
    let Ok(Some(bytes)) = read_capped_bytes(&history, 65_536) else {
        return String::new();
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return String::new();
    };
    for key in ["modelId", "model_id", "model"] {
        let m = label(&doc[key], 128);
        if !m.is_empty() {
            return m;
        }
    }
    String::new()
}

fn parse_cline_like_file(path: &Path, provider: &str) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let Some(bytes) = read_capped_bytes(path, MAX_FILE_BYTES)? else {
        parsed.warnings.insert(format!(
            "Coverage excludes files larger than {} MiB.",
            MAX_FILE_BYTES / 1024 / 1024
        ));
        return Ok(parsed);
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            parsed.warnings.insert(format!(
                "{provider} malformed or unfinished JSON records were skipped; coverage is incomplete."
            ));
            return Ok(parsed);
        }
    };
    let items = match &doc {
        Value::Array(items) => items.as_slice(),
        Value::Object(map) => map
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    };
    let session = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .unwrap_or("task-session")
        .to_owned();
    let fallback_model = read_cline_history_model(path);
    for (idx, item) in items.iter().enumerate() {
        if item["type"] != "say" || item["say"] != "api_req_started" {
            continue;
        }
        let parsed_text = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| s.starts_with('{'))
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let src = parsed_text
            .as_ref()
            .filter(|v| {
                v.get("tokensIn").is_some()
                    || v.get("tokensOut").is_some()
                    || v.get("cacheReads").is_some()
                    || v.get("cacheWrites").is_some()
            })
            .unwrap_or(item);
        let raw_in = number(src, "tokensIn");
        let raw_out = number(src, "tokensOut");
        let cached = number(src, "cacheReads");
        let written = number(src, "cacheWrites");
        if raw_in.is_none() && raw_out.is_none() && cached.is_none() && written.is_none() {
            continue;
        }
        let input = raw_in.and_then(|i| {
            i.checked_add(cached.unwrap_or(0))?
                .checked_add(written.unwrap_or(0))
        });
        let usage = TokenUsage {
            input_tokens: input,
            cached_input_tokens: cached,
            cache_write_tokens: written,
            output_tokens: raw_out,
            reasoning_tokens: None,
        };
        let mut model = label(&src["model"], 128);
        if model.is_empty() {
            model = label(&src["modelId"], 128);
        }
        if model.is_empty() {
            model = fallback_model.clone();
        }
        let time = item.get("ts").and_then(timestamp_ms).unwrap_or(0);
        if !push_record(
            &mut parsed,
            provider,
            Record {
                session: session.clone(),
                context: Context {
                    model,
                    effort: String::new(),
                },
                time,
                sequence: idx + 1,
                forked: false,
                kind: RecordKind::MessageUsage {
                    id: format!("req:{idx}:{time}"),
                    usage,
                    write_1h: None,
                },
            },
        ) {
            break;
        }
    }
    Ok(parsed)
}

fn zed_decode_snapshot(data_type: Option<&str>, data: &[u8]) -> Option<Value> {
    let kind = data_type.unwrap_or("").trim().to_ascii_lowercase();
    match kind.as_str() {
        "json" => (data.len() <= MAX_ZED_BLOB_BYTES)
            .then(|| serde_json::from_slice(data).ok())
            .flatten(),
        "zstd" => zed_decode_zstd(data),
        "" => (data.len() <= MAX_ZED_BLOB_BYTES)
            .then(|| serde_json::from_slice(data).ok())
            .flatten()
            .or_else(|| zed_decode_zstd(data)),
        _ => None,
    }
}

fn zed_decode_zstd(data: &[u8]) -> Option<Value> {
    if data.is_empty() {
        return None;
    }
    let mut buffer = Vec::new();
    let mut decoder = zstd::stream::read::Decoder::new(data).ok()?;
    (&mut decoder)
        .take((MAX_ZED_BLOB_BYTES + 1) as u64)
        .read_to_end(&mut buffer)
        .ok()?;
    if buffer.len() > MAX_ZED_BLOB_BYTES {
        return None;
    }
    serde_json::from_slice(&buffer).ok()
}

fn zed_usage_from_obj(u: &Value) -> TokenUsage {
    let raw_in = number(u, "input_tokens");
    let cached = number(u, "cache_read_input_tokens");
    let written = number(u, "cache_creation_input_tokens");
    let input = raw_in.and_then(|i| {
        i.checked_add(cached.unwrap_or(0))?
            .checked_add(written.unwrap_or(0))
    });
    TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        cache_write_tokens: written,
        output_tokens: number(u, "output_tokens"),
        reasoning_tokens: None,
    }
}

fn zed_model_from_snapshot(snapshot: &Value) -> String {
    match snapshot.get("model") {
        Some(Value::String(_)) => label(&snapshot["model"], 128),
        Some(Value::Object(_)) => {
            let m = label(&snapshot["model"]["model"], 128);
            if !m.is_empty() {
                m
            } else {
                label(&snapshot["model"]["id"], 128)
            }
        }
        _ => String::new(),
    }
}

fn parse_zed_db(path: &Path) -> io::Result<ParsedFile> {
    let conn = crate::sources::open_database(path)
        .map_err(|_| io::Error::other("sqlite database open failed"))?;
    let mut parsed = ParsedFile::default();
    let stmt = conn
        .prepare("SELECT id, updated_at, created_at, data_type, data FROM threads")
        .or_else(|_| conn.prepare("SELECT * FROM threads"));
    let Ok(mut stmt) = stmt else {
        return Ok(parsed);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(parsed);
    };
    let mut sequence = 0usize;
    while let Some(row) = rows
        .next()
        .map_err(|_| io::Error::other("sqlite row read failed"))?
    {
        let Some(session) = crate::sources::column_text_opt(row, "id")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && s.len() <= 256)
        else {
            continue;
        };
        let Ok(data_idx) = row.as_ref().column_index("data") else {
            continue;
        };
        let data = match row.get_ref(data_idx) {
            Ok(rusqlite::types::ValueRef::Blob(b)) => b,
            Ok(rusqlite::types::ValueRef::Text(b)) => b,
            _ => continue,
        };
        let data_type = crate::sources::column_text_opt(row, "data_type");
        let Some(snapshot) = zed_decode_snapshot(data_type.as_deref(), data) else {
            continue;
        };
        let model = zed_model_from_snapshot(&snapshot);
        if let Some(req_map) = snapshot
            .get("request_token_usage")
            .and_then(Value::as_object)
            .filter(|m| !m.is_empty())
        {
            for (msg_id, u) in req_map {
                let usage = zed_usage_from_obj(u);
                if !usage.any() {
                    continue;
                }
                let time = u
                    .get("timestamp")
                    .or_else(|| u.get("created_at"))
                    .and_then(timestamp_ms)
                    .unwrap_or(0);
                let id = identity(&Value::String(msg_id.clone()))
                    .unwrap_or_else(|| format!("msg:{sequence}"));
                sequence += 1;
                if !push_record(
                    &mut parsed,
                    "zed",
                    Record {
                        session: session.clone(),
                        context: Context {
                            model: model.clone(),
                            effort: String::new(),
                        },
                        time,
                        sequence,
                        forked: false,
                        kind: RecordKind::MessageUsage {
                            id,
                            usage,
                            write_1h: None,
                        },
                    },
                ) {
                    return Ok(parsed);
                }
            }
        } else if let Some(cum) = snapshot
            .get("cumulative_token_usage")
            .filter(|v| v.is_object())
        {
            let usage = zed_usage_from_obj(cum);
            if usage.any() {
                sequence += 1;
                if !push_record(
                    &mut parsed,
                    "zed",
                    Record {
                        session,
                        context: Context {
                            model,
                            effort: String::new(),
                        },
                        time: 0,
                        sequence,
                        forked: false,
                        kind: RecordKind::MessageUsage {
                            id: "cumulative".to_owned(),
                            usage,
                            write_1h: None,
                        },
                    },
                ) {
                    return Ok(parsed);
                }
            }
        }
    }
    Ok(parsed)
}

fn parse_crush_db(path: &Path) -> io::Result<ParsedFile> {
    let conn = crate::sources::open_database(path)
        .map_err(|_| io::Error::other("sqlite database open failed"))?;
    let mut parsed = ParsedFile::default();
    let mut session_models = HashMap::<String, Option<String>>::new();
    if let Ok(mut stmt) = conn
        .prepare("SELECT session_id, model FROM messages WHERE role = 'assistant' ORDER BY created_at ASC, rowid ASC")
        .or_else(|_| conn.prepare("SELECT session_id, model FROM messages WHERE role = 'assistant'"))
        && let Ok(mut rows) = stmt.query([])
    {
        while let Some(row) = rows
            .next()
            .map_err(|_| io::Error::other("sqlite row read failed"))?
        {
            if let Some(sid) = crate::sources::column_text_opt(row, "session_id")
                && let Some(model) = crate::sources::column_text_opt(row, "model")
            {
                let m = label(&Value::String(model), 128);
                if !m.is_empty() {
                    session_models
                        .entry(sid)
                        .and_modify(|existing| {
                            if existing.as_deref() != Some(m.as_str()) {
                                *existing = None;
                            }
                        })
                        .or_insert(Some(m));
                }
            }
        }
    }
    let Ok(mut stmt) = conn.prepare("SELECT * FROM sessions") else {
        return Ok(parsed);
    };
    let Ok(mut rows) = stmt.query([]) else {
        return Ok(parsed);
    };
    let mut sequence = 0usize;
    while let Some(row) = rows
        .next()
        .map_err(|_| io::Error::other("sqlite row read failed"))?
    {
        let Some(session) = crate::sources::column_text_opt(row, "id")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && s.len() <= 256)
        else {
            continue;
        };
        let prompt = crate::sources::column_text_opt(row, "prompt_tokens")
            .and_then(|s| s.trim().parse::<u64>().ok());
        let completion = crate::sources::column_text_opt(row, "completion_tokens")
            .and_then(|s| s.trim().parse::<u64>().ok());
        if prompt.unwrap_or(0) == 0 && completion.unwrap_or(0) == 0 {
            continue;
        }
        let time = crate::sources::column_text_opt(row, "updated_at")
            .or_else(|| crate::sources::column_text_opt(row, "created_at"))
            .and_then(|s| parse_flexible_timestamp(&s))
            .unwrap_or(0);
        let model = session_models
            .get(&session)
            .and_then(|opt| opt.clone())
            .unwrap_or_default();
        sequence += 1;
        if !push_record(
            &mut parsed,
            "crush",
            Record {
                session,
                context: Context {
                    model,
                    effort: String::new(),
                },
                time,
                sequence,
                forked: false,
                kind: RecordKind::MessageUsage {
                    id: "session".to_owned(),
                    usage: TokenUsage {
                        input_tokens: prompt,
                        cached_input_tokens: None,
                        cache_write_tokens: None,
                        output_tokens: completion,
                        reasoning_tokens: None,
                    },
                    write_1h: None,
                },
            },
        ) {
            break;
        }
    }
    Ok(parsed)
}

fn feed_fnv1a(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    feed_fnv1a(&mut hash, bytes);
    hash
}

fn parse_aider_count(text: &str) -> Option<u64> {
    let cleaned = text.trim().replace(',', "");
    if cleaned.is_empty() {
        return None;
    }
    let last = cleaned.bytes().last()?;
    match last {
        b'k' | b'K' => {
            let v: f64 = cleaned[..cleaned.len() - 1].parse().ok()?;
            (v.is_finite() && v >= 0.0).then_some((v * 1_000.0).round() as u64)
        }
        b'm' | b'M' => {
            let v: f64 = cleaned[..cleaned.len() - 1].parse().ok()?;
            (v.is_finite() && v >= 0.0).then_some((v * 1_000_000.0).round() as u64)
        }
        _ => cleaned.parse::<u64>().ok(),
    }
}

fn parse_aider_tokens_line(rest: &str) -> Option<TokenUsage> {
    let prefix = rest.split(" Cost:").next().unwrap_or(rest);
    if prefix.len() > 512 {
        return None;
    }
    let mut sent = None;
    let mut received = None;
    let mut cache_read = None;
    let mut cache_write = None;
    let mut current = String::new();
    for raw_part in prefix.split(',') {
        let part_trimmed = raw_part.trim();
        if current.is_empty() {
            current.push_str(part_trimmed);
        } else {
            current.push(',');
            current.push_str(part_trimmed);
        }
        let p = current.trim().trim_end_matches('.').trim();
        let lower = p.to_ascii_lowercase();
        if let Some(num) = lower.strip_suffix(" sent") {
            sent = parse_aider_count(num);
            current.clear();
        } else if let Some(num) = lower.strip_suffix(" received") {
            received = parse_aider_count(num);
            current.clear();
        } else if let Some(num) = lower
            .strip_suffix(" cache reads")
            .or_else(|| lower.strip_suffix(" cache read"))
            .or_else(|| lower.strip_suffix(" cache hits"))
            .or_else(|| lower.strip_suffix(" cache hit"))
        {
            cache_read = parse_aider_count(num);
            current.clear();
        } else if let Some(num) = lower
            .strip_suffix(" cache writes")
            .or_else(|| lower.strip_suffix(" cache write"))
        {
            cache_write = parse_aider_count(num);
            current.clear();
        } else {
            // Only keep `current` across a comma if it is a pure numeric thousands prefix (e.g. "5" before "244 sent").
            let is_numeric_prefix = !p.is_empty()
                && p.len() <= 24
                && p.bytes().all(|b| b.is_ascii_digit() || b == b',');
            if !is_numeric_prefix {
                current.clear();
            }
        }
    }
    if sent.is_none() && received.is_none() {
        return None;
    }
    let input = sent.and_then(|s| {
        if cache_read.is_some() || cache_write.is_some() {
            let extra = cache_read
                .unwrap_or(0)
                .saturating_add(cache_write.unwrap_or(0));
            if extra > s {
                s.checked_add(extra)
            } else {
                Some(s)
            }
        } else {
            Some(s)
        }
    });
    Some(TokenUsage {
        input_tokens: input,
        cached_input_tokens: cache_read,
        cache_write_tokens: cache_write,
        output_tokens: received,
        reasoning_tokens: None,
    })
}

fn parse_aider_file(path: &Path) -> io::Result<ParsedFile> {
    let mut parsed = ParsedFile::default();
    let Some(bytes) = read_capped_bytes(path, MAX_FILE_BYTES)? else {
        parsed.warnings.insert(format!(
            "Coverage excludes files larger than {} MiB.",
            MAX_FILE_BYTES / 1024 / 1024
        ));
        return Ok(parsed);
    };
    let text = String::from_utf8_lossy(&bytes);
    let path_hash = fnv1a_64(path.to_string_lossy().as_bytes());
    let mut session_idx = 0usize;
    let mut session = format!("aider-{path_hash:016x}-0");
    let mut model = String::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim_end();
        if line.starts_with("# aider chat started at ") {
            session_idx += 1;
            session = format!("aider-{path_hash:016x}-{session_idx}");
        } else if let Some(rest) = line.strip_prefix("> Main model: ") {
            if let Some(tok) = rest.split_whitespace().next() {
                let m = label(&Value::String(tok.to_owned()), 128);
                if !m.is_empty() {
                    model = m;
                }
            }
        } else if let Some(rest) = line.strip_prefix("> Tokens: ")
            && let Some(usage) = parse_aider_tokens_line(rest)
        {
            let sequence = idx + 1;
            if !push_record(
                &mut parsed,
                "aider",
                Record {
                    session: session.clone(),
                    context: Context {
                        model: model.clone(),
                        effort: String::new(),
                    },
                    time: 0,
                    sequence,
                    forked: false,
                    kind: RecordKind::MessageUsage {
                        id: format!("line:{sequence}"),
                        usage,
                        write_1h: None,
                    },
                },
            ) {
                break;
            }
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
    daily_by_provider: BTreeMap<String, BTreeMap<String, UsageDay>>,
    since: Option<i64>,
    rates: &'a RateIndex,
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
    let mut messages = BTreeMap::new();
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
                usage, write_1h, ..
            } => {
                add_usage(
                    totals, provider, session, record, *usage, *write_1h, warnings,
                );
            }
            RecordKind::MessageUsage {
                id,
                usage,
                write_1h,
            } => {
                messages.insert(id, (provider, record, *usage, *write_1h));
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
    for (_, (provider, record, usage, write_1h)) in messages {
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

fn sum_opt(target: &mut Option<u64>, value: Option<u64>, first: bool) {
    *target = if first {
        value
    } else {
        target.and_then(|a| a.checked_add(value?))
    };
}

fn add_day_usage(
    days: &mut BTreeMap<String, UsageDay>,
    date: String,
    usage: TokenUsage,
    cost: Option<f64>,
) {
    let day = days.entry(date.clone()).or_insert_with(|| UsageDay {
        date,
        ..UsageDay::default()
    });
    let first = day.requests == 0;
    sum_opt(&mut day.input_tokens, usage.input_tokens, first);
    sum_opt(&mut day.output_tokens, usage.output_tokens, first);
    sum_opt(
        &mut day.cached_input_tokens,
        usage.cached_input_tokens,
        first,
    );
    day.estimated_cost_usd = sum_cost(day.estimated_cost_usd, cost, first);
    day.requests = day.requests.saturating_add(1);
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
        .resolve(provider, &record.context.model)
        .and_then(|rate| estimate_usage(provider, usage, write_1h, rate, warnings));
    let aggregate = row_for(
        &mut totals.rows,
        provider,
        session,
        &record.context,
        record.time,
    );
    let first = !aggregate.has_usage;
    sum_opt(&mut aggregate.row.input_tokens, usage.input_tokens, first);
    sum_opt(
        &mut aggregate.row.cached_input_tokens,
        usage.cached_input_tokens,
        first,
    );
    sum_opt(
        &mut aggregate.row.cache_write_tokens,
        usage.cache_write_tokens,
        first,
    );
    sum_opt(&mut aggregate.row.output_tokens, usage.output_tokens, first);
    sum_opt(
        &mut aggregate.row.reasoning_tokens,
        usage.reasoning_tokens,
        first,
    );
    aggregate.row.estimated_cost_usd = sum_cost(aggregate.row.estimated_cost_usd, cost, first);
    aggregate.row.requests = aggregate.row.requests.saturating_add(1);
    aggregate.has_usage = true;
    if record.time > 0
        && let Some(date) = Local.timestamp_millis_opt(record.time).single()
    {
        let date = date.format("%Y-%m-%d").to_string();
        add_day_usage(&mut totals.daily, date.clone(), usage, cost);
        add_day_usage(
            totals
                .daily_by_provider
                .entry(provider.to_owned())
                .or_default(),
            date,
            usage,
            cost,
        );
    }
}

fn sum_cost(previous: Option<f64>, cost: Option<f64>, first: bool) -> Option<f64> {
    let sum = if first { cost? } else { previous? + cost? };
    sum.is_finite().then_some(sum)
}

#[derive(Clone, Deserialize)]
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
    #[serde(default)]
    google_source: Option<String>,
    #[serde(default)]
    deepseek_source: Option<String>,
    rates: Vec<Rate>,
}

struct RateIndex {
    exact: BTreeMap<(String, String), Rate>,
    builtin_by_model: BTreeMap<String, Option<Rate>>,
}

impl RateIndex {
    fn resolve(&self, provider: &str, model: &str) -> Option<&Rate> {
        self.exact
            .get(&(provider.to_owned(), model.to_owned()))
            .or_else(|| self.builtin_by_model.get(model).and_then(Option::as_ref))
    }
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
    file.write_all(b"# Optional overrides for bundled standard API base rates, USD per million tokens.\n# Built-in official prices were checked 2026-11-19. Unknown exact model IDs stay unpriced.\n# Sources: https://developers.openai.com/api/docs/pricing\n# https://platform.claude.com/docs/en/about-claude/pricing\n# https://ai.google.dev/gemini-api/docs/pricing\n# https://api-docs.deepseek.com/quick_start/pricing\n# Estimates are not subscription bills. Context, speed, time-of-day, region and unrecorded write premiums are excluded.\n# Supported providers: codex, claude-code, antigravity, deepseek-harness, gemini-cli,\n# qwen-code, opencode, kilo-code, factory-droid, copilot-cli, junie, cline, roo-code, zed, crush, aider.\n# Model must match the displayed ID.\n# Cached input and cache writes are subsets of inclusive input.\n# A custom cache-write rate is an effective price and overrides automatic TTL pricing.\n# Uncomment and replace each placeholder with a finite nonnegative number.\n# [[rates]]\n# provider = \"codex\"\n# model = \"exact-model-id\"\n# input_per_million = YOUR_RATE\n# cached_input_per_million = YOUR_RATE\n# output_per_million = YOUR_RATE\n# cache_write_per_million = YOUR_RATE # required for nonzero writes\n")
}

fn load_rates(path: &Path, warnings: &mut BTreeSet<String>) -> (String, RateIndex) {
    let catalog: Catalog = toml::from_str(include_str!("model_prices.toml"))
        .expect("bundled official model prices must be valid TOML");
    debug_assert_eq!(catalog.checked_at, "2026-11-19");
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
    debug_assert!(
        catalog
            .google_source
            .as_deref()
            .is_some_and(|s| s.starts_with("https://ai.google.dev/"))
    );
    debug_assert!(
        catalog
            .deepseek_source
            .as_deref()
            .is_some_and(|s| s.starts_with("https://api-docs.deepseek.com/"))
    );
    let mut exact = BTreeMap::new();
    let mut builtin_by_model: BTreeMap<String, Option<Rate>> = BTreeMap::new();
    for mut rate in catalog.rates {
        rate.builtin = true;
        builtin_by_model
            .entry(rate.model.clone())
            .and_modify(|slot| *slot = None)
            .or_insert_with(|| Some(rate.clone()));
        exact.insert((rate.provider.clone(), rate.model.clone()), rate);
    }
    exact.extend(load_custom_rates(path, warnings));
    (
        catalog.checked_at,
        RateIndex {
            exact,
            builtin_by_model,
        },
    )
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
            || !is_supported_provider_slug(&rate.provider)
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
    provider: &str,
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
        None if provider == "codex" => {
            if !rate.builtin
                || rate
                    .cache_write_per_million
                    .is_some_and(|price| price > rate.input_per_million)
            {
                warnings.insert("Codex cache-write counts are missing. Base-rate approximation excludes unrecorded write premiums, long-context, speed/service-tier and region premiums.".to_owned());
            }
            0
        }
        None if provider == "claude-code" => return None,
        None => {
            if rate
                .cache_write_per_million
                .is_some_and(|price| price > rate.input_per_million)
            {
                warnings.insert(format!("{provider} cache-write counts are missing. Base-rate approximation excludes unrecorded write premiums, long-context, speed/service-tier and region premiums."));
            }
            0
        }
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
            if rate.builtin && rate.provider == "codex" && default_price > rate.input_per_million {
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
            self.write_text(name, &text);
        }
        fn write_text(&self, rel: &str, text: &str) {
            self.write_bytes(rel, text.as_bytes());
        }
        fn write_bytes(&self, rel: &str, bytes: &[u8]) {
            let full = self.0.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, bytes).unwrap();
        }
        fn scanner(&self, provider: Provider) -> UsageScanner {
            self.scanner_store(provider, SessionStore::LogFiles)
        }
        fn scanner_store(&self, provider: Provider, store: SessionStore) -> UsageScanner {
            UsageScanner::new(
                vec![SessionSource {
                    provider,
                    root: self.0.clone(),
                    store,
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
        assert_eq!(snapshot.pricing_checked_at, "2026-11-19");
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

    #[test]
    fn codex_zst_archives_and_claude_api_request_effort_and_thinking_tokens_are_parsed() {
        let f = Fixture::new();
        let codex_lines = [
            meta(),
            context("gpt-6.1-terra", "medium"),
            total("2026-11-10T10:00:00Z", 200, 40),
        ]
        .iter()
        .map(|v| serde_json::to_string(v).unwrap() + "\n")
        .collect::<String>();
        let compressed = zstd::stream::encode_all(codex_lines.as_bytes(), 0).unwrap();
        f.write_bytes("rollout-2026-11-10-test.jsonl.zst", &compressed);
        let codex_snap = f.scanner(Provider::Codex).scan();
        assert_eq!(codex_snap.rows.len(), 1);
        assert_eq!(codex_snap.rows[0].provider, "codex");
        assert_eq!(codex_snap.rows[0].model, "gpt-6.1-terra");
        assert_eq!(codex_snap.rows[0].input_tokens, Some(200));
        assert_eq!(codex_snap.rows[0].output_tokens, Some(40));

        let f2 = Fixture::new();
        f2.write(
            "claude.jsonl",
            &[
                json!({
                    "type": "api-request",
                    "sessionId": "claude-eff",
                    "timestamp": "2026-11-02T02:57:34Z",
                    "params": {"model": "claude-opus-5-5", "output_config": {"effort": "high"}}
                }),
                json!({
                    "type": "assistant",
                    "sessionId": "claude-eff",
                    "timestamp": "2026-11-02T02:57:36Z",
                    "message": {
                        "id": "msg-1",
                        "model": "claude-opus-5-5",
                        "usage": {
                            "input_tokens": 100,
                            "cache_read_input_tokens": 200,
                            "cache_creation_input_tokens": 50,
                            "output_tokens": 80,
                            "output_tokens_details": {"thinking_tokens": 25}
                        }
                    }
                }),
            ],
        );
        let claude_snap = f2.scanner(Provider::ClaudeCode).scan();
        assert_eq!(claude_snap.rows.len(), 1);
        assert_eq!(claude_snap.rows[0].effort, "high");
        assert_eq!(claude_snap.rows[0].input_tokens, Some(350));
        assert_eq!(claude_snap.rows[0].reasoning_tokens, Some(25));
    }

    #[test]
    fn antigravity_transcripts_normalize_cache_carry_model_and_dedup_steps() {
        let f = Fixture::new();
        f.write(
            "conv-1/.system_generated/logs/transcript.jsonl",
            &[
                json!({
                    "step_index": 0,
                    "type": "USER_INPUT",
                    "content": "PRIVATE_USER_PROMPT",
                    "created_at": "2026-11-19T10:00:00Z"
                }),
                json!({
                    "step_index": 1,
                    "type": "PLANNER_RESPONSE",
                    "model": "gemini-pro-orchestrator-neon",
                    "content": "PRIVATE_MODEL_REPLY",
                    "created_at": "2026-11-19T10:00:10Z",
                    "input_tokens": 300,
                    "cache_read_tokens": 700,
                    "output_tokens": 50
                }),
                json!({
                    "step_index": 1,
                    "type": "PLANNER_RESPONSE",
                    "model": "gemini-pro-orchestrator-neon",
                    "created_at": "2026-11-19T10:00:10Z",
                    "input_tokens": 300,
                    "cache_read_tokens": 700,
                    "output_tokens": 50
                }),
                json!({
                    "step_index": 2,
                    "type": "CHECKPOINT",
                    "created_at": "2026-11-19T10:01:00Z",
                    "input_tokens": 100,
                    "cache_read_tokens": 400,
                    "output_tokens": 30
                }),
            ],
        );
        f.write(
            "conv-1/.system_generated/logs/browser_sandbox/transcript.jsonl",
            &[json!({
                "step_index": 0,
                "type": "PLANNER_RESPONSE",
                "model": "should-be-skipped",
                "created_at": "2026-11-19T10:02:00Z",
                "input_tokens": 9999,
                "cache_read_tokens": 0,
                "output_tokens": 999
            })],
        );
        let snap = f.scanner(Provider::Antigravity).scan();
        assert_eq!(snap.rows.len(), 1);
        let row = &snap.rows[0];
        assert_eq!(row.provider, "antigravity");
        assert_eq!(row.model, "gemini-pro-orchestrator-neon");
        assert_eq!(row.input_tokens, Some(1500));
        assert_eq!(row.cached_input_tokens, Some(1100));
        assert_eq!(row.output_tokens, Some(80));
        assert_eq!(row.requests, 2);
        assert_eq!(row.sessions, 1);
        let debug = format!("{snap:?}");
        assert!(!debug.contains("PRIVATE_USER_PROMPT"));
        assert!(!debug.contains("PRIVATE_MODEL_REPLY"));
        assert!(!debug.contains("conv-1"));
    }

    #[test]
    fn deepseek_harness_zstd_frames_seed_filtering_turn_time_and_official_pricing() {
        let f = Fixture::new();
        let make_frame = |vals: &[Value]| {
            let text = vals
                .iter()
                .map(|v| serde_json::to_string(v).unwrap() + "\n")
                .collect::<String>();
            zstd::stream::encode_all(text.as_bytes(), 0).unwrap()
        };
        let frame1 = make_frame(&[
            json!({"type": "session", "version": 4, "id": "dsh-sess-1", "isSeeded": false}),
            json!({"type": "request/header", "time": 1_793_589_111_000i64, "data": {"header": {"config": {"model": "deepseek-flash", "reasoningEffort": "max"}}}}),
            json!({"type": "turn/start", "time": 1_793_589_111_000i64, "data": {"turn": 1}}),
            json!({"type": "assistant/message", "seq": 10, "time": 1_793_589_112_000i64, "data": {"message": {"id": "m-1", "source": {"model": "deepseek-flash"}}, "usage": {"inputTokens": 400_000, "cacheReadTokens": 600_000, "outputTokens": 100_000, "reasoningTokens": 20_000, "totalTokens": 1_100_000}}}),
        ]);
        let frame2 = make_frame(&[
            json!({"type": "session/end-seed", "time": 1_793_589_113_000i64}),
            json!({"type": "turn/end", "time": 1_793_589_116_000i64, "data": {"turn": 1}}),
        ]);
        let mut combined = frame1;
        combined.extend_from_slice(&frame2);
        f.write_bytes("s1/session.v4.jsonl.zstd", &combined);

        let seeded_frame = make_frame(&[
            json!({"type": "session", "version": 4, "id": "dsh-seeded", "isSeeded": true}),
            json!({"type": "assistant/message", "seq": 1, "time": 1_793_589_200_000i64, "data": {"message": {"id": "seed-m", "source": {"model": "deepseek-flash"}}, "usage": {"inputTokens": 9999, "outputTokens": 999}}}),
            json!({"type": "session/end-seed", "time": 1_793_589_201_000i64}),
            json!({"type": "assistant/message", "seq": 2, "time": 1_793_589_202_000i64, "data": {"message": {"id": "post-seed", "source": {"model": "deepseek-flash"}}, "usage": {"inputTokens": 100, "outputTokens": 50}}}),
        ]);
        f.write_bytes("s2/session.v4.jsonl.zstd", &seeded_frame);

        let snap = f.scanner(Provider::DeepSeekHarness).scan();
        assert_eq!(snap.rows.len(), 2);
        let maxed = snap.rows.iter().find(|r| r.effort == "max").unwrap();
        assert_eq!(maxed.provider, "deepseek-harness");
        assert_eq!(maxed.input_tokens, Some(1_000_000));
        assert_eq!(maxed.cached_input_tokens, Some(600_000));
        assert_eq!(maxed.output_tokens, Some(100_000));
        assert_eq!(maxed.reasoning_tokens, Some(20_000));
        assert_eq!(maxed.task_time_ms, Some(5_000));
        // 0.4M * 0.30 + 0.6M * 0.006 + 0.1M * 1.20 = 0.12 + 0.0036 + 0.12 = 0.2436
        assert!((maxed.estimated_cost_usd.unwrap() - 0.2436).abs() < 1e-12);

        let post = snap.rows.iter().find(|r| r.effort.is_empty()).unwrap();
        assert_eq!(post.input_tokens, Some(100));
        assert_eq!(post.output_tokens, Some(50));
    }

    #[test]
    fn gemini_cli_json_and_jsonl_snapshots_normalize_thoughts_and_official_rates() {
        let f = Fixture::new();
        let json_doc = json!({
            "sessionId": "gem-json-1",
            "messages": [
                {
                    "id": "g-1",
                    "timestamp": "2026-03-26T20:49:36Z",
                    "type": "gemini",
                    "model": "gemini-3-flash-preview",
                    "tokens": {"input": 1_000_000, "cached": 500_000, "output": 80_000, "thoughts": 20_000, "tool": 0, "total": 1_100_000}
                }
            ]
        });
        f.write_text(
            "proj1/chats/session-1.json",
            &serde_json::to_string(&json_doc).unwrap(),
        );
        f.write(
            "proj2/chats/session-2.jsonl",
            &[
                json!({"sessionId": "gem-jsonl-1", "projectHash": "abc"}),
                json!({"$set": {"messages": [
                    {"id": "g-2", "timestamp": "2026-03-27T10:00:00Z", "type": "gemini", "model": "gemini-2.5-pro", "tokens": {"input": 1_000, "cached": 0, "output": 100, "thoughts": 50}}
                ]}}),
                json!({"$set": {"messages": [
                    {"id": "g-2", "timestamp": "2026-03-27T10:00:00Z", "type": "gemini", "model": "gemini-2.5-pro", "tokens": {"input": 1_000, "cached": 200, "output": 150, "thoughts": 50}}
                ]}}),
            ],
        );
        let snap = f
            .scanner_store(Provider::GeminiCli, SessionStore::JsonLineSnapshot)
            .scan();
        assert_eq!(snap.rows.len(), 2);
        let flash = snap
            .rows
            .iter()
            .find(|r| r.model == "gemini-3-flash-preview")
            .unwrap();
        assert_eq!(flash.input_tokens, Some(1_000_000));
        assert_eq!(flash.cached_input_tokens, Some(500_000));
        assert_eq!(flash.output_tokens, Some(100_000));
        assert_eq!(flash.reasoning_tokens, Some(20_000));
        // 0.5M * 0.50 + 0.5M * 0.05 + 0.1M * 3.00 = 0.25 + 0.025 + 0.30 = 0.575
        assert!((flash.estimated_cost_usd.unwrap() - 0.575).abs() < 1e-12);

        let pro = snap
            .rows
            .iter()
            .find(|r| r.model == "gemini-2.5-pro")
            .unwrap();
        assert_eq!(pro.requests, 1);
        assert_eq!(pro.cached_input_tokens, Some(200));
        assert_eq!(pro.output_tokens, Some(200));
    }

    #[test]
    fn qwen_code_prefers_ui_telemetry_and_falls_back_to_assistant_usage() {
        let f = Fixture::new();
        f.write(
            "proj1/chats/both.jsonl",
            &[
                json!({
                    "sessionId": "qwen-1",
                    "uuid": "u-1",
                    "timestamp": "2026-03-26T15:19:27Z",
                    "type": "system",
                    "subtype": "ui_telemetry",
                    "systemPayload": {"uiEvent": {
                        "event.name": "qwen-code.api_response",
                        "event.timestamp": "2026-03-26T15:19:27Z",
                        "response_id": "resp-1",
                        "model": "coder-model",
                        "input_token_count": 500,
                        "cached_content_token_count": 200,
                        "output_token_count": 60,
                        "thoughts_token_count": 10
                    }}
                }),
                json!({
                    "sessionId": "qwen-1",
                    "uuid": "u-2",
                    "timestamp": "2026-03-26T15:19:27Z",
                    "type": "assistant",
                    "model": "coder-model",
                    "usageMetadata": {
                        "promptTokenCount": 500,
                        "cachedContentTokenCount": 200,
                        "candidatesTokenCount": 60,
                        "thoughtsTokenCount": 10
                    }
                }),
            ],
        );
        f.write(
            "proj2/chats/fallback.jsonl",
            &[json!({
                "sessionId": "qwen-2",
                "uuid": "u-3",
                "timestamp": "2026-03-26T16:00:00Z",
                "type": "assistant",
                "model": "coder-model",
                "usageMetadata": {
                    "promptTokenCount": 300,
                    "cachedContentTokenCount": 100,
                    "candidatesTokenCount": 40,
                    "thoughtsTokenCount": 5
                }
            })],
        );
        let snap = f.scanner(Provider::QwenCode).scan();
        assert_eq!(snap.rows.len(), 1);
        let row = &snap.rows[0];
        assert_eq!(row.provider, "qwen-code");
        assert_eq!(row.input_tokens, Some(800));
        assert_eq!(row.cached_input_tokens, Some(300));
        assert_eq!(row.output_tokens, Some(115));
        assert_eq!(row.reasoning_tokens, Some(15));
        assert_eq!(row.requests, 2);
        assert_eq!(row.sessions, 2);
    }

    #[test]
    fn opencode_and_kilo_sqlite_deduplicate_v1_v2_skip_aborted_and_invalidate_wal() {
        let f = Fixture::new();
        let db_path = f.0.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        let v2_data = json!({
            "time": {"created": 1_793_044_661_000i64, "completed": 1_793_044_663_000i64},
            "model": {"id": "claude-sonnet-4-5", "providerID": "anthropic", "variant": "medium"},
            "tokens": {"input": 100, "output": 40, "reasoning": 10, "cache": {"read": 200, "write": 50}}
        });
        let v1_dup = json!({
            "role": "assistant",
            "time": {"created": 1_793_044_661_000i64},
            "modelID": "claude-sonnet-4-5",
            "variant": "medium",
            "tokens": {"input": 100, "output": 40, "reasoning": 10, "cache": {"read": 200, "write": 50}}
        });
        let v1_aborted = json!({
            "role": "assistant",
            "time": {"created": 1_793_044_670_000i64},
            "modelID": "claude-sonnet-4-5",
            "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        });
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, time_created, data) VALUES (?1, ?2, 'assistant', ?3, ?4)",
            rusqlite::params!["m-1", "ses-1", 1_793_044_661_000i64, v2_data.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["m-1", "ses-1", 1_793_044_661_000i64, v1_dup.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "m-zero",
                "ses-1",
                1_793_044_670_000i64,
                v1_aborted.to_string()
            ],
        )
        .unwrap();

        let mut scanner = f.scanner_store(Provider::OpenCode, SessionStore::Sqlite);
        let snap1 = scanner.scan();
        assert_eq!(snap1.rows.len(), 1);
        assert_eq!(snap1.rows[0].provider, "opencode");
        assert_eq!(snap1.rows[0].effort, "medium");
        assert_eq!(snap1.rows[0].input_tokens, Some(350));
        assert_eq!(snap1.rows[0].cached_input_tokens, Some(200));
        assert_eq!(snap1.rows[0].cache_write_tokens, Some(50));
        assert_eq!(snap1.rows[0].output_tokens, Some(50));
        assert_eq!(snap1.rows[0].reasoning_tokens, Some(10));
        assert_eq!(snap1.rows[0].requests, 1);
        assert!(snap1.rows[0].estimated_cost_usd.is_some());

        // Write a new message without checkpointing WAL; cache must notice -wal changes while writer is open.
        let main_meta_before = fs::metadata(&db_path).unwrap();
        let v2_more = json!({
            "time": {"created": 1_793_044_700_000i64},
            "model": {"id": "claude-sonnet-4-5", "variant": "medium"},
            "tokens": {"input": 50, "output": 20, "reasoning": 0, "cache": {"read": 100, "write": 0}}
        });
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, time_created, data) VALUES (?1, ?2, 'assistant', ?3, ?4)",
            rusqlite::params!["m-2", "ses-1", 1_793_044_700_000i64, v2_more.to_string()],
        )
        .unwrap();
        let main_meta_after = fs::metadata(&db_path).unwrap();
        assert_eq!(main_meta_before.len(), main_meta_after.len());
        let snap2 = scanner.scan();
        assert_eq!(snap2.rows[0].requests, 2);
        assert_eq!(snap2.rows[0].input_tokens, Some(500));
        drop(conn);

        // Kilo Code shares the same DB format.
        fs::copy(&db_path, f.0.join("kilo.db")).unwrap();
        let kilo_snap = f
            .scanner_store(Provider::KiloCode, SessionStore::Sqlite)
            .scan();
        assert_eq!(kilo_snap.rows.len(), 1);
        assert_eq!(kilo_snap.rows[0].provider, "kilo-code");
    }

    #[test]
    fn factory_droid_copilot_junie_cline_roo_zed_crush_and_aider_parse_correctly() {
        // Factory Droid: reads tokenUsage (not inclusiveTokenUsage), ignores .bak
        let f_droid = Fixture::new();
        f_droid.write_text(
            "proj/sess-1.settings.json",
            &json!({
                "model": "custom:sub2api-opus-5-5",
                "reasoningEffort": "high",
                "assistantActiveTimeMs": 99_999,
                "tokenUsage": {"inputTokens": 100, "outputTokens": 40, "cacheCreationTokens": 20, "cacheReadTokens": 80, "thinkingTokens": 10},
                "inclusiveTokenUsage": {"inputTokens": 999_999, "outputTokens": 999_999, "cacheCreationTokens": 0, "cacheReadTokens": 0}
            })
            .to_string(),
        );
        f_droid.write(
            "proj/sess-1.jsonl",
            &[json!({"type": "message", "timestamp": "2026-10-31T06:00:00Z"})],
        );
        f_droid.write_text(
            "proj/sess-1.settings.json.bak",
            &json!({"tokenUsage": {"inputTokens": 5000, "outputTokens": 5000, "cacheCreationTokens": 0, "cacheReadTokens": 0}}).to_string(),
        );
        let droid_snap = f_droid.scanner(Provider::FactoryDroid).scan();
        assert_eq!(droid_snap.rows.len(), 1);
        assert_eq!(droid_snap.rows[0].input_tokens, Some(200));
        assert_eq!(droid_snap.rows[0].output_tokens, Some(40));
        assert_eq!(droid_snap.rows[0].task_time_ms, None);

        // Copilot CLI
        let f_cop = Fixture::new();
        f_cop.write(
            "sess-abc/events.jsonl",
            &[
                json!({"id": "e1", "type": "session.start", "timestamp": "2026-11-01T10:00:00Z", "data": {"sessionId": "cop-1", "selectedModel": "gpt-5.4"}}),
                json!({"id": "e2", "type": "assistant.turn_start", "timestamp": "2026-11-01T10:00:01Z", "data": {"turnId": "t1"}}),
                json!({"id": "e3", "type": "assistant.usage", "timestamp": "2026-11-01T10:00:05Z", "data": {"inputTokens": 1000, "outputTokens": 100, "cacheReadTokens": 400, "cacheWriteTokens": 0}}),
                json!({"id": "e4", "type": "assistant.turn_end", "timestamp": "2026-11-01T10:00:06Z", "data": {"turnId": "t1"}}),
            ],
        );
        let cop_snap = f_cop.scanner(Provider::CopilotCli).scan();
        assert_eq!(cop_snap.rows.len(), 1);
        assert_eq!(cop_snap.rows[0].input_tokens, Some(1000));
        assert_eq!(cop_snap.rows[0].task_time_ms, Some(5000));
        assert!(cop_snap.rows[0].estimated_cost_usd.is_some());

        // Junie (skips COMPLETED replay)
        let f_junie = Fixture::new();
        f_junie.write(
            "j1/events.jsonl",
            &[
                json!({"kind": "SessionA2uxEvent", "taskId": "task-1", "timestampMs": 1_788_257_705_100i64, "event": {"state": "IN_PROGRESS", "agentEvent": {"kind": "LlmResponseMetadataEvent", "modelUsage": [{"model": "claude-sonnet-4-5", "inputTokens": 1200, "outputTokens": 80}]}}}),
                json!({"kind": "SessionA2uxEvent", "taskId": "task-1", "timestampMs": 1_788_257_709_200i64, "event": {"state": "COMPLETED", "agentEvent": {"kind": "LlmResponseMetadataEvent", "modelUsage": [{"model": "claude-sonnet-4-5", "inputTokens": 1200, "outputTokens": 80}]}}}),
            ],
        );
        let junie_snap = f_junie.scanner(Provider::JetBrains).scan();
        assert_eq!(junie_snap.rows.len(), 1);
        assert_eq!(junie_snap.rows[0].input_tokens, Some(1200));
        assert_eq!(junie_snap.rows[0].requests, 1);

        // Cline & Roo Code
        let f_cline = Fixture::new();
        f_cline.write_text(
            "tasks/t1/history_item.json",
            &json!({"modelId": "claude-sonnet-4-6"}).to_string(),
        );
        f_cline.write_text(
            "tasks/t1/ui_messages.json",
            &json!([
                {"ts": 1_755_906_774_129i64, "type": "say", "say": "api_req_started", "text": json!({"request": "SECRET_PROMPT_TEXT", "tokensIn": 100, "tokensOut": 50, "cacheWrites": 20, "cacheReads": 80}).to_string()}
            ]).to_string(),
        );
        let cline_snap = f_cline
            .scanner_store(Provider::Cline, SessionStore::JsonFiles)
            .scan();
        assert_eq!(cline_snap.rows.len(), 1);
        assert_eq!(cline_snap.rows[0].model, "claude-sonnet-4-6");
        assert_eq!(cline_snap.rows[0].input_tokens, Some(200));
        assert!(!format!("{cline_snap:?}").contains("SECRET_PROMPT_TEXT"));

        // Zed SQLite zstd thread blob
        let f_zed = Fixture::new();
        let zed_db = f_zed.0.join("threads.db");
        let zconn = rusqlite::Connection::open(&zed_db).unwrap();
        zconn
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, summary TEXT, updated_at TEXT, created_at TEXT, data_type TEXT, data BLOB);",
            )
            .unwrap();
        let zed_blob = json!({
            "version": "0.3.0",
            "model": {"provider": "anthropic", "model": "claude-sonnet-4-5"},
            "request_token_usage": {
                "msg-1": {"input_tokens": 50, "output_tokens": 25, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10}
            }
        });
        let zed_zst = zstd::stream::encode_all(zed_blob.to_string().as_bytes(), 0).unwrap();
        zconn
            .execute(
                "INSERT INTO threads (id, summary, updated_at, created_at, data_type, data) VALUES ('th-1', 'SECRET', '2026-10-21T07:12:33Z', '2026-10-21T07:12:00Z', 'zstd', ?1)",
                rusqlite::params![zed_zst],
            )
            .unwrap();
        drop(zconn);
        let zed_snap = f_zed
            .scanner_store(Provider::Zed, SessionStore::Sqlite)
            .scan();
        assert_eq!(zed_snap.rows.len(), 1);
        assert_eq!(zed_snap.rows[0].input_tokens, Some(160));
        assert_eq!(zed_snap.rows[0].output_tokens, Some(25));

        // Crush SQLite optional columns
        let f_crush = Fixture::new();
        let crush_db = f_crush.0.join("crush.db");
        let cconn = rusqlite::Connection::open(&crush_db).unwrap();
        cconn
            .execute_batch(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, created_at INTEGER, updated_at INTEGER, prompt_tokens INTEGER, completion_tokens INTEGER);
                 CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, model TEXT, created_at INTEGER);
                 INSERT INTO sessions VALUES ('c1', 1789189700, 1789189720, 420, 80);
                 INSERT INTO messages VALUES ('m1', 'c1', 'assistant', 'gpt-5.4', 1789189710);",
            )
            .unwrap();
        drop(cconn);
        let crush_snap = f_crush
            .scanner_store(Provider::Crush, SessionStore::Sqlite)
            .scan();
        assert_eq!(crush_snap.rows.len(), 1);
        assert_eq!(crush_snap.rows[0].input_tokens, Some(420));
        assert_eq!(crush_snap.rows[0].output_tokens, Some(80));

        // Aider Markdown
        let f_aider = Fixture::new();
        f_aider.write_text(
            "repo/.aider.chat.history.md",
            "# aider chat started at 2026-10-21 08:13:45\n\
             #### SECRET_USER_PROMPT\n\
             > Main model: gpt-5.4 with diff edit format\n\
             > Tokens: 5,244 sent, 604 received. Cost: $0.02 request, $0.02 session.\n\
             > Tokens: 5.2k sent, 1.1k received, 200 cache write, 4.0k cache read.\n",
        );
        let aider_snap = f_aider.scanner(Provider::Aider).scan();
        assert_eq!(aider_snap.rows.len(), 1);
        assert_eq!(aider_snap.rows[0].model, "gpt-5.4");
        assert_eq!(aider_snap.rows[0].input_tokens, Some(10_444));
        assert_eq!(aider_snap.rows[0].output_tokens, Some(1_704));
        assert_eq!(aider_snap.rows[0].requests, 2);
        assert!(!format!("{aider_snap:?}").contains("SECRET_USER_PROMPT"));
    }

    #[test]
    fn cross_harness_official_pricing_custom_overrides_daily_by_provider_and_missing_dirs() {
        let f = Fixture::new();
        // OpenCode uses official Claude model automatically, plus custom override for Gemini CLI.
        let db_path = f.0.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        let oc_data = json!({
            "time": {"created": local_ms(2026, 11, 10, 12, 0)},
            "model": {"id": "claude-sonnet-4-5"},
            "tokens": {"input": 100, "output": 50, "reasoning": 0, "cache": {"read": 200, "write": 100}}
        });
        conn.execute(
            "INSERT INTO session_message VALUES ('oc-1', 's1', 'assistant', ?1, ?2)",
            rusqlite::params![local_ms(2026, 11, 10, 12, 0), oc_data.to_string()],
        )
        .unwrap();
        drop(conn);

        f.write_text(
            "chats/gem.json",
            &json!({
                "sessionId": "g-1",
                "messages": [{
                    "id": "gm-1",
                    "timestamp": "2026-11-11T12:00:00Z",
                    "type": "gemini",
                    "model": "custom-gemini-id",
                    "tokens": {"input": 1_000_000, "cached": 0, "output": 1_000_000, "thoughts": 0}
                }]
            })
            .to_string(),
        );
        fs::write(
            f.0.join("rates.toml"),
            "[[rates]]\nprovider='gemini-cli'\nmodel='custom-gemini-id'\ninput_per_million=2.0\ncached_input_per_million=0.2\noutput_per_million=8.0\n",
        )
        .unwrap();

        let missing = f.0.join("does-not-exist");
        let mut scanner = UsageScanner::new(
            vec![
                SessionSource {
                    provider: Provider::OpenCode,
                    root: f.0.clone(),
                    store: SessionStore::Sqlite,
                },
                SessionSource {
                    provider: Provider::GeminiCli,
                    root: f.0.clone(),
                    store: SessionStore::JsonLineSnapshot,
                },
                SessionSource {
                    provider: Provider::Goose,
                    root: missing,
                    store: SessionStore::Sqlite,
                },
            ],
            f.0.join("rates.toml"),
        );
        let snap = scanner.scan();
        assert_eq!(snap.rows.len(), 2);
        assert!(!snap.warnings.iter().any(|w| w.contains("is missing")));
        let oc_row = snap.rows.iter().find(|r| r.provider == "opencode").unwrap();
        assert!(oc_row.estimated_cost_usd.is_some());
        let gem_row = snap
            .rows
            .iter()
            .find(|r| r.provider == "gemini-cli")
            .unwrap();
        assert!((gem_row.estimated_cost_usd.unwrap() - 10.0).abs() < 1e-12);
        assert_eq!(snap.daily_by_provider["opencode"].len(), 1);
        assert_eq!(snap.daily_by_provider["gemini-cli"].len(), 1);
    }

    #[test]
    fn claude_without_request_ids_deduplicates_messages_only_within_a_session() {
        let f = Fixture::new();
        let first = claude("shared-message", 20);
        let revision = claude("shared-message", 30);
        let mut another_session = revision.clone();
        another_session["sessionId"] = json!("another-session");
        f.write("one.jsonl", &[first, revision]);
        f.write("two.jsonl", &[another_session]);

        let snapshot = f.scanner(Provider::ClaudeCode).scan();
        assert_eq!(snapshot.rows[0].input_tokens, Some(700));
        assert_eq!(snapshot.rows[0].output_tokens, Some(60));
        assert_eq!(snapshot.rows[0].requests, 2);
        assert_eq!(snapshot.rows[0].sessions, 2);
    }

    #[test]
    fn copied_claude_response_uses_its_original_session_and_day_before_period_filtering() {
        let f = Fixture::new();
        let mut original = at_ms(claude("shared-message", 1), local_ms(2026, 1, 1, 12, 0));
        original["requestId"] = json!("shared-request");
        let mut copied = original.clone();
        copied["sessionId"] = json!("resumed-session");
        copied["timestamp"] = json!(local_ms(2026, 1, 2, 12, 0));
        copied["message"]["usage"]["output_tokens"] = json!(30);
        let original_session_only = at_ms(claude("other-message", 10), local_ms(2026, 1, 1, 13, 0));
        f.write("original.jsonl", &[original, original_session_only]);
        f.write("resumed.jsonl", &[copied]);

        let mut scanner = f.scanner(Provider::ClaudeCode);
        let all = scanner.scan();
        assert_eq!(all.rows[0].input_tokens, Some(700));
        assert_eq!(all.rows[0].output_tokens, Some(40));
        assert_eq!(all.rows[0].requests, 2);
        assert_eq!(all.rows[0].sessions, 1);
        assert_eq!(all.daily.len(), 1);
        assert_eq!(all.daily[0].date, "2026-01-01");
        assert_eq!(all.daily[0].output_tokens, Some(40));
        let today = scanner.scan_at(UsagePeriod::Today, local_ms(2026, 1, 2, 13, 0));
        assert!(today.rows.is_empty());
        assert!(today.daily.is_empty());
    }

    #[test]
    fn claude_distinct_request_ids_with_same_message_id_and_invalid_revisions() {
        let f = Fixture::new();
        let mut req_a = claude("message-a", 30);
        req_a["requestId"] = json!("request-a");
        let mut req_a_invalid = req_a.clone();
        req_a_invalid["timestamp"] = json!("2026-01-02T00:00:00Z");
        req_a_invalid["message"]["usage"]["reasoning_output_tokens"] = json!(31);
        let mut req_b = claude("message-a", 15);
        req_b["requestId"] = json!("request-b");
        f.write("one.jsonl", &[req_a, req_a_invalid, req_b]);

        let snapshot = f.scanner(Provider::ClaudeCode).scan();
        assert_eq!(snapshot.rows[0].output_tokens, Some(45));
        assert_eq!(snapshot.rows[0].requests, 2);
    }

    #[test]
    fn zstd_skippable_frame_resumes_decoding() {
        let f = Fixture::new();
        let mut bytes = Vec::new();
        // Valid Zstandard skippable frame (magic 0x184D2A50 + 4-byte payload).
        bytes.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18]);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(b"meta");
        let payload = format!(
            "{}\n{}\n{}\n",
            meta(),
            context("gpt-5.4", "medium"),
            total("2026-10-30T10:00:00Z", 1000, 100)
        );
        let data_frame = zstd::stream::encode_all(payload.as_bytes(), 0).unwrap();
        bytes.extend_from_slice(&data_frame);
        let path = f.0.join("skippable.jsonl.zst");
        fs::write(&path, bytes).unwrap();

        let snap = f.scanner(Provider::Codex).scan();
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].input_tokens, Some(1000));
        assert!(!snap.warnings.iter().any(|w| w.contains("compressed frame")));
    }

    #[test]
    fn opencode_zero_v2_falls_back_to_usable_v1_and_qwen4_max_cache_write_prices() {
        let f = Fixture::new();
        let db_path = f.0.join("opencode.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        let zero_v2 = json!({
            "time": {"created": 1_793_044_661_000i64},
            "model": {"id": "qwen4-max"},
            "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        });
        let usable_v1 = json!({
            "role": "assistant",
            "time": {"created": 1_793_044_661_000i64},
            "modelID": "qwen4-max",
            "tokens": {"input": 100, "output": 50, "reasoning": 0, "cache": {"read": 200, "write": 100}}
        });
        conn.execute(
            "INSERT INTO session_message VALUES ('m-1', 's-1', 'assistant', 1793044661000, ?1)",
            rusqlite::params![zero_v2.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message VALUES ('m-1', 's-1', 1793044661000, ?1)",
            rusqlite::params![usable_v1.to_string()],
        )
        .unwrap();
        drop(conn);

        let snap = f
            .scanner_store(Provider::OpenCode, SessionStore::Sqlite)
            .scan();
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].input_tokens, Some(400));
        assert_eq!(snap.rows[0].cache_write_tokens, Some(100));
        assert!(snap.rows[0].estimated_cost_usd.is_some());
    }

    #[test]
    fn crush_mixed_models_and_zed_undated_and_aider_cache_hit_and_overlong_lines() {
        // Crush multi-model session leaves model unknown rather than attributing all tokens to the last model.
        let f_crush = Fixture::new();
        let crush_db = f_crush.0.join("crush.db");
        let cconn = rusqlite::Connection::open(&crush_db).unwrap();
        cconn
            .execute_batch(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, created_at INTEGER, updated_at INTEGER, prompt_tokens INTEGER, completion_tokens INTEGER);
                 CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, model TEXT, created_at INTEGER);
                 INSERT INTO sessions VALUES ('c1', 1789189700, 1789189720, 420, 80);
                 INSERT INTO messages VALUES ('m1', 'c1', 'assistant', 'gpt-5.4', 1789189710);
                 INSERT INTO messages VALUES ('m2', 'c1', 'assistant', 'claude-sonnet-4-5', 1789189715);",
            )
            .unwrap();
        drop(cconn);
        let crush_snap = f_crush
            .scanner_store(Provider::Crush, SessionStore::Sqlite)
            .scan();
        assert_eq!(crush_snap.rows.len(), 1);
        assert_eq!(crush_snap.rows[0].model, "");

        // Zed request_token_usage without per-request timestamps stays in All totals without distorting daily charts.
        let f_zed = Fixture::new();
        let zed_db = f_zed.0.join("threads.db");
        let zconn = rusqlite::Connection::open(&zed_db).unwrap();
        zconn
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, updated_at TEXT, created_at TEXT, data_type TEXT, data BLOB);",
            )
            .unwrap();
        let zed_blob = json!({
            "model": {"model": "claude-sonnet-4-5"},
            "request_token_usage": {
                "msg-1": {"input_tokens": 50, "output_tokens": 25, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
            }
        });
        zconn
            .execute(
                "INSERT INTO threads VALUES ('th-1', '2026-10-21T07:12:33Z', '2026-10-20T07:12:00Z', 'json', ?1)",
                rusqlite::params![zed_blob.to_string()],
            )
            .unwrap();
        drop(zconn);
        let zed_snap = f_zed
            .scanner_store(Provider::Zed, SessionStore::Sqlite)
            .scan();
        assert_eq!(zed_snap.rows.len(), 1);
        assert!(zed_snap.daily.is_empty());

        // Aider: cache hit before received, overlong malformed line ignored, and undated (daily empty).
        let f_aider = Fixture::new();
        let malformed = format!("> Tokens: {}\n", "foo,".repeat(200));
        f_aider.write_text(
            "repo/.aider.chat.history.md",
            &format!(
                "# aider chat started at 2026-10-21 08:13:45\n\
                 > Main model: gpt-5.4\n\
                 > Tokens: 100 sent, 20 cache hit, 5 received.\n\
                 {malformed}"
            ),
        );
        let aider_snap = f_aider.scanner(Provider::Aider).scan();
        assert_eq!(aider_snap.rows.len(), 1);
        assert_eq!(aider_snap.rows[0].input_tokens, Some(100));
        assert_eq!(aider_snap.rows[0].cached_input_tokens, Some(20));
        assert_eq!(aider_snap.rows[0].output_tokens, Some(5));
        assert!(aider_snap.daily.is_empty());
    }

    fn assert_snapshot_eq(actual: &UsageSnapshot, expected: &UsageSnapshot) {
        assert_eq!(actual.pricing_checked_at, expected.pricing_checked_at);
        assert_eq!(actual.scanned_files, expected.scanned_files);
        assert_eq!(actual.warnings, expected.warnings);
        assert_eq!(actual.refreshed_at_ms, expected.refreshed_at_ms);
        assert_eq!(actual.rows.len(), expected.rows.len());
        for (a, e) in actual.rows.iter().zip(&expected.rows) {
            assert_eq!(a.provider, e.provider);
            assert_eq!(a.model, e.model);
            assert_eq!(a.effort, e.effort);
            assert_eq!(a.input_tokens, e.input_tokens);
            assert_eq!(a.cached_input_tokens, e.cached_input_tokens);
            assert_eq!(a.cache_write_tokens, e.cache_write_tokens);
            assert_eq!(a.output_tokens, e.output_tokens);
            assert_eq!(a.reasoning_tokens, e.reasoning_tokens);
            assert_eq!(a.task_time_ms, e.task_time_ms);
            assert_eq!(a.estimated_cost_usd, e.estimated_cost_usd);
            assert_eq!(a.sessions, e.sessions);
            assert_eq!(a.requests, e.requests);
            assert_eq!(a.last_seen_ms, e.last_seen_ms);
        }
        let assert_days = |a_days: &[UsageDay], e_days: &[UsageDay]| {
            assert_eq!(a_days.len(), e_days.len());
            for (a, e) in a_days.iter().zip(e_days) {
                assert_eq!(a.date, e.date);
                assert_eq!(a.input_tokens, e.input_tokens);
                assert_eq!(a.output_tokens, e.output_tokens);
                assert_eq!(a.cached_input_tokens, e.cached_input_tokens);
                assert_eq!(a.estimated_cost_usd, e.estimated_cost_usd);
                assert_eq!(a.requests, e.requests);
            }
        };
        assert_days(&actual.daily, &expected.daily);
        assert_eq!(
            actual.daily_by_provider.keys().collect::<Vec<_>>(),
            expected.daily_by_provider.keys().collect::<Vec<_>>()
        );
        for (provider, a_days) in &actual.daily_by_provider {
            assert_days(a_days, &expected.daily_by_provider[provider]);
        }
    }

    #[test]
    fn scan_all_periods_matches_individual_period_scans() {
        let f = Fixture::new();
        let now_dt = Local
            .with_ymd_and_hms(2026, 2, 15, 14, 0, 0)
            .single()
            .unwrap();
        let now = now_dt.timestamp_millis();
        let day_ms = 86_400_000_i64;
        let ts = |days_ago: i64, hour: u32| {
            let dt = Local
                .with_ymd_and_hms(2026, 2, 15, hour, 0, 0)
                .single()
                .unwrap()
                .timestamp_millis()
                - days_ago * day_ms;
            chrono::DateTime::from_timestamp_millis(dt)
                .unwrap()
                .to_rfc3339()
        };
        f.write(
            "multi.jsonl",
            &[
                meta(),
                context("gpt-5.2", "high"),
                total(&ts(45, 10), 1_000, 100),
                total(&ts(15, 11), 2_500, 250),
                total(&ts(3, 12), 4_000, 400),
                total(&ts(0, 9), 5_500, 550),
            ],
        );

        let mut single = f.scanner(Provider::Codex);
        let expected_today = single.scan_at(UsagePeriod::Today, now);
        let expected_week = single.scan_at(UsagePeriod::Week, now);
        let expected_month = single.scan_at(UsagePeriod::Month, now);
        let expected_all = single.scan_at(UsagePeriod::All, now);

        let mut multi = f.scanner(Provider::Codex);
        let bundle = multi.scan_all_periods_at(now, None).unwrap();

        assert_snapshot_eq(bundle.for_period(UsagePeriod::Today), &expected_today);
        assert_snapshot_eq(bundle.for_period(UsagePeriod::Week), &expected_week);
        assert_snapshot_eq(bundle.for_period(UsagePeriod::Month), &expected_month);
        assert_snapshot_eq(bundle.for_period(UsagePeriod::All), &expected_all);
        assert_eq!(bundle.refreshed_at_ms(), now);
    }

    #[test]
    fn scan_all_periods_cancellable_stops_when_cancelled() {
        let f = Fixture::new();
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("gpt-5.2", "medium"),
                total("2026-01-01T00:00:01Z", 100, 20),
            ],
        );
        let mut scanner = f.scanner(Provider::Codex);
        let cancel = AtomicBool::new(true);
        assert!(scanner.scan_all_periods_cancellable(&cancel).is_none());
        cancel.store(false, Ordering::Relaxed);
        let bundle = scanner.scan_all_periods_cancellable(&cancel).unwrap();
        assert_eq!(bundle.all.rows.len(), 1);
    }

    #[test]
    fn usage_scope_key_is_deterministic_opaque_and_sensitive_to_roots_and_rates() {
        let sources_a = vec![SessionSource {
            provider: Provider::Codex,
            root: PathBuf::from(r"C:\Users\Alice\.codex\sessions"),
            store: SessionStore::LogFiles,
        }];
        let sources_a_alt = vec![SessionSource {
            provider: Provider::Codex,
            root: PathBuf::from("c:/users/alice/.codex/sessions"),
            store: SessionStore::LogFiles,
        }];
        let sources_b = vec![SessionSource {
            provider: Provider::Codex,
            root: PathBuf::from(r"C:\Users\Bob\.codex\sessions"),
            store: SessionStore::LogFiles,
        }];
        let rates_a = PathBuf::from(r"C:\Users\Alice\AppData\Roaming\Discodex\usage-rates.toml");
        let rates_b = PathBuf::from(r"C:\Users\Bob\AppData\Roaming\Discodex\usage-rates.toml");

        let k1 = usage_scope_key(&sources_a, &rates_a);
        let k1_alt = usage_scope_key(&sources_a_alt, &rates_a);
        let k2 = usage_scope_key(&sources_b, &rates_a);
        let k3 = usage_scope_key(&sources_a, &rates_b);

        assert_eq!(k1.len(), 32);
        assert!(k1.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(k1, k1_alt);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert!(!k1.to_ascii_lowercase().contains("alice"));
    }

    #[test]
    fn usage_scope_key_resolves_relative_roots_against_base_dir() {
        let rel_sources = vec![SessionSource {
            provider: Provider::Codex,
            root: PathBuf::from(r".\codex\sessions"),
            store: SessionStore::LogFiles,
        }];
        let abs_sources_alpha = vec![SessionSource {
            provider: Provider::Codex,
            root: PathBuf::from(r"C:\Work\Alpha\codex\sessions"),
            store: SessionStore::LogFiles,
        }];
        let base_alpha = Path::new(r"C:\Work\Alpha");
        let base_beta = Path::new(r"C:\Work\Beta");
        let rel_rates = Path::new(r".\cfg\..\usage-rates.toml");
        let abs_rates_alpha = Path::new(r"C:\Work\Alpha\usage-rates.toml");

        let k_rel_alpha = usage_scope_key_with_base(&rel_sources, rel_rates, Some(base_alpha));
        let k_abs_alpha =
            usage_scope_key_with_base(&abs_sources_alpha, abs_rates_alpha, Some(base_alpha));
        let k_rel_beta = usage_scope_key_with_base(&rel_sources, rel_rates, Some(base_beta));

        assert_eq!(k_rel_alpha, k_abs_alpha);
        assert_ne!(k_rel_alpha, k_rel_beta);
    }

    #[test]
    fn extra_usage_watch_roots_includes_codex_archive_and_excludes_aider() {
        let sources = vec![
            SessionSource {
                provider: Provider::Codex,
                root: PathBuf::from(r"C:\Work\CustomCodex\sessions"),
                store: SessionStore::LogFiles,
            },
            SessionSource {
                provider: Provider::Aider,
                root: PathBuf::from(r"C:\Work\Repo"),
                store: SessionStore::LogFiles,
            },
        ];
        let extras = extra_usage_watch_roots_with_base(&sources, None);
        assert_eq!(
            extras,
            vec![PathBuf::from(r"C:\Work\CustomCodex\archived_sessions")]
        );
    }

    #[test]
    fn usage_bundle_round_trips_through_json() {
        let f = Fixture::new();
        f.write(
            "one.jsonl",
            &[
                meta(),
                context("gpt-5.2", "low"),
                total("2026-01-01T00:00:01Z", 100, 20),
            ],
        );
        let mut scanner = f.scanner(Provider::Codex);
        let bundle = scanner.scan_all_periods();
        let encoded = serde_json::to_string(&bundle).unwrap();
        let decoded: UsageBundle = serde_json::from_str(&encoded).unwrap();
        assert_snapshot_eq(&decoded.today, &bundle.today);
        assert_snapshot_eq(&decoded.week, &bundle.week);
        assert_snapshot_eq(&decoded.month, &bundle.month);
        assert_snapshot_eq(&decoded.all, &bundle.all);
    }

    #[test]
    #[ignore = "depends on local session logs on this machine"]
    fn live_usage_scan_detects_local_providers() {
        let temp_rates =
            std::env::temp_dir().join(format!("discodex-live-rates-{}.toml", std::process::id()));
        let mut scanner = UsageScanner::new(crate::provider::session_sources(), temp_rates);
        let snap = scanner.scan();
        let mut by_provider: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        for row in &snap.rows {
            let entry = by_provider.entry(row.provider.clone()).or_default();
            entry.0 += 1;
            entry.1 = entry.1.saturating_add(
                row.input_tokens
                    .unwrap_or_default()
                    .saturating_add(row.output_tokens.unwrap_or_default()),
            );
        }
        println!(
            "scanned_files={} rows={}",
            snap.scanned_files,
            snap.rows.len()
        );
        for (provider, (rows, tokens)) in &by_provider {
            println!("provider={provider} rows={rows} total_tokens={tokens}");
        }
        println!("warnings={}", snap.warnings.len());
        for warning in &snap.warnings {
            println!("  warning: {warning}");
        }
        assert!(by_provider.len() >= 2);
    }
}
