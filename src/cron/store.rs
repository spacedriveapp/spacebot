//! Cron job CRUD storage (SQLite).

use crate::cron::scheduler::CronConfig;
use crate::error::Result;
use anyhow::Context as _;
use sqlx::SqlitePool;

/// Cron job store for persistence.
#[derive(Debug)]
pub struct CronStore {
    pool: SqlitePool,
}

impl CronStore {
    /// Create a new cron store.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Save a cron job configuration.
    pub async fn save(&self, config: &CronConfig) -> Result<()> {
        let active_start = config.active_hours.map(|h| h.0 as i64);
        let active_end = config.active_hours.map(|h| h.1 as i64);

        sqlx::query(
            r#"
            INSERT INTO cron_jobs (id, prompt, interval_secs, delivery_target, active_start_hour, active_end_hour, enabled)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                prompt = excluded.prompt,
                interval_secs = excluded.interval_secs,
                delivery_target = excluded.delivery_target,
                active_start_hour = excluded.active_start_hour,
                active_end_hour = excluded.active_end_hour,
                enabled = excluded.enabled
            "#
        )
        .bind(&config.id)
        .bind(&config.prompt)
        .bind(config.interval_secs as i64)
        .bind(&config.delivery_target)
        .bind(active_start)
        .bind(active_end)
        .bind(config.enabled as i64)
        .execute(&self.pool)
        .await
        .context("failed to save cron job")?;

        Ok(())
    }

    /// Load all enabled cron job configurations.
    pub async fn load_all(&self) -> Result<Vec<CronConfig>> {
        let rows = sqlx::query(
            r#"
            SELECT id, prompt, interval_secs, delivery_target, active_start_hour, active_end_hour, enabled
            FROM cron_jobs
            WHERE enabled = 1
            ORDER BY created_at ASC
            "#
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to load cron jobs")?;

        let configs = rows
            .into_iter()
            .map(|row| CronConfig {
                id: row.try_get("id").unwrap_or_default(),
                prompt: row.try_get("prompt").unwrap_or_default(),
                interval_secs: row.try_get::<i64, _>("interval_secs").unwrap_or(3600) as u64,
                delivery_target: row.try_get("delivery_target").unwrap_or_default(),
                active_hours: {
                    let start: Option<i64> = row.try_get("active_start_hour").ok();
                    let end: Option<i64> = row.try_get("active_end_hour").ok();
                    match (start, end) {
                        (Some(s), Some(e)) => Some((s as u8, e as u8)),
                        _ => None,
                    }
                },
                enabled: row.try_get::<i64, _>("enabled").unwrap_or(1) != 0,
            })
            .collect();

        Ok(configs)
    }

    /// Delete a cron job.
    pub async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete cron job")?;

        Ok(())
    }

    /// Update the enabled state of a cron job (used by circuit breaker).
    pub async fn update_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        sqlx::query("UPDATE cron_jobs SET enabled = ? WHERE id = ?")
            .bind(enabled as i64)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to update cron job enabled state")?;

        Ok(())
    }

    /// Log a cron job execution result.
    pub async fn log_execution(
        &self,
        cron_id: &str,
        success: bool,
        result_summary: Option<&str>,
    ) -> Result<()> {
        let execution_id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            r#"
            INSERT INTO cron_executions (id, cron_id, success, result_summary)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&execution_id)
        .bind(cron_id)
        .bind(success as i64)
        .bind(result_summary)
        .execute(&self.pool)
        .await
        .context("failed to log cron execution")?;

        Ok(())
    }

    /// Load all cron job configurations (including disabled).
    pub async fn load_all_unfiltered(&self) -> Result<Vec<CronConfig>> {
        let rows = sqlx::query(
            r#"
            SELECT id, prompt, interval_secs, delivery_target, active_start_hour, active_end_hour, enabled
            FROM cron_jobs
            ORDER BY created_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to load cron jobs")?;

        let configs = rows
            .into_iter()
            .map(|row| CronConfig {
                id: row.try_get("id").unwrap_or_default(),
                prompt: row.try_get("prompt").unwrap_or_default(),
                interval_secs: row.try_get::<i64, _>("interval_secs").unwrap_or(3600) as u64,
                delivery_target: row.try_get("delivery_target").unwrap_or_default(),
                active_hours: {
                    let start: Option<i64> = row.try_get("active_start_hour").ok();
                    let end: Option<i64> = row.try_get("active_end_hour").ok();
                    match (start, end) {
                        (Some(s), Some(e)) => Some((s as u8, e as u8)),
                        _ => None,
                    }
                },
                enabled: row.try_get::<i64, _>("enabled").unwrap_or(1) != 0,
            })
            .collect();

        Ok(configs)
    }

    /// Load execution history for a specific cron job.
    pub async fn load_executions(
        &self,
        cron_id: &str,
        limit: i64,
    ) -> Result<Vec<CronExecutionEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, executed_at, success, result_summary
            FROM cron_executions
            WHERE cron_id = ?
            ORDER BY executed_at DESC
            LIMIT ?
            "#,
        )
        .bind(cron_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to load cron executions")?;

        let entries = rows
            .into_iter()
            .map(|row| CronExecutionEntry {
                id: row.try_get("id").unwrap_or_default(),
                executed_at: row.try_get("executed_at").unwrap_or_default(),
                success: row.try_get::<i64, _>("success").unwrap_or(0) != 0,
                result_summary: row.try_get("result_summary").ok(),
            })
            .collect();

        Ok(entries)
    }

    /// Load recent execution history across all cron jobs.
    pub async fn load_all_executions(&self, limit: i64) -> Result<Vec<CronExecutionEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, cron_id, executed_at, success, result_summary
            FROM cron_executions
            ORDER BY executed_at DESC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to load cron executions")?;

        let entries = rows
            .into_iter()
            .map(|row| CronExecutionEntry {
                id: row.try_get("id").unwrap_or_default(),
                executed_at: row.try_get("executed_at").unwrap_or_default(),
                success: row.try_get::<i64, _>("success").unwrap_or(0) != 0,
                result_summary: row.try_get("result_summary").ok(),
            })
            .collect();

        Ok(entries)
    }

    /// Get execution stats for a cron job (success count, failure count, last execution).
    pub async fn get_execution_stats(&self, cron_id: &str) -> Result<CronExecutionStats> {
        let row = sqlx::query(
            r#"
            SELECT
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success_count,
                SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failure_count,
                MAX(executed_at) as last_executed_at
            FROM cron_executions
            WHERE cron_id = ?
            "#,
        )
        .bind(cron_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load cron execution stats")?;

        if let Some(row) = row {
            let success_count: i64 = row.try_get("success_count").unwrap_or(0);
            let failure_count: i64 = row.try_get("failure_count").unwrap_or(0);
            let last_executed_at: Option<String> = row.try_get("last_executed_at").ok();

            Ok(CronExecutionStats {
                success_count: success_count as u64,
                failure_count: failure_count as u64,
                last_executed_at,
            })
        } else {
            Ok(CronExecutionStats::default())
        }
    }
}

/// Entry in the cron execution log.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CronExecutionEntry {
    pub id: String,
    pub executed_at: String,
    pub success: bool,
    pub result_summary: Option<String>,
}

/// Execution statistics for a cron job.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct CronExecutionStats {
    pub success_count: u64,
    pub failure_count: u64,
    pub last_executed_at: Option<String>,
}

use sqlx::Row as _;
