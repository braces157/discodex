use std::{
    io,
    path::PathBuf,
    process::Command as ProcessCommand,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
};

use crate::{
    provider::SessionSource,
    usage::UsageScanner,
    usage_cache::{self, CACHE_SCHEMA_VERSION, CachedUsage, PublishOutcome},
};

#[derive(Default)]
struct PendingWork {
    scan: bool,
    edit_rates: bool,
}

pub struct Worker {
    pending: Arc<(Mutex<PendingWork>, Condvar)>,
    pub updates: mpsc::Receiver<Update>,
}

pub enum Update {
    Bundle(Box<CachedUsage>),
    ScanError(String),
    Rates(Result<(), String>),
}

impl Worker {
    /// Spawns the background usage scanner and rate-editor worker thread.
    pub fn start(
        sources: Vec<SessionSource>,
        rates_path: PathBuf,
        cache_path: PathBuf,
        scope_key: String,
    ) -> io::Result<Self> {
        let pending = Arc::new((Mutex::new(PendingWork::default()), Condvar::new()));
        let work = pending.clone();
        let (send, updates) = mpsc::channel();
        thread::Builder::new()
            .name("usage-scan".into())
            .spawn(move || {
                let mut scanner = UsageScanner::new(sources, rates_path.clone());
                let mut generation =
                    usage_cache::load_cache(&cache_path, &scope_key).map_or(0, |c| c.generation);
                loop {
                    let (scan, edit) = {
                        let (lock, ready) = &*work;
                        let mut pending =
                            lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        while !pending.scan && !pending.edit_rates {
                            pending = ready
                                .wait(pending)
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                        (
                            std::mem::take(&mut pending.scan),
                            std::mem::take(&mut pending.edit_rates),
                        )
                    };
                    if edit {
                        let result = crate::usage::ensure_rates_file(&rates_path)
                            .and_then(|()| {
                                ProcessCommand::new("notepad.exe")
                                    .arg(&rates_path)
                                    .spawn()
                                    .map(|_| ())
                            })
                            .map_err(|error| format!("Cannot open custom rates. {error}"));
                        if send.send(Update::Rates(result)).is_err() {
                            break;
                        }
                    }
                    if scan {
                        let rates_fp = usage_cache::rates_fingerprint(&rates_path);
                        let update =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                scanner.scan_all_periods()
                            })) {
                                Ok(bundle) => {
                                    let fallback_date =
                                        usage_cache::local_date_for_ms(bundle.refreshed_at_ms())
                                            .unwrap_or_else(usage_cache::current_local_date);
                                    let cached = match usage_cache::publish_cache(
                                        &cache_path,
                                        &scope_key,
                                        rates_fp.clone(),
                                        bundle.clone(),
                                        generation,
                                    ) {
                                        Ok(PublishOutcome::Published(saved)) => {
                                            generation = saved.generation;
                                            saved
                                        }
                                        Ok(PublishOutcome::Superseded(existing)) => {
                                            generation = generation.max(existing.generation);
                                            existing
                                        }
                                        Err(_) => {
                                            generation = generation.saturating_add(1);
                                            CachedUsage {
                                                schema_version: CACHE_SCHEMA_VERSION,
                                                scope_key: scope_key.clone(),
                                                local_date: fallback_date,
                                                generation,
                                                rates_fingerprint: rates_fp,
                                                bundle,
                                            }
                                        }
                                    };
                                    Update::Bundle(Box::new(cached))
                                }
                                Err(_) => Update::ScanError(
                                    "The usage scan stopped unexpectedly. Refresh to retry.".into(),
                                ),
                            };
                        if send.send(update).is_err() {
                            break;
                        }
                    }
                }
            })?;
        Ok(Self { pending, updates })
    }

    pub fn scan(&self) {
        let (lock, ready) = &*self.pending;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scan = true;
        ready.notify_one();
    }

    pub fn edit_rates(&self) {
        let (lock, ready) = &*self.pending;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .edit_rates = true;
        ready.notify_one();
    }
}
