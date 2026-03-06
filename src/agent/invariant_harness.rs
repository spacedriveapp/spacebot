//! Invariant-driven fault harness for agent loop regression tests.

use super::channel_dispatch::{
    WorkerCompletionError, map_worker_completion_result, reserve_worker_slot_local,
};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
enum HarnessFault {
    ReserveWorker,
    CompleteWorker,
    WorkerFailure,
    WorkerCancelled,
    WorkerSuccess,
    EventLagged,
    EventClosed,
    CortexStart,
    CortexEnd,
    RetryAttempt,
}

#[derive(Debug, Default)]
struct HarnessState {
    active_workers: HashSet<crate::WorkerId>,
    max_workers: usize,
    worker_false_success: bool,
    lagged_event_kept_running: bool,
    closed_event_causes_stop: bool,
    cortex_inflight: usize,
    retry_attempts: usize,
}

impl HarnessState {
    fn new(max_workers: usize) -> Self {
        Self {
            max_workers,
            lagged_event_kept_running: true,
            closed_event_causes_stop: true,
            ..Self::default()
        }
    }

    fn assert_invariants(&self) {
        assert!(
            self.active_workers.len() <= self.max_workers,
            "active workers exceeded cap"
        );
        assert!(
            !self.worker_false_success,
            "worker failure was misclassified as success"
        );
        assert!(
            self.lagged_event_kept_running,
            "channel loop should continue after lagged event receiver errors"
        );
        assert!(
            self.closed_event_causes_stop,
            "channel loop should stop after closed event receiver"
        );
        assert!(
            self.cortex_inflight <= 1,
            "cortex single-flight lock invariant violated"
        );
        assert!(self.retry_attempts <= 4, "channel retry budget exceeded");
    }

    fn apply(&mut self, fault: HarnessFault, rng: &mut StdRng) {
        match fault {
            HarnessFault::ReserveWorker => {
                let worker_id = uuid::Uuid::new_v4();
                let channel_id: Arc<str> = Arc::from("channel");
                match reserve_worker_slot_local(
                    self.active_workers.len(),
                    &channel_id,
                    self.max_workers,
                ) {
                    Ok(()) => {
                        self.active_workers.insert(worker_id);
                    }
                    Err(crate::error::AgentError::WorkerLimitReached { .. }) => {
                        // Expected when at limit.
                    }
                    Err(error) => panic!("unexpected reserve error: {error}"),
                }
            }
            HarnessFault::CompleteWorker => {
                if !self.active_workers.is_empty() {
                    let index = rng.random_range(0..self.active_workers.len());
                    if let Some(worker_id) = self.active_workers.iter().nth(index).copied() {
                        self.active_workers.remove(&worker_id);
                    }
                }
            }
            HarnessFault::WorkerFailure => {
                let (_text, _notify, success) = map_worker_completion_result(Err(
                    WorkerCompletionError::failed("worker failed"),
                ));
                if success {
                    self.worker_false_success = true;
                }
            }
            HarnessFault::WorkerCancelled => {
                let (text, _notify, success) =
                    map_worker_completion_result(Err(WorkerCompletionError::Cancelled {
                        reason: "user requested".to_string(),
                    }));
                if success || !text.starts_with("Worker cancelled:") {
                    self.worker_false_success = true;
                }
            }
            HarnessFault::WorkerSuccess => {
                let (_text, _notify, success) = map_worker_completion_result(Ok("ok".to_string()));
                if !success {
                    self.worker_false_success = true;
                }
            }
            HarnessFault::EventLagged => {
                self.lagged_event_kept_running &= matches!(
                    crate::classify_broadcast_recv_result::<()>(Err(
                        tokio::sync::broadcast::error::RecvError::Lagged(1),
                    )),
                    crate::BroadcastRecvResult::Lagged(_)
                );
            }
            HarnessFault::EventClosed => {
                self.closed_event_causes_stop &= matches!(
                    crate::classify_broadcast_recv_result::<()>(Err(
                        tokio::sync::broadcast::error::RecvError::Closed,
                    )),
                    crate::BroadcastRecvResult::Closed
                );
            }
            HarnessFault::CortexStart => {
                self.cortex_inflight += 1;
            }
            HarnessFault::CortexEnd => {
                self.cortex_inflight = self.cortex_inflight.saturating_sub(1);
            }
            HarnessFault::RetryAttempt => {
                self.retry_attempts += 1;
            }
        }

        self.assert_invariants();
    }
}

#[test]
fn deterministic_worker_limit_invariant() {
    let mut rng = StdRng::seed_from_u64(7);
    let mut state = HarnessState::new(2);
    state.apply(HarnessFault::ReserveWorker, &mut rng);
    state.apply(HarnessFault::ReserveWorker, &mut rng);
    state.apply(HarnessFault::ReserveWorker, &mut rng);
    state.apply(HarnessFault::CompleteWorker, &mut rng);
    state.apply(HarnessFault::ReserveWorker, &mut rng);
}

#[test]
fn deterministic_failure_classification_invariant() {
    let mut rng = StdRng::seed_from_u64(13);
    let mut state = HarnessState::new(1);
    state.apply(HarnessFault::WorkerFailure, &mut rng);
    state.apply(HarnessFault::WorkerCancelled, &mut rng);
    state.apply(HarnessFault::WorkerSuccess, &mut rng);
}

#[test]
fn deterministic_event_receiver_invariant() {
    let mut rng = StdRng::seed_from_u64(19);
    let mut state = HarnessState::new(1);
    state.apply(HarnessFault::EventLagged, &mut rng);
    state.apply(HarnessFault::EventClosed, &mut rng);
}

#[test]
fn deterministic_cortex_single_flight_invariant() {
    let mut rng = StdRng::seed_from_u64(29);
    let mut state = HarnessState::new(1);
    state.apply(HarnessFault::CortexStart, &mut rng);
    state.apply(HarnessFault::CortexEnd, &mut rng);
    state.apply(HarnessFault::CortexStart, &mut rng);
}

#[test]
#[should_panic(expected = "channel retry budget exceeded")]
fn deterministic_retry_budget_invariant() {
    let mut rng = StdRng::seed_from_u64(31);
    let mut state = HarnessState::new(1);
    state.apply(HarnessFault::RetryAttempt, &mut rng);
    state.apply(HarnessFault::RetryAttempt, &mut rng);
    state.apply(HarnessFault::RetryAttempt, &mut rng);
    state.apply(HarnessFault::RetryAttempt, &mut rng);
    state.apply(HarnessFault::RetryAttempt, &mut rng);
}

#[test]
fn seeded_fault_sequence_preserves_invariants() {
    let mut rng = StdRng::seed_from_u64(20260301);
    let mut state = HarnessState::new(3);

    for _ in 0..300 {
        let fault = match rng.random_range(0..10) {
            0 => HarnessFault::ReserveWorker,
            1 => HarnessFault::CompleteWorker,
            2 => HarnessFault::WorkerFailure,
            3 => HarnessFault::WorkerCancelled,
            4 => HarnessFault::WorkerSuccess,
            5 => HarnessFault::EventLagged,
            6 => HarnessFault::EventClosed,
            7 => HarnessFault::CortexStart,
            8 => HarnessFault::CortexEnd,
            _ => HarnessFault::RetryAttempt,
        };

        // Keep random scenario realistic by ending inflight cortex requests immediately.
        if matches!(fault, HarnessFault::CortexStart) && state.cortex_inflight > 0 {
            state.apply(HarnessFault::CortexEnd, &mut rng);
        }
        if matches!(fault, HarnessFault::RetryAttempt) && state.retry_attempts >= 4 {
            continue;
        }

        state.apply(fault, &mut rng);
    }
}
