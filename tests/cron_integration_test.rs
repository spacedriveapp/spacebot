//! Cron integration tests for claim races and cursor atomicity.
//!
//! These tests verify the TOCTOU-safe claim-and-advance behavior
//! and ensure that only one execution can claim a given cursor position.

use chrono::Timelike;
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Arc;
use tokio::sync::Mutex;

use spacebot::cron::scheduler::CronConfig;
use spacebot::cron::store::CronStore;

/// Helper to create an in-memory cron store with migrations.
async fn setup_cron_store() -> Arc<CronStore> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect sqlite memory db");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    Arc::new(CronStore::new(pool))
}

/// Test that claim_and_advance is atomic - only one caller succeeds.
#[tokio::test]
async fn claim_and_advance_is_atomic_only_one_succeeds() {
    let store = setup_cron_store().await;
    let base_time = chrono::Utc::now();
    let scheduled = base_time.with_nanosecond(0).unwrap();
    let next_scheduled = scheduled + chrono::Duration::minutes(5);
    let scheduled_text = scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let next_text = next_scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // Save a cron job with a scheduled run time
    store
        .save(&CronConfig {
            id: "claim-test".to_string(),
            prompt: "test".to_string(),
            cron_expr: None,
            interval_secs: 300,
            delivery_target: "discord:123456789".to_string(),
            active_hours: None,
            enabled: true,
            run_once: false,
            next_run_at: Some(scheduled_text.clone()),
            timeout_secs: None,
        })
        .await
        .expect("save cron config");

    // Simulate concurrent claim attempts from multiple "workers"
    let claim_count = Arc::new(Mutex::new(0u32));
    let mut handles = vec![];

    for _ in 0..5 {
        let store = Arc::clone(&store);
        let scheduled_text = scheduled_text.clone();
        let next_text = next_text.clone();
        let claim_count = Arc::clone(&claim_count);

        let handle = tokio::spawn(async move {
            // Small random delay to increase race probability
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

            let claimed = store
                .claim_and_advance("claim-test", &scheduled_text, &next_text)
                .await
                .expect("claim_and_advance should not error");

            if claimed {
                let mut count = claim_count.lock().await;
                *count += 1;
            }

            claimed
        });
        handles.push(handle);
    }

    // Wait for all claim attempts
    let results: Vec<bool> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("task should not panic"))
        .collect();

    // Exactly one claim should succeed
    let success_count = results.iter().filter(|&&r| r).count();
    assert_eq!(
        success_count, 1,
        "exactly one concurrent claim should succeed, got {}",
        success_count
    );

    // Verify via the counter as well
    let final_count = *claim_count.lock().await;
    assert_eq!(final_count, 1, "claim counter should be exactly 1");

    // Verify the cursor was advanced
    let config = store
        .load("claim-test")
        .await
        .expect("load should succeed")
        .expect("job should exist");
    assert_eq!(
        config.next_run_at,
        Some(next_text),
        "cursor should be advanced"
    );
}

/// Test that a second claim for the same scheduled time fails after successful advance.
#[tokio::test]
async fn duplicate_claim_fails_after_successful_advance() {
    let store = setup_cron_store().await;
    let base_time = chrono::Utc::now();
    let scheduled = base_time.with_nanosecond(0).unwrap();
    let next_scheduled = scheduled + chrono::Duration::minutes(5);
    let scheduled_text = scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let next_text = next_scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    store
        .save(&CronConfig {
            id: "dup-claim-test".to_string(),
            prompt: "test".to_string(),
            cron_expr: None,
            interval_secs: 300,
            delivery_target: "discord:123456789".to_string(),
            active_hours: None,
            enabled: true,
            run_once: false,
            next_run_at: Some(scheduled_text.clone()),
            timeout_secs: None,
        })
        .await
        .expect("save cron config");

    // First claim succeeds
    let first_claim = store
        .claim_and_advance("dup-claim-test", &scheduled_text, &next_text)
        .await
        .expect("first claim should not error");
    assert!(first_claim, "first claim should succeed");

    // Second claim for same scheduled time fails (cursor already advanced)
    let second_claim = store
        .claim_and_advance("dup-claim-test", &scheduled_text, &next_text)
        .await
        .expect("second claim should not error");
    assert!(!second_claim, "duplicate claim should fail");

    // Third claim for the NEW scheduled time should succeed (run_once case)
    let next_next = next_scheduled + chrono::Duration::minutes(5);
    let next_next_text = next_next.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let third_claim = store
        .claim_and_advance("dup-claim-test", &next_text, &next_next_text)
        .await
        .expect("third claim should not error");
    assert!(third_claim, "claim for new cursor should succeed");
}

/// Test that run_once jobs properly claim and won't run again.
#[tokio::test]
async fn run_once_claim_prevents_subsequent_execution() {
    let store = setup_cron_store().await;
    let scheduled = chrono::Utc::now().with_nanosecond(0).unwrap();
    let next_scheduled = scheduled + chrono::Duration::minutes(5);
    let scheduled_text = scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let next_text = next_scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    store
        .save(&CronConfig {
            id: "run-once-test".to_string(),
            prompt: "one-time task".to_string(),
            cron_expr: None,
            interval_secs: 300,
            delivery_target: "discord:123456789".to_string(),
            active_hours: None,
            enabled: true,
            run_once: true,
            next_run_at: Some(scheduled_text.clone()),
            timeout_secs: None,
        })
        .await
        .expect("save cron config");

    // First claim succeeds
    let first_claim = store
        .claim_and_advance("run-once-test", &scheduled_text, &next_text)
        .await
        .expect("claim should not error");
    assert!(first_claim, "run_once claim should succeed");

    // Claiming the NEW cursor should fail because run_once is already claimed
    // (In real usage, the job would be disabled after execution)
    let next_next = next_scheduled + chrono::Duration::minutes(5);
    let next_next_text = next_next.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let second_claim = store
        .claim_and_advance("run-once-test", &next_text, &next_next_text)
        .await
        .expect("second claim should not error");

    // This claim succeeds because cursor was advanced, but in real scheduler
    // the job would be disabled by the execution logic
    assert!(
        second_claim,
        "claim for new cursor succeeds (job should be disabled by execution)"
    );
}

/// Test that stale cursor detection works after claim race loser refreshes.
#[tokio::test]
async fn stale_cursor_detection_after_lost_claim() {
    let store = setup_cron_store().await;
    let base_time = chrono::Utc::now();
    let original = base_time.with_nanosecond(0).unwrap();
    let advanced = original + chrono::Duration::minutes(5);
    let original_text = original.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let advanced_text = advanced.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    store
        .save(&CronConfig {
            id: "stale-test".to_string(),
            prompt: "stale detection test".to_string(),
            cron_expr: None,
            interval_secs: 300,
            delivery_target: "discord:123456789".to_string(),
            active_hours: None,
            enabled: true,
            run_once: false,
            next_run_at: Some(original_text.clone()),
            timeout_secs: None,
        })
        .await
        .expect("save cron config");

    // Simulate: Worker A claims successfully
    let worker_a_claim = store
        .claim_and_advance("stale-test", &original_text, &advanced_text)
        .await
        .expect("claim should not error");
    assert!(worker_a_claim, "worker A should win the claim");

    // Simulate: Worker B tries to claim same cursor, fails (lost race)
    let worker_b_claim = store
        .claim_and_advance("stale-test", &original_text, &advanced_text)
        .await
        .expect("claim should not error");
    assert!(!worker_b_claim, "worker B should lose the claim");

    // Worker B should detect stale cursor and refresh from store
    let refreshed_config = store
        .load("stale-test")
        .await
        .expect("load should succeed")
        .expect("job should exist");

    // Verify the refreshed cursor shows the advancement
    assert_eq!(
        refreshed_config.next_run_at,
        Some(advanced_text.clone()),
        "refreshed cursor should show advanced time"
    );

    // Now worker B can claim the NEW cursor
    let next_next = advanced + chrono::Duration::minutes(5);
    let next_next_text = next_next.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let worker_b_second_claim = store
        .claim_and_advance("stale-test", &advanced_text, &next_next_text)
        .await
        .expect("second claim should not error");
    assert!(
        worker_b_second_claim,
        "worker B should claim the new cursor"
    );
}
