use std::time::Duration;

use crate::{
    discord::PresenceSnapshot,
    foreground,
    provider::{Provider, SessionSource, SessionStore},
    state::{ActivityState, Phase},
};

pub(crate) const SESSION_ACTIVE_TIMEOUT_MS: i64 = 180_000;
const DISCORD_RETRY_MS: i64 = 5_000;
const PRESENCE_HEALTHCHECK_MS: i64 = 15_000;
const TEST_PRESENCE_MS: i64 = 15_000;
const FOREGROUND_POLL_MS: i64 = 1_000;
const FOREGROUND_DWELL_MS: i64 = 2_000;
const FOREGROUND_HOLD_MS: i64 = 15_000;
const STORE_POLL_MS: i64 = 2_000;
const AIDER_SCAN_MS: i64 = 15_000;

#[derive(Debug, Default)]
struct ForegroundState {
    detected_provider: Option<Provider>,
    raw_provider: Option<Provider>,
    raw_since_ms: i64,
    established_provider: Option<Provider>,
    started_at_ms: i64,
    hold_deadline_ms: Option<i64>,
}

impl ForegroundState {
    fn refresh(&mut self, detected_provider: Option<Provider>, now_ms: i64) {
        self.detected_provider = detected_provider;
        let current_raw = detected_provider.filter(|provider| !provider.has_session_tracking());
        if current_raw != self.raw_provider {
            self.raw_provider = current_raw;
            self.raw_since_ms = now_ms;
        }

        match self.raw_provider {
            Some(provider) => {
                if Some(provider) == self.established_provider {
                    self.hold_deadline_ms = None;
                } else if now_ms.saturating_sub(self.raw_since_ms) >= FOREGROUND_DWELL_MS {
                    self.established_provider = Some(provider);
                    self.started_at_ms = self.raw_since_ms;
                    self.hold_deadline_ms = None;
                }
            }
            None => {
                if self.established_provider.is_some() {
                    if let Some(deadline) = self.hold_deadline_ms {
                        if now_ms >= deadline {
                            self.established_provider = None;
                            self.hold_deadline_ms = None;
                        }
                    } else {
                        self.hold_deadline_ms = Some(now_ms.saturating_add(FOREGROUND_HOLD_MS));
                    }
                } else {
                    self.hold_deadline_ms = None;
                }
            }
        }
    }

    fn deadline_ms(&self) -> Option<i64> {
        let dwell = (self.raw_provider.is_some() && self.raw_provider != self.established_provider)
            .then(|| self.raw_since_ms.saturating_add(FOREGROUND_DWELL_MS));
        dwell.into_iter().chain(self.hold_deadline_ms).min()
    }

    fn presence(&self) -> Option<PresenceSnapshot> {
        let provider = self.established_provider?;
        Some(PresenceSnapshot {
            provider,
            project: provider.display_name().to_string(),
            project_stack: None,
            current_file: None,
            current_language: None,
            activity: provider.default_activity().to_string(),
            phase: Phase::Thinking,
            started_at_ms: self.started_at_ms,
            agent: None,
            reasoning: None,
        })
    }
}

pub(crate) struct DueWork {
    pub stores: bool,
    pub aider: bool,
}

pub(crate) struct Schedule {
    foreground: ForegroundState,
    foreground_enabled: bool,
    stores_enabled: bool,
    aider_enabled: bool,
    next_foreground_ms: i64,
    next_store_ms: i64,
    next_aider_ms: i64,
    next_retry_ms: i64,
    next_healthcheck_ms: i64,
    test_presence: Option<(i64, i64)>,
}

impl Schedule {
    pub fn new(sources: &[SessionSource]) -> Self {
        Self {
            foreground: ForegroundState::default(),
            foreground_enabled: false,
            stores_enabled: sources
                .iter()
                .any(|source| source.store != SessionStore::LogFiles),
            aider_enabled: sources
                .iter()
                .any(|source| source.provider == Provider::Aider),
            next_foreground_ms: 0,
            next_store_ms: 0,
            next_aider_ms: 0,
            next_retry_ms: 0,
            next_healthcheck_ms: 0,
            test_presence: None,
        }
    }

    pub fn due(&mut self, now_ms: i64, foreground_enabled: bool) -> DueWork {
        self.advance(now_ms, foreground_enabled, foreground::active_provider)
    }

    fn advance(
        &mut self,
        now_ms: i64,
        foreground_enabled: bool,
        detect: impl FnOnce() -> Option<Provider>,
    ) -> DueWork {
        self.foreground_enabled = foreground_enabled;
        if foreground_enabled
            && (now_ms >= self.next_foreground_ms
                || self
                    .foreground
                    .deadline_ms()
                    .is_some_and(|deadline| now_ms >= deadline))
        {
            self.foreground.refresh(detect(), now_ms);
            self.next_foreground_ms = now_ms.saturating_add(FOREGROUND_POLL_MS);
        } else if !foreground_enabled {
            self.foreground = ForegroundState::default();
            self.next_foreground_ms = now_ms.saturating_add(FOREGROUND_POLL_MS);
        }

        if self
            .test_presence
            .is_some_and(|(deadline, _)| now_ms >= deadline)
        {
            self.test_presence = None;
        }
        let stores = self.stores_enabled && now_ms >= self.next_store_ms;
        if stores {
            self.next_store_ms = now_ms.saturating_add(STORE_POLL_MS);
        }
        let aider = self.aider_enabled && now_ms >= self.next_aider_ms;
        if aider {
            self.next_aider_ms = now_ms.saturating_add(AIDER_SCAN_MS);
        }
        DueWork { stores, aider }
    }

    pub fn foreground_presence(&self) -> Option<PresenceSnapshot> {
        self.foreground.presence()
    }

    pub fn detected_provider(&self) -> Option<Provider> {
        self.foreground.detected_provider
    }

    pub fn start_test(&mut self, now_ms: i64) {
        self.test_presence = Some((now_ms.saturating_add(TEST_PRESENCE_MS), now_ms));
    }

    pub fn test_started_at_ms(&self) -> Option<i64> {
        self.test_presence.map(|(_, started)| started)
    }

    pub fn can_publish(&self, now_ms: i64, has_presence: bool, connected: bool) -> bool {
        !has_presence || connected || now_ms >= self.next_retry_ms
    }

    pub fn force_refresh(&self, now_ms: i64, has_presence: bool, connected: bool) -> bool {
        has_presence && connected && now_ms >= self.next_healthcheck_ms
    }

    pub fn publish_failed(&mut self, now_ms: i64) {
        self.next_retry_ms = now_ms.saturating_add(DISCORD_RETRY_MS);
        self.next_healthcheck_ms = 0;
    }

    pub fn published(&mut self, now_ms: i64, connected: bool, force_refresh: bool) {
        if connected {
            self.next_retry_ms = 0;
            if self.next_healthcheck_ms == 0 || force_refresh {
                self.next_healthcheck_ms = now_ms.saturating_add(PRESENCE_HEALTHCHECK_MS);
            }
        } else {
            self.next_healthcheck_ms = 0;
        }
    }

    pub fn timeout(
        &self,
        now_ms: i64,
        activity: &ActivityState,
        has_presence: bool,
        connected: bool,
    ) -> Option<Duration> {
        let foreground_deadline = if self.foreground_enabled {
            Some(
                self.foreground
                    .deadline_ms()
                    .map_or(self.next_foreground_ms, |deadline| {
                        deadline.min(self.next_foreground_ms)
                    }),
            )
        } else {
            None
        };
        next_timeout(
            now_ms,
            &[
                activity.next_terminal_deadline_ms(),
                activity.next_stale_deadline_ms(SESSION_ACTIVE_TIMEOUT_MS),
                self.test_presence.map(|(deadline, _)| deadline),
                (has_presence && !connected).then_some(self.next_retry_ms),
                (has_presence && connected).then_some(self.next_healthcheck_ms),
                foreground_deadline,
                self.stores_enabled.then_some(self.next_store_ms),
                self.aider_enabled.then_some(self.next_aider_ms),
            ],
        )
    }
}

fn next_timeout(now_ms: i64, deadlines: &[Option<i64>]) -> Option<Duration> {
    deadlines
        .iter()
        .flatten()
        .min()
        .map(|deadline| Duration::from_millis(deadline.saturating_sub(now_ms).max(1) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_uses_nearest_deadline() {
        assert_eq!(
            next_timeout(1_000, &[Some(5_000), None, Some(2_500)]),
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(
            next_timeout(1_000, &[Some(500)]),
            Some(Duration::from_millis(1))
        );
        assert_eq!(next_timeout(1_000, &[None]), None);
    }

    #[test]
    fn session_only_providers_are_detected_without_fallback_presence() {
        for provider in Provider::ALL
            .into_iter()
            .filter(|provider| provider.has_session_tracking())
        {
            let mut schedule = Schedule::new(&[]);
            schedule.advance(1_000, true, || Some(provider));
            schedule.advance(3_000, true, || Some(provider));
            assert_eq!(schedule.detected_provider(), Some(provider));
            assert!(schedule.foreground_presence().is_none());
        }
    }

    #[test]
    fn fallback_requires_dwell_and_preserves_elapsed_time_across_brief_focus_loss() {
        let mut state = ForegroundState::default();
        state.refresh(Some(Provider::Cursor), 1_000);
        state.refresh(None, 1_500);
        assert!(state.presence().is_none());
        state.refresh(Some(Provider::Cursor), 2_000);
        assert!(state.presence().is_none());
        state.refresh(Some(Provider::Cursor), 4_000);
        assert_eq!(state.presence().unwrap().started_at_ms, 2_000);
        state.refresh(None, 5_000);
        assert_eq!(state.presence().unwrap().provider, Provider::Cursor);
        state.refresh(Some(Provider::Cursor), 8_000);
        assert_eq!(state.presence().unwrap().started_at_ms, 2_000);
        state.refresh(None, 10_000);
        state.refresh(None, 25_000);
        assert!(state.presence().is_none());
    }

    #[test]
    fn disabled_foreground_clears_detection_and_has_no_poll_deadline() {
        let mut schedule = Schedule::new(&[]);
        schedule.advance(1_000, true, || Some(Provider::Cursor));
        schedule.advance(3_000, true, || Some(Provider::Cursor));
        assert!(schedule.foreground_presence().is_some());
        schedule.advance(4_000, false, || {
            panic!("disabled detection must not read the focused window")
        });
        assert_eq!(schedule.detected_provider(), None);
        assert!(schedule.foreground_presence().is_none());
        assert_eq!(
            schedule.timeout(4_000, &ActivityState::default(), false, false),
            None
        );
    }

    #[test]
    fn failed_publish_retries_after_five_seconds_but_clear_is_immediate() {
        let mut schedule = Schedule::new(&[]);
        schedule.publish_failed(1_000);
        assert!(!schedule.can_publish(5_999, true, false));
        assert!(schedule.can_publish(6_000, true, false));
        assert!(schedule.can_publish(1_001, false, false));
        assert_eq!(
            schedule.timeout(1_000, &ActivityState::default(), true, false),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn connected_presence_is_refreshed_every_fifteen_seconds() {
        let mut schedule = Schedule::new(&[]);
        schedule.published(1_000, true, false);
        assert!(!schedule.force_refresh(15_999, true, true));
        assert!(schedule.force_refresh(16_000, true, true));
        assert_eq!(
            schedule.timeout(1_000, &ActivityState::default(), true, true),
            Some(Duration::from_secs(15))
        );
        schedule.published(16_000, true, true);
        assert!(!schedule.force_refresh(30_999, true, true));
        assert!(schedule.force_refresh(31_000, true, true));
        assert!(!schedule.force_refresh(31_000, false, true));
    }

    #[test]
    fn aider_only_sources_wake_at_their_scan_deadline() {
        let mut schedule = Schedule::new(&[SessionSource {
            provider: Provider::Aider,
            root: std::path::PathBuf::from("workspace"),
            store: SessionStore::LogFiles,
        }]);
        let due = schedule.advance(1_000, false, || None);
        assert!(due.aider);
        assert!(!due.stores);
        assert_eq!(
            schedule.timeout(1_000, &ActivityState::default(), false, false),
            Some(Duration::from_secs(15))
        );
        assert!(!schedule.advance(15_999, false, || None).aider);
        assert!(schedule.advance(16_000, false, || None).aider);
    }

    #[test]
    fn store_polling_and_test_presence_have_independent_deadlines() {
        let mut schedule = Schedule::new(&[SessionSource {
            provider: Provider::Crush,
            root: std::path::PathBuf::from("crush"),
            store: SessionStore::Sqlite,
        }]);
        assert!(schedule.advance(1_000, false, || None).stores);
        assert!(!schedule.advance(2_999, false, || None).stores);
        assert!(schedule.advance(3_000, false, || None).stores);
        schedule.start_test(3_000);
        assert_eq!(schedule.test_started_at_ms(), Some(3_000));
        schedule.advance(17_999, false, || None);
        assert_eq!(schedule.test_started_at_ms(), Some(3_000));
        schedule.advance(18_000, false, || None);
        assert_eq!(schedule.test_started_at_ms(), None);
    }
}
