//! 协调本进程的粘贴写回与注入；状态切换由同一短锁保护，后台生产者可同步取消旧任务。

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

#[derive(Debug, Default)]
struct State {
    busy: bool,
    generation: u64,
    started_ticks: i64,
}

#[derive(Debug, Default)]
pub(super) struct PasteCoordinator {
    state: Mutex<State>,
}

pub(super) struct PasteLease {
    state: Arc<PasteCoordinator>,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct PasteToken {
    state: Arc<PasteCoordinator>,
    generation: u64,
}

/// Both pre-GPUI bridges and the GPUI Global use the same process-local coordinator.
pub(super) fn shared_coordinator() -> Arc<PasteCoordinator> {
    static SHARED: OnceLock<Arc<PasteCoordinator>> = OnceLock::new();
    SHARED
        .get_or_init(|| Arc::new(PasteCoordinator::default()))
        .clone()
}

/// Runtime controls cancel at observation time, before their native operation enters the UI queue.
pub(super) fn observe_control(ticks: i64) {
    shared_coordinator().cancel_before(ticks);
}

impl PasteCoordinator {
    /// Recover poisoned state without panicking; callers never retain this guard across OS or GPUI work.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Acquire and publish all lease metadata in one transition before clipboard preparation starts.
    pub(super) fn try_begin(self: &Arc<Self>, ticks: i64) -> Option<PasteLease> {
        let mut state = self.lock();
        if state.busy {
            return None;
        }
        state.busy = true;
        state.generation = state.generation.wrapping_add(1);
        state.started_ticks = ticks;
        Some(PasteLease {
            state: self.clone(),
            generation: state.generation,
        })
    }

    #[cfg(test)]
    pub(super) fn cancel(&self) {
        let mut state = self.lock();
        state.generation = state.generation.wrapping_add(1);
    }

    /// A control observed before a newer lease cannot cancel or modify that lease.
    pub(super) fn started_after(&self, ticks: i64) -> bool {
        let state = self.lock();
        state.busy && state.started_ticks > ticks
    }

    pub(super) fn cancel_before(&self, ticks: i64) {
        let mut state = self.lock();
        if !state.busy || state.started_ticks <= ticks {
            state.generation = state.generation.wrapping_add(1);
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        let state = self.lock();
        state.busy && state.generation == generation
    }
}

impl PasteLease {
    pub(super) fn is_current(&self) -> bool {
        self.state.is_current(self.generation)
    }

    pub(super) fn token(&self) -> PasteToken {
        PasteToken {
            state: self.state.clone(),
            generation: self.generation,
        }
    }
}

impl PasteToken {
    pub(super) fn is_current(&self) -> bool {
        self.state.is_current(self.generation)
    }
}

impl Drop for PasteLease {
    fn drop(&mut self) {
        self.state.lock().busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_old_producer_cancellation_cannot_observe_half_published_lease_metadata() {
        let coordinator = Arc::new(PasteCoordinator::default());
        let producer_state = coordinator.clone();
        let producer = std::thread::spawn(move || {
            for _ in 0..10_000 {
                producer_state.cancel_before(150);
            }
        });
        for _ in 0..1_000 {
            let lease = coordinator.try_begin(200).unwrap();
            assert!(lease.is_current());
            assert!(lease.token().is_current());
            drop(lease);
        }
        producer.join().unwrap();
    }

    #[test]
    fn poisoned_coordinator_recovers_without_panicking_or_releasing_an_active_lease() {
        let coordinator = Arc::new(PasteCoordinator::default());
        let lease = coordinator.try_begin(100).unwrap();
        let poisoned = coordinator.clone();
        assert!(
            std::thread::spawn(move || {
                let _state = poisoned.state.lock().unwrap();
                panic!("intentional test poison");
            })
            .join()
            .is_err()
        );
        assert!(lease.is_current());
        coordinator.cancel_before(150);
        assert!(!lease.is_current());
        assert!(coordinator.try_begin(200).is_none());
        drop(lease);
        assert!(coordinator.try_begin(200).is_some());
    }

    #[test]
    fn queued_old_recapture_and_copy_hide_do_not_cancel_a_new_paste() {
        let state = Arc::new(PasteCoordinator::default());
        let first = state.try_begin(100).unwrap();
        state.cancel_before(150);
        assert!(!first.is_current());
        drop(first);
        let next = state.try_begin(200).unwrap();
        for old_control in [150, 175] {
            assert!(state.started_after(old_control));
            state.cancel_before(old_control);
            assert!(next.is_current());
            assert!(next.token().is_current());
        }
        assert!(!state.started_after(250));
        state.cancel_before(250);
        assert!(!next.is_current());
    }

    #[test]
    fn overlapping_paste_cannot_replace_clipboard_while_first_is_waiting() {
        let state = Arc::new(PasteCoordinator::default());
        let first = state.try_begin(100).unwrap();
        assert!(state.try_begin(100).is_none());
        assert!(first.is_current());
        drop(first);
        assert!(state.try_begin(100).is_some());
    }

    #[test]
    fn copy_cancels_old_task_without_releasing_its_lease() {
        let state = Arc::new(PasteCoordinator::default());
        let first = state.try_begin(100).unwrap();
        state.cancel();
        assert!(!first.is_current());
        assert!(state.try_begin(100).is_none());
        drop(first);
        let next = state.try_begin(100).unwrap();
        assert!(next.is_current());
    }

    #[test]
    fn cloned_tokens_cannot_release_or_outlive_the_owning_lease() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(100).unwrap();
        let token = lease.token();
        drop(token.clone());
        assert!(state.try_begin(100).is_none());
        assert!(token.is_current());
        drop(lease);
        assert!(!token.is_current());
        assert!(state.try_begin(100).is_some());
    }

    #[test]
    fn queued_injection_token_cannot_authorize_a_replacement_paste() {
        let state = Arc::new(PasteCoordinator::default());
        let lease = state.try_begin(100).unwrap();
        let token = lease.token();
        state.cancel();
        assert!(!token.is_current());
        drop(lease);
        let next = state.try_begin(100).unwrap();
        assert!(next.token().is_current());
        assert!(!token.is_current());
    }
}
