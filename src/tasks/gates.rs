//! External-state gates: park a task until something outside this system says go.
//!
//! A dependency edge says "wait for that task". A gate says "wait for that
//! *fact*" — CI is green, the branch merged, an upstream task returned a
//! particular value. The scheduler needs no new concept: a task with an
//! unsatisfied gate is not promotable, exactly as a task with an unfinished
//! parent is not.
//!
//! The thing to get right here is not the polling. It is that a gate can be
//! unsatisfied for reasons that recover completely differently, and collapsing
//! them is how this codebase already shipped an infinite loop once. `pending`
//! resolves itself and should be polled. `failed` will not, and polling it is
//! pure waste — the task has to stop and wait for a person. `erroring` is our
//! problem rather than the graph's, and reading it as `failed` would tell
//! someone CI went red when in fact we could not reach CI at all.

use crate::error::Result;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};
use std::time::Duration;

/// How long a single gate evaluation may take.
///
/// This is the first thing in the system that makes outbound requests on a
/// timer with nobody watching, so it gets a hard ceiling rather than inheriting
/// a default from somewhere.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Consecutive evaluation errors before a gate stops polling and asks for help.
///
/// Without this, a gate pointed at a URL that will never resolve polls forever.
/// The point is not to give up on the task — it is to make an unreachable
/// endpoint *visible* instead of silently expensive.
pub const GATE_ERROR_LIMIT: i64 = 5;

/// Longest gap between polls when a gate keeps erroring.
const MAX_BACKOFF_SECS: i64 = 900;

/// What kind of fact a gate waits on.
///
/// Deliberately no vendor SDKs. `Http` covers GitHub, GitLab, Buildkite, and
/// Jenkins without knowing what any of them are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// Poll a URL; assert on the status, or on a JSON Pointer into the body.
    Http,
    /// Read an upstream task's stored outputs and compare at a JSON Pointer.
    ///
    /// This is also where conditional steps live — "run the rollback only if
    /// deploy reported failure" is a gate, not a second predicate language.
    TaskOutput,
}

impl GateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GateKind::Http => "http",
            GateKind::TaskOutput => "task_output",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "http" => Some(GateKind::Http),
            "task_output" => Some(GateKind::TaskOutput),
            _ => None,
        }
    }
}

/// The state of a gate, and the reason the four are not three.
///
/// See the module docs. Each variant answers a different question: should we
/// poll again, should the task run, and whose problem is it?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateResult {
    /// Not true yet. May become true on its own; keep polling.
    Pending,
    /// True. Latched — never re-evaluated.
    Satisfied,
    /// Definitively false. Polling will not fix it; a person must.
    Failed,
    /// We could not tell. Our problem, not the graph's.
    Erroring,
}

impl GateResult {
    pub fn as_str(self) -> &'static str {
        match self {
            GateResult::Pending => "pending",
            GateResult::Satisfied => "satisfied",
            GateResult::Failed => "failed",
            GateResult::Erroring => "erroring",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(GateResult::Pending),
            "satisfied" => Some(GateResult::Satisfied),
            "failed" => Some(GateResult::Failed),
            "erroring" => Some(GateResult::Erroring),
            _ => None,
        }
    }

    /// Whether this state can change by looking again.
    ///
    /// `Satisfied` is a latch: a gate that has opened stays open, because the
    /// alternative is un-starting work that is already running. `Failed` needs
    /// a person, and re-polling it is the waste this distinction exists to
    /// prevent.
    pub fn is_worth_polling(self) -> bool {
        matches!(self, GateResult::Pending | GateResult::Erroring)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskGate {
    pub id: String,
    pub task_number: i64,
    pub kind: GateKind,
    pub config: Value,
    /// What a person should read on the board. "waiting for CI on main" beats
    /// a URL.
    pub label: Option<String>,
    pub poll_interval_secs: i64,
    pub last_checked_at: Option<String>,
    pub last_result: GateResult,
    pub last_detail: Option<String>,
    pub consecutive_errors: i64,
    pub created_at: String,
}

/// Why a gate was refused at creation.
///
/// Checked up front so a malformed gate is rejected while a person is still
/// looking at it, rather than erroring once a minute forever with nobody
/// reading the log.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GateConfigError {
    #[error("`{value}` is not a gate kind — use http or task_output")]
    UnknownKind { value: String },
    #[error("an {kind} gate needs `{field}` in its config")]
    MissingField {
        kind: &'static str,
        field: &'static str,
    },
    #[error("`{url}` is not an http or https URL")]
    UnsupportedScheme { url: String },
    #[error("an http gate needs at least one of `expect_status` or `pointer`")]
    NoAssertion,
    #[error("poll interval must be at least {min} seconds")]
    PollTooFast { min: i64 },
}

/// The minimum gap between polls of one gate.
///
/// Not a preference. A gate is server-side, unattended, and repeating, so a
/// five-second interval is a way to have this instance mistaken for a denial of
/// service by whatever it is polling.
pub const MIN_POLL_INTERVAL_SECS: i64 = 15;

/// Validate a gate before it is stored.
pub fn validate_config(
    kind: GateKind,
    config: &Value,
    poll_interval_secs: i64,
) -> std::result::Result<(), GateConfigError> {
    if poll_interval_secs < MIN_POLL_INTERVAL_SECS {
        return Err(GateConfigError::PollTooFast {
            min: MIN_POLL_INTERVAL_SECS,
        });
    }

    match kind {
        GateKind::Http => {
            let url = config
                .get("url")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(GateConfigError::MissingField {
                    kind: "http",
                    field: "url",
                })?;

            // Scheme is checked rather than assumed. A gate URL is stored,
            // repeated, and made from the server, so `file://` and friends are
            // not merely useless here — they are a way to make this process
            // read things on someone else's behalf.
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(GateConfigError::UnsupportedScheme {
                    url: url.to_string(),
                });
            }

            // A gate with no assertion is satisfied by any response, which
            // means it is not a gate. Refuse it rather than quietly opening.
            if config.get("expect_status").is_none() && config.get("pointer").is_none() {
                return Err(GateConfigError::NoAssertion);
            }
        }
        GateKind::TaskOutput => {
            if config.get("task_number").and_then(Value::as_i64).is_none() {
                return Err(GateConfigError::MissingField {
                    kind: "task_output",
                    field: "task_number",
                });
            }
            if config.get("pointer").and_then(Value::as_str).is_none() {
                return Err(GateConfigError::MissingField {
                    kind: "task_output",
                    field: "pointer",
                });
            }
        }
    }

    Ok(())
}

/// One evaluation's verdict.
pub struct Evaluation {
    pub result: GateResult,
    /// Why, in words a person can act on.
    pub detail: String,
}

impl Evaluation {
    fn new(result: GateResult, detail: impl Into<String>) -> Self {
        Self {
            result,
            detail: detail.into(),
        }
    }
}

pub struct GateStore {
    pool: SqlitePool,
}

impl GateStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn create(
        &self,
        task_number: i64,
        kind: GateKind,
        config: &Value,
        label: Option<&str>,
        poll_interval_secs: i64,
    ) -> Result<TaskGate> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_gates \
                 (id, task_number, kind, config, label, poll_interval_secs) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(task_number)
        .bind(kind.as_str())
        .bind(config.to_string())
        .bind(label)
        .bind(poll_interval_secs)
        .execute(&self.pool)
        .await
        .context("failed to create task gate")?;

        self.get(&id)
            .await?
            .context("gate inserted but not found")
            .map_err(Into::into)
    }

    pub async fn get(&self, id: &str) -> Result<Option<TaskGate>> {
        let row = sqlx::query(&format!("{SELECT_COLUMNS} FROM task_gates WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch task gate")?;
        row.map(gate_from_row).transpose()
    }

    pub async fn list_for_task(&self, task_number: i64) -> Result<Vec<TaskGate>> {
        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM task_gates WHERE task_number = ? ORDER BY created_at ASC"
        ))
        .bind(task_number)
        .fetch_all(&self.pool)
        .await
        .context("failed to list task gates")?;
        rows.into_iter().map(gate_from_row).collect()
    }

    pub async fn delete(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM task_gates WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete task gate")?;
        Ok(result.rows_affected() > 0)
    }

    /// Gates worth looking at again, oldest check first.
    ///
    /// Filters on `last_result` in SQL rather than fetching everything and
    /// deciding in Rust, because the whole point of latching `satisfied` and
    /// parking `failed` is that they cost nothing afterwards.
    pub async fn due_for_poll(&self, now_unix: i64) -> Result<Vec<TaskGate>> {
        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM task_gates \
             WHERE last_result IN ('pending', 'erroring') \
             ORDER BY last_checked_at IS NOT NULL, last_checked_at ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .context("failed to list gates due for polling")?;

        let gates: Vec<TaskGate> = rows
            .into_iter()
            .map(gate_from_row)
            .collect::<Result<Vec<_>>>()?;

        Ok(gates
            .into_iter()
            .filter(|gate| gate.is_due(now_unix))
            .collect())
    }

    /// Record what an evaluation found.
    ///
    /// `consecutive_errors` resets on any conclusive answer, because the count
    /// exists to detect a *persistently* unreachable endpoint — one flake
    /// between two successful polls is not that.
    pub async fn record_evaluation(&self, id: &str, evaluation: &Evaluation) -> Result<()> {
        let error_delta = if evaluation.result == GateResult::Erroring {
            "consecutive_errors + 1"
        } else {
            "0"
        };

        sqlx::query(&format!(
            "UPDATE task_gates SET last_result = ?, last_detail = ?, \
             last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), \
             consecutive_errors = {error_delta} WHERE id = ?"
        ))
        .bind(evaluation.result.as_str())
        .bind(&evaluation.detail)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to record gate evaluation")?;
        Ok(())
    }

    /// The gates currently holding a task back, with the reason each gives.
    ///
    /// Empty means nothing is holding it, which is what the sweep asks.
    pub async fn blocking_gates(&self, task_number: i64) -> Result<Vec<TaskGate>> {
        Ok(self
            .list_for_task(task_number)
            .await?
            .into_iter()
            .filter(|gate| gate.last_result != GateResult::Satisfied)
            .collect())
    }
}

impl TaskGate {
    /// Whether this gate should be polled now.
    ///
    /// Errors back off geometrically. A gate whose endpoint is down should not
    /// be asked once a minute forever, and the backoff is capped so a gate that
    /// recovers after a long outage still notices within a quarter of an hour.
    pub fn is_due(&self, now_unix: i64) -> bool {
        if !self.last_result.is_worth_polling() {
            return false;
        }
        if self.consecutive_errors >= GATE_ERROR_LIMIT {
            return false;
        }
        let Some(checked) = self
            .last_checked_at
            .as_deref()
            .and_then(parse_timestamp_to_unix)
        else {
            // Never checked. Due immediately — a fresh gate should not make the
            // task wait out a poll interval before anyone even looks.
            return true;
        };

        let backoff = self
            .poll_interval_secs
            .saturating_mul(1i64 << self.consecutive_errors.clamp(0, 8))
            .min(MAX_BACKOFF_SECS);
        let interval = if self.consecutive_errors > 0 {
            backoff
        } else {
            self.poll_interval_secs
        };

        now_unix - checked >= interval
    }

    /// One line explaining why this gate is holding the task.
    pub fn explain(&self) -> String {
        let name = self
            .label
            .clone()
            .unwrap_or_else(|| self.kind.as_str().to_string());
        match self.last_result {
            GateResult::Satisfied => format!("{name}: satisfied"),
            GateResult::Pending => match &self.last_detail {
                Some(detail) => format!("{name}: waiting — {detail}"),
                None => format!("{name}: waiting"),
            },
            GateResult::Failed => match &self.last_detail {
                Some(detail) => format!("{name}: will not open — {detail}"),
                None => format!("{name}: will not open"),
            },
            GateResult::Erroring => {
                let detail = self.last_detail.as_deref().unwrap_or("no detail");
                format!(
                    "{name}: cannot be checked ({} consecutive errors) — {detail}",
                    self.consecutive_errors
                )
            }
        }
    }
}

/// Evaluate an HTTP gate.
///
/// The three outcomes map onto the states deliberately: an assertion that does
/// not hold yet is `Pending`, an explicitly negative answer is `Failed`, and a
/// request we could not complete is `Erroring`. Getting this mapping wrong is
/// how "CI is red" becomes indistinguishable from "CI is unreachable".
pub async fn evaluate_http(config: &Value) -> Evaluation {
    let Some(url) = config.get("url").and_then(Value::as_str) else {
        return Evaluation::new(GateResult::Erroring, "gate config has no url");
    };

    let client = match reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        // A gate follows redirects a little, not forever. An open redirect
        // chain is a way to spend this process's time.
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return Evaluation::new(GateResult::Erroring, format!("http client: {error}"));
        }
    };

    let mut request = client.get(url);
    if let Some(headers) = config.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.header(name, value);
            }
        }
    }

    let response = match request.send().await {
        Ok(response) => response,
        // Unreachable is not the same as negative. Saying "failed" here would
        // tell someone their build broke when in fact DNS did.
        Err(error) => {
            return Evaluation::new(GateResult::Erroring, format!("request failed: {error}"));
        }
    };

    let status = response.status().as_u16();

    if let Some(expected) = config.get("expect_status").and_then(Value::as_i64) {
        if status as i64 == expected {
            if config.get("pointer").is_none() {
                return Evaluation::new(GateResult::Satisfied, format!("status {status}"));
            }
        } else {
            // A 5xx is the endpoint having a bad day, not an answer about the
            // thing we asked about — it should back off, not park the task.
            let result = if (500..600).contains(&status) {
                GateResult::Erroring
            } else {
                GateResult::Pending
            };
            return Evaluation::new(result, format!("status {status}, expected {expected}"));
        }
    }

    let Some(pointer) = config.get("pointer").and_then(Value::as_str) else {
        return Evaluation::new(GateResult::Satisfied, format!("status {status}"));
    };

    let body: Value = match response.json().await {
        Ok(body) => body,
        Err(error) => {
            return Evaluation::new(
                GateResult::Erroring,
                format!("response was not JSON: {error}"),
            );
        }
    };

    evaluate_pointer(&body, pointer, config, "response")
}

/// Evaluate a gate against an upstream task's stored outputs.
pub fn evaluate_task_output(config: &Value, outputs: Option<&Value>) -> Evaluation {
    let pointer = config.get("pointer").and_then(Value::as_str).unwrap_or("");
    let Some(outputs) = outputs else {
        // Not an error: the upstream task simply has not finished. This is the
        // ordinary case on every tick before it does.
        return Evaluation::new(
            GateResult::Pending,
            "upstream task has not produced outputs yet",
        );
    };
    evaluate_pointer(outputs, pointer, config, "outputs")
}

/// Compare the value at `pointer` against the gate's expectation.
///
/// `equals` is an exact match; `any_of` is a set. With neither, mere presence
/// of a non-null value satisfies the gate — which is the common "wait until
/// this field exists" case and is stated explicitly rather than inferred.
fn evaluate_pointer(document: &Value, pointer: &str, config: &Value, what: &str) -> Evaluation {
    let found = if pointer.is_empty() {
        Some(document)
    } else {
        document.pointer(pointer)
    };

    let Some(found) = found else {
        return Evaluation::new(
            GateResult::Pending,
            format!("{what} has nothing at `{pointer}`"),
        );
    };

    if let Some(expected) = config.get("equals") {
        return if found == expected {
            Evaluation::new(GateResult::Satisfied, format!("`{pointer}` is {expected}"))
        } else {
            let result = failed_or_pending(config);
            Evaluation::new(
                result,
                format!("`{pointer}` is {found}, expected {expected}"),
            )
        };
    }

    if let Some(options) = config.get("any_of").and_then(Value::as_array) {
        return if options.contains(found) {
            Evaluation::new(GateResult::Satisfied, format!("`{pointer}` is {found}"))
        } else {
            let result = failed_or_pending(config);
            Evaluation::new(
                result,
                format!("`{pointer}` is {found}, expected one of {options:?}"),
            )
        };
    }

    if found.is_null() {
        return Evaluation::new(GateResult::Pending, format!("`{pointer}` is null"));
    }

    Evaluation::new(GateResult::Satisfied, format!("`{pointer}` is present"))
}

/// Whether a mismatch is "not yet" or "no".
///
/// The default is `Pending`, because most gates wait for something that has not
/// happened. `fail_on_mismatch` is opt-in and is what makes "the build went
/// red" park the task for a person instead of polling a settled answer forever.
fn failed_or_pending(config: &Value) -> GateResult {
    if config
        .get("fail_on_mismatch")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        GateResult::Failed
    } else {
        GateResult::Pending
    }
}

const SELECT_COLUMNS: &str = "SELECT id, task_number, kind, config, label, poll_interval_secs, \
     last_checked_at, last_result, last_detail, consecutive_errors, created_at";

/// Row → gate, for callers outside this module.
///
/// The ready sweep reads gates as part of one batched query over its promotion
/// candidates rather than calling back per task, so it needs the mapping
/// without the query that normally comes with it.
pub(crate) fn gate_from_row_public(row: sqlx::sqlite::SqliteRow) -> Result<TaskGate> {
    gate_from_row(row)
}

fn gate_from_row(row: sqlx::sqlite::SqliteRow) -> Result<TaskGate> {
    let kind: String = row.try_get("kind").context("failed to read gate kind")?;
    let last_result: String = row
        .try_get("last_result")
        .context("failed to read gate result")?;
    let config: String = row
        .try_get("config")
        .context("failed to read gate config")?;

    Ok(TaskGate {
        id: row.try_get("id").context("failed to read gate id")?,
        task_number: row
            .try_get("task_number")
            .context("failed to read gate task_number")?,
        kind: GateKind::parse(&kind).unwrap_or(GateKind::Http),
        config: serde_json::from_str(&config).unwrap_or(Value::Null),
        label: row.try_get("label").ok().flatten(),
        poll_interval_secs: row.try_get("poll_interval_secs").unwrap_or(60),
        last_checked_at: row.try_get("last_checked_at").ok().flatten(),
        last_result: GateResult::parse(&last_result).unwrap_or(GateResult::Pending),
        last_detail: row.try_get("last_detail").ok().flatten(),
        consecutive_errors: row.try_get("consecutive_errors").unwrap_or(0),
        created_at: row
            .try_get("created_at")
            .context("failed to read gate created_at")?,
    })
}

/// Parse the two timestamp shapes this database contains.
///
/// SQLite's `strftime` writes `2026-08-03T11:18:58Z`, but some columns were
/// written by `CURRENT_TIMESTAMP`, which uses a space instead of the `T` and no
/// zone. An ISO-only parser silently matches nothing against those, which reads
/// exactly like a feature that is not running — it has already cost real time
/// in this codebase.
fn parse_timestamp_to_unix(value: &str) -> Option<i64> {
    let normalized = value.replace(' ', "T");
    let normalized = if normalized.ends_with('Z') {
        normalized
    } else {
        format!("{normalized}Z")
    };
    chrono::DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|parsed| parsed.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn gate(result: GateResult, errors: i64, checked: Option<&str>) -> TaskGate {
        TaskGate {
            id: "g1".into(),
            task_number: 1,
            kind: GateKind::Http,
            config: json!({}),
            label: None,
            poll_interval_secs: 60,
            last_checked_at: checked.map(str::to_string),
            last_result: result,
            last_detail: None,
            consecutive_errors: errors,
            created_at: "2026-08-03T00:00:00Z".into(),
        }
    }

    /// The load-bearing distinction. If `satisfied` were re-polled, a gate
    /// could close under a task that is already running; if `failed` were
    /// re-polled, a settled answer would be re-asked forever.
    #[test]
    fn only_unsettled_gates_are_worth_polling() {
        assert!(GateResult::Pending.is_worth_polling());
        assert!(GateResult::Erroring.is_worth_polling());
        assert!(!GateResult::Satisfied.is_worth_polling());
        assert!(!GateResult::Failed.is_worth_polling());
    }

    #[test]
    fn a_never_checked_gate_is_due_immediately() {
        assert!(gate(GateResult::Pending, 0, None).is_due(1_000_000));
    }

    #[test]
    fn a_recently_checked_gate_waits_out_its_interval() {
        let checked = "2026-08-03T00:00:00Z";
        let at = parse_timestamp_to_unix(checked).expect("parse");
        let gate = gate(GateResult::Pending, 0, Some(checked));
        assert!(!gate.is_due(at + 30), "30s into a 60s interval");
        assert!(gate.is_due(at + 60));
    }

    /// A gate whose endpoint is down must not be asked once a minute forever.
    #[test]
    fn errors_back_off_and_eventually_stop() {
        let checked = "2026-08-03T00:00:00Z";
        let at = parse_timestamp_to_unix(checked).expect("parse");

        let once = gate(GateResult::Erroring, 1, Some(checked));
        assert!(!once.is_due(at + 60), "one error doubles the interval");
        assert!(once.is_due(at + 120));

        let exhausted = gate(GateResult::Erroring, GATE_ERROR_LIMIT, Some(checked));
        assert!(
            !exhausted.is_due(at + 100_000),
            "a gate that cannot be reached stops polling and waits for a person"
        );
    }

    /// The timestamp format trap: two shapes live in this database.
    #[test]
    fn both_timestamp_shapes_parse() {
        assert_eq!(
            parse_timestamp_to_unix("2026-08-03T11:18:58Z"),
            parse_timestamp_to_unix("2026-08-03 11:18:58"),
        );
        assert!(parse_timestamp_to_unix("2026-08-03 11:18:58").is_some());
    }

    #[test]
    fn a_gate_with_no_assertion_is_refused() {
        let error = validate_config(GateKind::Http, &json!({"url": "https://ci/x"}), 60)
            .expect_err("a gate satisfied by any response is not a gate");
        assert!(matches!(error, GateConfigError::NoAssertion), "{error:?}");
    }

    #[test]
    fn a_non_http_url_is_refused() {
        let error = validate_config(
            GateKind::Http,
            &json!({"url": "file:///etc/passwd", "expect_status": 200}),
            60,
        )
        .expect_err("a stored, repeating, server-side request must not read local files");
        assert!(
            matches!(error, GateConfigError::UnsupportedScheme { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_hammering_poll_interval_is_refused() {
        let error = validate_config(
            GateKind::Http,
            &json!({"url": "https://ci/x", "expect_status": 200}),
            1,
        )
        .expect_err("unattended polling needs a floor");
        assert!(
            matches!(error, GateConfigError::PollTooFast { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_missing_upstream_output_is_pending_not_failed() {
        let evaluation = evaluate_task_output(&json!({"pointer": "/status"}), None);
        assert_eq!(evaluation.result, GateResult::Pending);
    }

    #[test]
    fn an_output_gate_opens_on_an_exact_match() {
        let evaluation = evaluate_task_output(
            &json!({"pointer": "/status", "equals": "green"}),
            Some(&json!({"status": "green"})),
        );
        assert_eq!(evaluation.result, GateResult::Satisfied);
    }

    /// The default for a mismatch is "not yet", because most gates wait for
    /// something that has not happened. Parking the task is opt-in.
    #[test]
    fn a_mismatch_waits_unless_told_it_is_final() {
        let waiting = evaluate_task_output(
            &json!({"pointer": "/status", "equals": "green"}),
            Some(&json!({"status": "running"})),
        );
        assert_eq!(waiting.result, GateResult::Pending);

        let settled = evaluate_task_output(
            &json!({"pointer": "/status", "equals": "green", "fail_on_mismatch": true}),
            Some(&json!({"status": "red"})),
        );
        assert_eq!(
            settled.result,
            GateResult::Failed,
            "a red build will not go green by being asked again"
        );
        assert!(settled.detail.contains("red"), "{}", settled.detail);
    }

    #[test]
    fn any_of_accepts_a_set() {
        let config = json!({"pointer": "/state", "any_of": ["success", "neutral"]});
        assert_eq!(
            evaluate_task_output(&config, Some(&json!({"state": "neutral"}))).result,
            GateResult::Satisfied
        );
        assert_eq!(
            evaluate_task_output(&config, Some(&json!({"state": "failure"}))).result,
            GateResult::Pending
        );
    }

    #[test]
    fn explain_distinguishes_unreachable_from_negative() {
        let mut failed = gate(GateResult::Failed, 0, None);
        failed.label = Some("CI on main".into());
        failed.last_detail = Some("`/state` is failure".into());
        assert!(
            failed.explain().contains("will not open"),
            "{}",
            failed.explain()
        );

        let mut erroring = gate(GateResult::Erroring, 3, None);
        erroring.label = Some("CI on main".into());
        erroring.last_detail = Some("request failed".into());
        let text = erroring.explain();
        assert!(text.contains("cannot be checked"), "{text}");
        assert!(
            !text.contains("will not open"),
            "unreachable must not read as a negative answer: {text}"
        );
    }
}
