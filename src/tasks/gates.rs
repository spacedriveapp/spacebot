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
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};
use std::net::IpAddr;
use std::time::Duration;

/// How long a single gate evaluation may take.
///
/// This is the first thing in the system that makes outbound requests on a
/// timer with nobody watching, so it gets a hard ceiling rather than inheriting
/// a default from somewhere.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of a response body a gate will hold in memory.
///
/// The timeout bounds how long a poll takes, not how much it downloads, and a
/// gzipped body decompresses as it streams, so an endpoint can answer with
/// effectively unbounded bytes. A gate whose assertion needs a bigger answer
/// than this needs a different design, not a bigger buffer.
const MAX_GATE_BODY_BYTES: usize = 1024 * 1024;

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
    /// It did not hold, and this condition routes rather than waits — so the
    /// task it guarded is settled and this gate is finished.
    ///
    /// Distinct from `Failed`, which is trouble needing a person. Routing is an
    /// ordinary outcome: the branch simply did not apply. And distinct from
    /// `Pending`, which is what this used to be left as — a gate that had
    /// already decided but still reported "not yet", while never being polled
    /// again to correct itself. One label for "undecided" and "decided, and I
    /// routed away" is the mistake this codebase keeps paying for.
    Routed,
}

impl GateResult {
    pub fn as_str(self) -> &'static str {
        match self {
            GateResult::Pending => "pending",
            GateResult::Satisfied => "satisfied",
            GateResult::Failed => "failed",
            GateResult::Routed => "routed",
            GateResult::Erroring => "erroring",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(GateResult::Pending),
            "satisfied" => Some(GateResult::Satisfied),
            "failed" => Some(GateResult::Failed),
            "routed" => Some(GateResult::Routed),
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

/// What a *false* answer from a gate means.
///
/// The entire difference between "is CI green yet?" and "should this branch
/// run?". The two ask the same predicate with opposite failure modes: waiting
/// forever is correct for the first and a deadlock for the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateDisposition {
    /// Not yet. Poll again — the behaviour every gate had before this existed.
    Wait,
    /// No. This step does not apply; it is settled and will never run.
    Route,
}

impl GateDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            GateDisposition::Wait => "wait",
            GateDisposition::Route => "route",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wait" => Some(GateDisposition::Wait),
            "route" => Some(GateDisposition::Route),
            _ => None,
        }
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
    /// What a false answer means, or `None` to derive it — see
    /// [`TaskGate::disposition_for`]. Nullable on purpose: a field the author
    /// has to set correctly for the graph not to deadlock is a field that will
    /// eventually be set wrong.
    pub disposition: Option<GateDisposition>,
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
    #[error(
        "`{url}` points at a private, loopback, or link-local address — a gate cannot poll the network this instance lives on"
    )]
    ForbiddenAddress { url: String },
    #[error("an http gate needs at least one of `expect_status` or `pointer`")]
    NoAssertion,
    #[error("`expect_status` must be an integer HTTP status code (100..=599)")]
    InvalidExpectStatus,
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
                    url: redact_url_userinfo(url),
                });
            }

            // Literal-IP hosts are judged now; hostnames are judged at
            // evaluation time, where DNS answers. A stored URL naming the
            // metadata endpoint by number must fail here, while a person is
            // still looking at the gate, rather than once a minute in a log
            // nobody reads.
            if forbidden_url_address(url).is_some() {
                return Err(GateConfigError::ForbiddenAddress {
                    url: redact_url_userinfo(url),
                });
            }

            // Presence is not enough: the evaluator reads `expect_status`
            // with `as_i64`, so a string `"200"` type-checks here and is then
            // silently dropped there, leaving a gate satisfied by any status.
            if let Some(expect_status) = config.get("expect_status") {
                let valid = expect_status
                    .as_i64()
                    .is_some_and(|status| (100..=599).contains(&status));
                if !valid {
                    return Err(GateConfigError::InvalidExpectStatus);
                }
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

    /// Store a gate. `disposition` of `None` means derive it at poll time.
    pub async fn create(
        &self,
        task_number: i64,
        kind: GateKind,
        config: &Value,
        label: Option<&str>,
        poll_interval_secs: i64,
        disposition: Option<GateDisposition>,
    ) -> Result<TaskGate> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_gates \
                 (id, task_number, kind, config, label, poll_interval_secs, disposition) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(task_number)
        .bind(kind.as_str())
        .bind(config.to_string())
        .bind(label)
        .bind(poll_interval_secs)
        .bind(disposition.map(GateDisposition::as_str))
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

    /// Drop every gate on one task. Returns how many were removed.
    ///
    /// For the paths that destroy the task itself — a rolled-back launch, in
    /// particular. A gate whose task is gone is a stored, repeating, outbound
    /// request nobody can see or cancel.
    pub async fn delete_for_task(&self, task_number: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM task_gates WHERE task_number = ?")
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to delete the gates of a task")?;
        Ok(result.rows_affected())
    }

    pub async fn delete(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM task_gates WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete task gate")?;
        Ok(result.rows_affected() > 0)
    }

    /// Claim the gates worth looking at again, oldest check first.
    ///
    /// A claim, not a read: each gate returned has its `last_checked_at`
    /// stamped inside the same transaction, so a second poller running at the
    /// same moment sees the fresh stamp and finds nothing due. Without this,
    /// every per-agent poll loop evaluates the same due gate in the same
    /// round, and each racer's `record_evaluation` adds its own
    /// `consecutive_errors` — the backoff and [`GATE_ERROR_LIMIT`] trip once
    /// per agent instead of once per failure. `record_evaluation` overwrites
    /// the stamp with the real check time when the evaluation lands, and a
    /// claim whose poller dies mid-flight simply becomes due again one
    /// interval later.
    ///
    /// Filters on `last_result` in SQL rather than fetching everything and
    /// deciding in Rust, because the whole point of latching `satisfied` and
    /// parking `failed` is that they cost nothing afterwards.
    ///
    /// Gates on a settled task are excluded for the same reason. A gate governs
    /// *promotion*, and a task that is done or skipped will not be promoted
    /// again, so asking its endpoint once a minute forever is pure waste. It is
    /// also what stops a `route` gate from re-deciding a branch it has already
    /// settled: the poll that skipped the task is the last one it gets.
    pub async fn claim_due_gates(&self, now_unix: i64) -> Result<Vec<TaskGate>> {
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open gate claim transaction")?;

        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM task_gates g \
             WHERE g.last_result IN ('pending', 'erroring') \
               AND EXISTS (\
                 SELECT 1 FROM tasks t \
                  WHERE t.task_number = g.task_number \
                    AND t.status NOT IN {settled}) \
             ORDER BY g.last_checked_at IS NOT NULL, g.last_checked_at ASC",
            settled = crate::tasks::store::SETTLED_STATUSES
        ))
        .fetch_all(&mut *transaction)
        .await
        .context("failed to list gates due for polling")?;

        let gates: Vec<TaskGate> = rows
            .into_iter()
            .map(gate_from_row)
            .collect::<Result<Vec<_>>>()?;

        let due: Vec<TaskGate> = gates
            .into_iter()
            .filter(|gate| gate.is_due(now_unix))
            .collect();

        for gate in &due {
            sqlx::query(
                "UPDATE task_gates \
                 SET last_checked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
                 WHERE id = ?",
            )
            .bind(&gate.id)
            .execute(&mut *transaction)
            .await
            .context("failed to stamp a claimed gate")?;
        }

        transaction
            .commit()
            .await
            .context("failed to commit gate claim")?;
        Ok(due)
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
    /// Whether looking again could ever change this gate's answer.
    ///
    /// Two ways for a gate to be finished without having opened, and neither is
    /// visible from `last_result` alone. `failed` will not become true by being
    /// asked again — a person has to act. And a gate that has errored
    /// [`GATE_ERROR_LIMIT`] times in a row has *stopped being asked*, so its
    /// `erroring` is not "we are still trying", it is "we gave up trying".
    ///
    /// This is the predicate that separates a run that is waiting from a run
    /// that is stuck, and it is written once here rather than at each caller
    /// because those two answers have opposite recoveries: the first is
    /// healthy, the second needs somebody. `is_due` below is this question plus
    /// the backoff clock, which is why it starts by asking it.
    pub fn can_still_open(&self) -> bool {
        self.last_result.is_worth_polling() && self.consecutive_errors < GATE_ERROR_LIMIT
    }

    /// Whether this gate should be polled now.
    ///
    /// Errors back off geometrically. A gate whose endpoint is down should not
    /// be asked once a minute forever, and the backoff is capped so a gate that
    /// recovers after a long outage still notices within a quarter of an hour.
    pub fn is_due(&self, now_unix: i64) -> bool {
        if !self.can_still_open() {
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

    /// What a false answer from this gate means, given the source's status.
    ///
    /// `source_status` is the status of the task a `task_output` gate reads,
    /// and is `None` for an `http` gate or a source that has vanished.
    ///
    /// The derivation is a fact rather than a heuristic: it asks whether the
    /// input can still change, which is precisely what separates "not yet"
    /// from "no". A settled source cannot produce a different answer later, so
    /// a false predicate against it is final. An http endpoint, or a source
    /// still running, can — so it waits.
    ///
    /// "Settled" here is [`TaskStatus::is_terminal`], which includes `skipped`
    /// as well as `done`. A source that will never run has answered just as
    /// definitively as one that finished, and treating it as "still might
    /// change" would leave the gated step waiting on a task nothing will ever
    /// touch — the exact deadlock this feature exists to remove.
    pub fn disposition_for(
        &self,
        source_status: Option<crate::tasks::TaskStatus>,
    ) -> GateDisposition {
        // The override wins outright. It exists for what the derivation cannot
        // see: an http gate polling a decision endpoint that really is final,
        // or a task_output condition that should hold the whole pipeline rather
        // than skip past it.
        if let Some(explicit) = self.disposition {
            return explicit;
        }

        match self.kind {
            GateKind::TaskOutput
                if source_status.is_some_and(crate::tasks::TaskStatus::is_terminal) =>
            {
                GateDisposition::Route
            }
            _ => GateDisposition::Wait,
        }
    }

    /// Whether this result settles the gated task as "does not apply".
    ///
    /// The disposition and the result answer different halves of one question,
    /// and this is the only place they are combined.
    pub fn routes_away(
        &self,
        source_status: Option<crate::tasks::TaskStatus>,
        result: GateResult,
    ) -> bool {
        should_route(self.disposition_for(source_status), result)
    }

    /// One line explaining why this gate is holding the task.
    pub fn explain(&self) -> String {
        let name = self
            .label
            .clone()
            .unwrap_or_else(|| self.kind.as_str().to_string());
        match self.last_result {
            GateResult::Satisfied => format!("{name}: satisfied"),
            GateResult::Routed => match &self.last_detail {
                Some(detail) => format!("{name}: ruled this step out — {detail}"),
                None => format!("{name}: ruled this step out"),
            },
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

/// What one polled gate turned out to be, for whoever is logging.
#[derive(Debug, Clone, PartialEq)]
pub struct GatePoll {
    pub gate_id: String,
    pub task_number: i64,
    /// What it said last time, so only changes need reporting.
    pub previous: GateResult,
    pub result: GateResult,
    pub detail: String,
    /// The task was settled by this poll: the condition does not hold and the
    /// gate's disposition says that means "does not apply" rather than
    /// "not yet". Carries the reason written to `skip_reason`.
    pub skipped: Option<String>,
}

/// Evaluate every gate that is due and act on what it found.
///
/// Lives here rather than in the cortex loop so the decision — evaluate,
/// record, and possibly settle the task — can be tested without standing up an
/// agent. The caller's job is reduced to logging and emitting events, which is
/// the part that genuinely needs the process around it.
pub async fn poll_gates_once(
    tasks: &crate::tasks::TaskStore,
    gates: &GateStore,
    now_unix: i64,
) -> Result<Vec<GatePoll>> {
    let due = gates.claim_due_gates(now_unix).await?;
    let mut polled = Vec::with_capacity(due.len());

    for gate in due {
        // The source is read once and kept, because two questions are asked of
        // it: what does the pointer say, and can it still change? Reading it
        // twice would let the answers come from different moments.
        let source = match gate.kind {
            GateKind::Http => None,
            GateKind::TaskOutput => {
                // Fresh rather than cached: the whole point of the gate is that
                // it is waiting for that value to change.
                match gate.config.get("task_number").and_then(Value::as_i64) {
                    Some(number) => tasks.get_by_number(number).await.ok().flatten(),
                    None => None,
                }
            }
        };

        let evaluation = match gate.kind {
            GateKind::Http => evaluate_http(&gate.config).await,
            GateKind::TaskOutput => evaluate_task_output(
                &gate.config,
                source.as_ref().and_then(|task| task.outputs.as_ref()),
            ),
        };

        let previous = gate.last_result;
        let result = evaluation.result;
        if let Err(error) = gates.record_evaluation(&gate.id, &evaluation).await {
            tracing::warn!(%error, gate_id = %gate.id, "failed to record gate evaluation");
            continue;
        }

        // The one thing this pass does that it did not do before.
        //
        // A gate whose disposition resolves to `route` is not asking "has it
        // happened yet" — it is asking "does this step apply", and the answer
        // is no. So the task is settled rather than polled again. Everything
        // else is unchanged: the backoff, the error limit, and all four result
        // states mean exactly what they meant.
        let mut skipped = None;
        if gate.routes_away(source.as_ref().map(|task| task.status), result) {
            let name = gate
                .label
                .clone()
                .unwrap_or_else(|| gate.kind.as_str().to_string());
            let reason = format!("condition `{name}` does not hold: {}", evaluation.detail);
            match tasks.skip_task(gate.task_number, &reason).await {
                Ok(true) => {
                    // Record what this gate actually did. Without it the row
                    // keeps whatever the evaluation said — `pending` — while
                    // `claim_due_gates` never looks at it again, so it reports
                    // "not yet" forever about a decision it already made.
                    let verdict = Evaluation {
                        result: GateResult::Routed,
                        detail: evaluation.detail.clone(),
                    };
                    if let Err(error) = gates.record_evaluation(&gate.id, &verdict).await {
                        tracing::warn!(
                            %error,
                            gate_id = %gate.id,
                            "settled a task but failed to record the condition's verdict"
                        );
                    }
                    skipped = Some(reason);
                }
                // Already settled, or already done. The sweep and this pass
                // racing over one branch is the ordinary case, not a fault.
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    %error,
                    task_number = gate.task_number,
                    gate_id = %gate.id,
                    "failed to settle a task whose condition does not hold"
                ),
            }
        }

        polled.push(GatePoll {
            gate_id: gate.id,
            task_number: gate.task_number,
            previous,
            result,
            detail: evaluation.detail,
            skipped,
        });
    }

    Ok(polled)
}

/// Whether this evaluation settles the gated task as "does not apply".
///
/// The single place the routing rule is written, and the single place
/// `erroring` is kept out of it.
///
/// Under `route`, the source cannot change its mind — that is what derived the
/// disposition, or what the author asserted by overriding it — so `pending` is
/// not "not yet", it is "we asked and the answer was not yes". `failed` is the
/// same answer stated more firmly. Both settle the branch.
///
/// `erroring` never does, and this is the most important line in the feature.
/// It means *we could not tell*: DNS failed, the endpoint 404s, the config is
/// wrong. Being unable to reach CI is not CI saying no, and skipping a branch
/// because a lookup failed would silently drop work nobody asked to drop. An
/// unreachable gate backs off and eventually stops polling for a person to
/// look at, exactly as it did before.
///
/// `satisfied` is not a false answer at all — the gate opened and the step runs.
pub fn should_route(disposition: GateDisposition, result: GateResult) -> bool {
    disposition == GateDisposition::Route
        && matches!(result, GateResult::Pending | GateResult::Failed)
}

/// Whether an address is somewhere a gate must never connect.
///
/// A gate is a stored, repeating, server-side request carrying config-chosen
/// credentials — the shape SSRF takes — so loopback, private, link-local
/// (cloud metadata endpoints live there), and unspecified addresses are
/// refused outright rather than made configurable.
fn is_forbidden_address(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address is the IPv4 address in another notation;
            // judge it by what it actually is.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_address(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.is_unique_local()
        }
    }
}

/// `Url::host_str` keeps the brackets on an IPv6 literal (`[::1]`); neither
/// `IpAddr` parsing nor a DNS lookup wants them.
fn unbracket_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

/// A URL as it may be written into an error or a stored detail: same scheme,
/// host, and path, no credentials.
///
/// `last_detail` is served to anyone who can read the task, so
/// `https://user:password@host/…` must never reach it with the password
/// still inside.
fn redact_url_userinfo(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    // `set_username`/`set_password` fail only for cannot-be-a-base URLs
    // (`mailto:` and friends), which cannot carry userinfo in the first place.
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return url.to_string();
    }
    parsed.to_string()
}

/// The address a URL's host literal points at, when that literal is somewhere
/// a gate must never connect.
///
/// Only literals are answered here — a hostname needs DNS, which validation
/// deliberately does not perform. Evaluation resolves and applies the same
/// rule to every answer.
fn forbidden_url_address(url: &str) -> Option<IpAddr> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let address: IpAddr = unbracket_host(parsed.host_str()?).parse().ok()?;
    is_forbidden_address(&address).then_some(address)
}

/// The client knobs every gate request shares.
///
/// Redirects are off, not limited: a redirect target is a second URL that
/// never went through the address check, and a 3xx is a perfectly good
/// definitive answer for a gate to assert on.
fn gate_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
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

    let client = match gate_http_client(url).await {
        Ok(client) => client,
        Err(evaluation) => return evaluation,
    };

    evaluate_http_with_client(config, &client).await
}

/// Build the client for one poll, with the target's addresses settled first.
///
/// DNS is resolved here and every answer checked against the forbidden set,
/// then the client is pinned to exactly those addresses — an answer that
/// changes between the check and the connection cannot smuggle a private
/// address past it. Configs are re-checked here rather than trusted from
/// validation because a stored gate outlives the moment it was written.
async fn gate_http_client(url: &str) -> std::result::Result<reqwest::Client, Evaluation> {
    let parsed = match reqwest::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Err(Evaluation::new(
                GateResult::Erroring,
                format!("invalid url: {error}"),
            ));
        }
    };
    let Some(host) = parsed.host_str().map(unbracket_host).map(str::to_string) else {
        return Err(Evaluation::new(
            GateResult::Erroring,
            format!("`{}` has no host", redact_url_userinfo(url)),
        ));
    };
    let Some(port) = parsed.port_or_known_default() else {
        return Err(Evaluation::new(
            GateResult::Erroring,
            format!("`{}` is not an http or https URL", redact_url_userinfo(url)),
        ));
    };

    let addresses: Vec<std::net::SocketAddr> =
        match tokio::net::lookup_host((host.as_str(), port)).await {
            Ok(resolved) => resolved.collect(),
            // Unreachable is not the same as negative. Saying "failed" here
            // would tell someone their build broke when in fact DNS did.
            Err(error) => {
                return Err(Evaluation::new(
                    GateResult::Erroring,
                    format!("dns lookup for `{host}` failed: {error}"),
                ));
            }
        };
    if addresses.is_empty() {
        return Err(Evaluation::new(
            GateResult::Erroring,
            format!("dns lookup for `{host}` returned no addresses"),
        ));
    }
    if let Some(address) = addresses
        .iter()
        .map(|address| address.ip())
        .find(is_forbidden_address)
    {
        return Err(Evaluation::new(
            GateResult::Erroring,
            format!(
                "refused to connect to `{host}`: {address} is a private, loopback, or link-local address"
            ),
        ));
    }

    gate_client_builder()
        .resolve_to_addrs(&host, &addresses)
        .build()
        .map_err(|error| Evaluation::new(GateResult::Erroring, format!("http client: {error}")))
}

/// The fetch-and-assert half of [`evaluate_http`], split from address
/// resolution so tests can run it against a loopback server.
async fn evaluate_http_with_client(config: &Value, client: &reqwest::Client) -> Evaluation {
    let Some(url) = config.get("url").and_then(Value::as_str) else {
        return Evaluation::new(GateResult::Erroring, "gate config has no url");
    };

    let mut request = client.get(url);
    if let Some(headers) = config.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                // Header values go through the same `secret:`/`env:`
                // indirection as provider credentials — a stored gate must
                // not need a plaintext token, and the literal string must
                // never be sent as if it were the secret.
                let Some(resolved) = crate::config::resolve_env_value(value) else {
                    return Evaluation::new(
                        GateResult::Erroring,
                        format!("header `{name}` names a secret that could not be resolved"),
                    );
                };
                request = request.header(name, resolved);
            }
        }
    }

    let response = match request.send().await {
        Ok(response) => response,
        // Unreachable is not the same as negative. Saying "failed" here would
        // tell someone their build broke when in fact DNS did.
        Err(error) => {
            // The error embeds the request URL, credentials and all; the
            // detail is stored and served, so the URL goes in redacted.
            return Evaluation::new(
                GateResult::Erroring,
                format!(
                    "request to {} failed: {}",
                    redact_url_userinfo(url),
                    error.without_url()
                ),
            );
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

    // Read with a ceiling rather than `.json()`: the timeout bounds duration,
    // not bytes, and decompression multiplies whatever arrives. Overflow is
    // `Erroring` — an endpoint we cannot safely read is our problem, not an
    // answer about the thing being waited on.
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return Evaluation::new(
                    GateResult::Erroring,
                    format!("failed to read the response body: {error}"),
                );
            }
        };
        if body.len() + chunk.len() > MAX_GATE_BODY_BYTES {
            return Evaluation::new(
                GateResult::Erroring,
                format!("response body exceeded {MAX_GATE_BODY_BYTES} bytes"),
            );
        }
        body.extend_from_slice(&chunk);
    }

    let body: Value = match serde_json::from_slice(&body) {
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
     last_checked_at, last_result, last_detail, consecutive_errors, disposition, created_at";

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
        disposition: row
            .try_get::<Option<String>, _>("disposition")
            .ok()
            .flatten()
            .as_deref()
            .and_then(GateDisposition::parse),
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
            disposition: None,
            created_at: "2026-08-03T00:00:00Z".into(),
        }
    }

    /// The load-bearing distinction. If `satisfied` were re-polled, a gate
    /// could close under a task that is already running; if `failed` were
    /// re-polled, a settled answer would be re-asked forever.
    /// A gate that routed has decided. Reporting `pending` afterwards — which
    /// is what the evaluation says, since routing fires on a *pending* mismatch
    /// — would leave the row claiming "not yet" about a settled branch, and
    /// `claim_due_gates` never looks again to correct it.
    #[test]
    fn a_routed_verdict_is_settled_and_reads_as_a_decision() {
        assert!(!GateResult::Routed.is_worth_polling());
        assert_eq!(GateResult::parse("routed"), Some(GateResult::Routed));
        assert_eq!(GateResult::Routed.as_str(), "routed");

        let mut gate = gate(GateResult::Routed, 0, None);
        gate.label = Some("deploy reported red".into());
        gate.last_detail = Some("`/status` is \"green\", expected \"red\"".into());
        let text = gate.explain();
        assert!(text.contains("ruled this step out"), "{text}");
        assert!(
            !text.contains("waiting"),
            "a decided condition must not read as waiting: {text}"
        );
    }

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

    // -- Disposition --------------------------------------------------------

    /// The derivation, which is the reason this field can be left unset.
    ///
    /// It is a fact rather than a heuristic: it asks whether the input can
    /// still change, which is exactly what separates "not yet" from "no". A
    /// source still running can produce a different answer later, so a false
    /// predicate against it waits. A source that has settled cannot, so a false
    /// predicate against it is final and routes the branch away.
    ///
    /// If this regresses in the `wait` direction, a decided branch deadlocks.
    /// If it regresses in the `route` direction, a step is skipped while the
    /// task it reads is still working.
    #[test]
    fn a_condition_waits_while_its_source_can_still_change_and_routes_once_it_cannot() {
        let mut gate = gate(GateResult::Pending, 0, None);
        gate.kind = GateKind::TaskOutput;

        for still_going in [
            crate::tasks::TaskStatus::Backlog,
            crate::tasks::TaskStatus::Ready,
            crate::tasks::TaskStatus::InProgress,
            crate::tasks::TaskStatus::Blocked,
            crate::tasks::TaskStatus::PendingApproval,
        ] {
            assert_eq!(
                gate.disposition_for(Some(still_going)),
                GateDisposition::Wait,
                "{still_going} can still produce the value"
            );
        }

        assert_eq!(
            gate.disposition_for(Some(crate::tasks::TaskStatus::Done)),
            GateDisposition::Route,
            "a finished source will not change its mind"
        );
        assert_eq!(
            gate.disposition_for(Some(crate::tasks::TaskStatus::Skipped)),
            GateDisposition::Route,
            "a source that will never run has answered just as definitively as \
             one that finished — treating it as 'might still change' is the \
             deadlock this feature removes"
        );

        // An http gate has no source task at all, so nothing here can say the
        // answer is final. It waits, which is what every gate did before.
        let mut external = gate.clone();
        external.kind = GateKind::Http;
        assert_eq!(external.disposition_for(None), GateDisposition::Wait);
        assert_eq!(
            gate.disposition_for(None),
            GateDisposition::Wait,
            "a task_output gate whose source has vanished must not route on a guess"
        );
    }

    /// The override beats the derivation, in both directions.
    ///
    /// It exists for what the derivation cannot see: an http endpoint whose
    /// answer really is final, and a condition on a finished task that should
    /// hold the whole pipeline for a person rather than skip past it.
    #[test]
    fn an_explicit_disposition_beats_the_derivation() {
        let mut external = gate(GateResult::Pending, 0, None);
        external.kind = GateKind::Http;
        external.disposition = Some(GateDisposition::Route);
        assert_eq!(
            external.disposition_for(None),
            GateDisposition::Route,
            "an author may declare an external answer final"
        );

        let mut settled_source = gate(GateResult::Pending, 0, None);
        settled_source.kind = GateKind::TaskOutput;
        settled_source.disposition = Some(GateDisposition::Wait);
        assert_eq!(
            settled_source.disposition_for(Some(crate::tasks::TaskStatus::Done)),
            GateDisposition::Wait,
            "an author may hold the pipeline instead of skipping past it"
        );
    }

    /// The most load-bearing line in the feature.
    ///
    /// `erroring` means *we could not tell*: DNS failed, the endpoint 404s, the
    /// config is wrong. It is our problem, not an answer. Routing on it would
    /// skip branches because a lookup failed — silently dropping work nobody
    /// asked to drop, with no record of the decision beyond "condition did not
    /// hold". No disposition, explicit or derived, may make that happen.
    #[test]
    fn an_unreachable_condition_never_routes_however_it_is_configured() {
        for disposition in [GateDisposition::Wait, GateDisposition::Route] {
            assert!(
                !should_route(disposition, GateResult::Erroring),
                "being unable to reach the endpoint is not the endpoint saying no"
            );
            assert!(
                !should_route(disposition, GateResult::Satisfied),
                "a satisfied condition is not a false answer at all"
            );
        }

        assert!(should_route(GateDisposition::Route, GateResult::Pending));
        assert!(should_route(GateDisposition::Route, GateResult::Failed));
        for result in [GateResult::Pending, GateResult::Failed] {
            assert!(
                !should_route(GateDisposition::Wait, result),
                "a waiting gate holds the task; it never settles it"
            );
        }

        let mut erroring = gate(GateResult::Erroring, 2, None);
        erroring.kind = GateKind::TaskOutput;
        assert!(
            !erroring.routes_away(Some(crate::tasks::TaskStatus::Done), GateResult::Erroring),
            "a settled source derives `route`, and it still must not route on an error"
        );
    }

    /// The poller's one new behaviour, end to end over a real store.
    ///
    /// A `route` condition whose predicate is decidedly false settles its task
    /// with a reason naming the pointer and what was found there — and the same
    /// predicate under `wait` leaves the task exactly where it was.
    #[tokio::test]
    async fn a_route_condition_that_does_not_hold_settles_its_task_and_a_waiting_one_does_not() {
        let tasks = crate::tasks::store::setup_test_store().await;
        let gates = GateStore::new(tasks.pool().clone());

        let decider = tasks
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-test".into(),
                assigned_agent_id: "agent-test".into(),
                title: "did the deploy go red?".into(),
                status: crate::tasks::TaskStatus::InProgress,
                created_by: "branch".into(),
                ..Default::default()
            })
            .await
            .expect("create");
        let mut branch = Vec::new();
        for title in ["rollback", "hold for a person"] {
            branch.push(
                tasks
                    .create(crate::tasks::CreateTaskInput {
                        owner_agent_id: "agent-test".into(),
                        assigned_agent_id: "agent-test".into(),
                        title: title.into(),
                        status: crate::tasks::TaskStatus::Backlog,
                        created_by: "branch".into(),
                        ..Default::default()
                    })
                    .await
                    .expect("create"),
            );
        }

        tasks
            .submit_outputs(decider.task_number, &serde_json::json!({"state": "green"}))
            .await
            .expect("outputs");
        tasks
            .update(
                decider.task_number,
                crate::tasks::UpdateTaskInput {
                    status: Some(crate::tasks::TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("finish")
            .expect("exists");

        let config = json!({
            "task_number": decider.task_number,
            "pointer": "/state",
            "equals": "red",
        });
        gates
            .create(
                branch[0].task_number,
                GateKind::TaskOutput,
                &config,
                Some("deploy went red"),
                60,
                None,
            )
            .await
            .expect("routing gate");
        gates
            .create(
                branch[1].task_number,
                GateKind::TaskOutput,
                &config,
                Some("deploy went red"),
                60,
                Some(GateDisposition::Wait),
            )
            .await
            .expect("waiting gate");

        // A real clock: `last_checked_at` is written by SQLite's `now`, so a
        // synthetic epoch would make every gate look checked in the future.
        let now = chrono::Utc::now().timestamp();
        let polled = poll_gates_once(&tasks, &gates, now).await.expect("poll");
        assert_eq!(polled.len(), 2);

        let settled = tasks
            .get_by_number(branch[0].task_number)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(
            settled.status,
            crate::tasks::TaskStatus::Skipped,
            "a derived-route condition on a finished source settles the branch"
        );
        let reason = settled.skip_reason.expect("a settled card says why");
        assert!(
            reason.contains("/state") && reason.contains("green"),
            "the reason has to name the pointer and what was found: {reason}"
        );

        let held = tasks
            .get_by_number(branch[1].task_number)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(
            held.status,
            crate::tasks::TaskStatus::Backlog,
            "the same predicate under `wait` holds the task instead of settling it"
        );

        // And the settled task's gate is not asked again: a decided branch does
        // not get re-decided, and polling a card nothing will promote is waste.
        let again = poll_gates_once(&tasks, &gates, now + 3_600)
            .await
            .expect("poll");
        assert_eq!(
            again.len(),
            1,
            "only the still-waiting gate is due; the settled task's is not"
        );
    }

    /// Two pollers (the per-agent cortex loops) hitting one due gate in the
    /// same round must produce one evaluation, not two. Each racer used to add
    /// its own `consecutive_errors`, so backoff and [`GATE_ERROR_LIMIT`] tripped
    /// once per agent instead of once per failure.
    ///
    /// The gate's URL is refused by the SSRF guard, so the evaluation errors
    /// deterministically without a packet leaving the host — exactly the
    /// `erroring` path whose error count this test is about.
    #[tokio::test]
    async fn concurrent_pollers_evaluate_a_due_gate_once() {
        let tasks = crate::tasks::store::setup_test_store().await;
        let gates = GateStore::new(tasks.pool().clone());

        let task = tasks
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-test".into(),
                assigned_agent_id: "agent-test".into(),
                title: "wait on CI".into(),
                status: crate::tasks::TaskStatus::Backlog,
                created_by: "branch".into(),
                ..Default::default()
            })
            .await
            .expect("create");
        let gate = gates
            .create(
                task.task_number,
                GateKind::Http,
                &json!({"url": "http://192.168.1.1/gate", "expect_status": 200}),
                Some("CI on main"),
                60,
                None,
            )
            .await
            .expect("gate");

        // A real clock: `last_checked_at` is written by SQLite's `now`, so a
        // synthetic epoch would make the gate look checked in the future.
        let now = chrono::Utc::now().timestamp();
        let (first, second) = tokio::join!(
            poll_gates_once(&tasks, &gates, now),
            poll_gates_once(&tasks, &gates, now),
        );
        let first = first.expect("first poll");
        let second = second.expect("second poll");
        assert_eq!(
            first.len() + second.len(),
            1,
            "the claim must hand the gate to exactly one poller: {first:?} {second:?}"
        );

        let stored = gates.get(&gate.id).await.expect("read").expect("exists");
        assert_eq!(stored.last_result, GateResult::Erroring);
        assert_eq!(
            stored.consecutive_errors, 1,
            "one unreachable endpoint is one error, however many pollers saw it"
        );
    }

    // -- evaluate_http ------------------------------------------------------

    /// A bare-bones HTTP endpoint on loopback: answers every connection with
    /// the given response and reports each request head it saw.
    ///
    /// The SSRF guard (correctly) refuses to talk to this server, so the
    /// fetch-and-assert half of evaluation is exercised through
    /// `evaluate_http_with_client`, and the guard itself is exercised through
    /// the public `evaluate_http` against literal addresses.
    struct TestEndpoint {
        url: String,
        seen: tokio::sync::mpsc::UnboundedReceiver<String>,
    }

    async fn start_endpoint(
        status_line: &'static str,
        extra_headers: &'static str,
        body: Vec<u8>,
    ) -> TestEndpoint {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let (seen_tx, seen) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let seen_tx = seen_tx.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 8192];
                    loop {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                head.extend_from_slice(&chunk[..read]);
                                if head.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    if seen_tx
                        .send(String::from_utf8_lossy(&head).into_owned())
                        .is_err()
                    {
                        return;
                    }
                    let response = format!(
                        "{status_line}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    // The final write has nothing after it: a disconnecting
                    // client ends the task either way, so no branch is needed.
                    drop(socket.write_all(&body).await);
                });
            }
        });
        TestEndpoint {
            url: format!("http://127.0.0.1:{port}/gate"),
            seen,
        }
    }

    #[tokio::test]
    async fn an_http_gate_opens_on_the_pointed_at_value() {
        let endpoint =
            start_endpoint("HTTP/1.1 200 OK", "", br#"{"status":"green"}"#.to_vec()).await;
        let client = gate_client_builder().build().expect("client");

        let evaluation = evaluate_http_with_client(
            &json!({"url": endpoint.url, "pointer": "/status", "equals": "green"}),
            &client,
        )
        .await;

        assert_eq!(
            evaluation.result,
            GateResult::Satisfied,
            "{}",
            evaluation.detail
        );
    }

    /// The SSRF guard: a gate is a stored, repeating, credentialed request
    /// made from this host, so the addresses this host can reach are exactly
    /// the ones it must refuse. Literal IPs never touch DNS, so the refusal
    /// is decided before a packet could leave.
    #[tokio::test]
    async fn an_http_gate_refuses_the_network_this_instance_lives_on() {
        for url in [
            "http://127.0.0.1:9/gate",
            "http://10.1.2.3/gate",
            "http://172.16.0.1/gate",
            "http://192.168.1.1/gate",
            "http://169.254.169.254/latest/meta-data/",
            "http://0.0.0.0/gate",
            "http://[::1]/gate",
            "http://[fe80::1]/gate",
            "http://[::ffff:127.0.0.1]/gate",
        ] {
            let evaluation = evaluate_http(&json!({"url": url, "expect_status": 200})).await;
            assert_eq!(
                evaluation.result,
                GateResult::Erroring,
                "{url} must be refused, got {}",
                evaluation.detail
            );
            assert!(
                evaluation
                    .detail
                    .contains("private, loopback, or link-local"),
                "{url}: {}",
                evaluation.detail
            );
        }
    }

    #[test]
    fn a_gate_at_a_private_address_is_refused_at_creation() {
        let error = validate_config(
            GateKind::Http,
            &json!({"url": "http://169.254.169.254/latest/meta-data/", "expect_status": 200}),
            60,
        )
        .expect_err("the cloud metadata endpoint must not survive validation");
        assert!(
            matches!(error, GateConfigError::ForbiddenAddress { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_hostname_is_not_refused_at_creation() {
        // Validation does not do DNS; a hostname's addresses are checked at
        // evaluation time, against whatever DNS says then.
        validate_config(
            GateKind::Http,
            &json!({"url": "http://ci.internal/x", "expect_status": 200}),
            60,
        )
        .expect("a hostname cannot be judged before it resolves");
    }

    /// A string `"200"` passes a presence check and is then silently dropped
    /// by the evaluator's `as_i64`, leaving a gate satisfied by any status.
    #[test]
    fn a_non_integer_or_out_of_range_expect_status_is_refused() {
        for expect_status in [json!("200"), json!(99), json!(600), json!(200.5)] {
            let error = validate_config(
                GateKind::Http,
                &json!({"url": "https://ci.internal/x", "expect_status": expect_status}),
                60,
            )
            .expect_err("a status the evaluator cannot act on must not be stored");
            assert!(
                matches!(error, GateConfigError::InvalidExpectStatus),
                "{expect_status} should be refused, got {error}"
            );
        }

        validate_config(
            GateKind::Http,
            &json!({"url": "https://ci.internal/x", "expect_status": 200}),
            60,
        )
        .expect("a plain integer status is the ordinary case");
    }

    #[tokio::test]
    async fn a_status_gate_opens_only_on_the_expected_status() {
        let endpoint =
            start_endpoint("HTTP/1.1 200 OK", "", br#"{"status":"green"}"#.to_vec()).await;
        let client = gate_client_builder().build().expect("client");

        let satisfied =
            evaluate_http_with_client(&json!({"url": endpoint.url, "expect_status": 200}), &client)
                .await;
        assert_eq!(satisfied.result, GateResult::Satisfied);

        let waiting =
            evaluate_http_with_client(&json!({"url": endpoint.url, "expect_status": 201}), &client)
                .await;
        assert_eq!(waiting.result, GateResult::Pending);
        assert_eq!(waiting.detail, "status 200, expected 201");
    }

    #[test]
    fn userinfo_is_stripped_before_a_url_is_stored_or_reported() {
        assert_eq!(
            redact_url_userinfo("https://ci-bot:hunter2@ci.internal/builds/1"),
            "https://ci.internal/builds/1"
        );
        assert_eq!(
            redact_url_userinfo("https://ci.internal/builds/1"),
            "https://ci.internal/builds/1"
        );
    }

    /// The refusal message is shown to the gate's author; it still must not
    /// carry the credentials back out.
    #[test]
    fn a_refused_gate_url_is_reported_without_its_credentials() {
        let error = validate_config(
            GateKind::Http,
            &json!({"url": "http://ci-bot:hunter2@127.0.0.1/x", "expect_status": 200}),
            60,
        )
        .expect_err("a private address is refused");
        let message = error.to_string();
        assert!(!message.contains("hunter2"), "got: {message}");
        assert!(message.contains("127.0.0.1"), "got: {message}");
    }

    /// `last_detail` is stored and served. A failed request against a URL
    /// with userinfo must leave the password out of it.
    #[tokio::test]
    async fn a_request_failure_never_leaks_url_credentials_into_the_detail() {
        let client = gate_client_builder().build().expect("client");
        let evaluation = evaluate_http_with_client(
            &json!({"url": "http://ci-bot:hunter2@127.0.0.1:9/gate", "expect_status": 200}),
            &client,
        )
        .await;
        assert_eq!(evaluation.result, GateResult::Erroring);
        assert!(
            !evaluation.detail.contains("hunter2"),
            "the detail is stored and served: {}",
            evaluation.detail
        );
        assert!(
            evaluation.detail.contains("127.0.0.1"),
            "the detail still says where it tried: {}",
            evaluation.detail
        );
    }

    #[tokio::test]
    async fn a_body_past_the_cap_errors_instead_of_buffering() {
        let endpoint =
            start_endpoint("HTTP/1.1 200 OK", "", vec![b'a'; MAX_GATE_BODY_BYTES + 1]).await;
        let client = gate_client_builder().build().expect("client");

        let evaluation =
            evaluate_http_with_client(&json!({"url": endpoint.url, "pointer": "/status"}), &client)
                .await;

        assert_eq!(evaluation.result, GateResult::Erroring);
        assert!(
            evaluation.detail.contains("exceeded"),
            "{}",
            evaluation.detail
        );
    }

    /// A gate header naming a secret that does not resolve must fail closed:
    /// the literal `secret:…` string is not a credential and must never be
    /// sent as if it were one.
    #[tokio::test]
    async fn an_unresolvable_secret_header_errors_instead_of_sending_the_literal() {
        let mut endpoint =
            start_endpoint("HTTP/1.1 200 OK", "", br#"{"status":"green"}"#.to_vec()).await;
        let client = gate_client_builder().build().expect("client");

        let evaluation = evaluate_http_with_client(
            &json!({
                "url": endpoint.url,
                "pointer": "/status",
                "headers": {"authorization": "secret:NOT_IN_ANY_STORE_TEST_ONLY"},
            }),
            &client,
        )
        .await;

        assert_eq!(
            evaluation.result,
            GateResult::Erroring,
            "an unresolvable secret must not be sent as its own literal value"
        );
        assert!(
            endpoint.seen.try_recv().is_err(),
            "no request should leave until every header resolves"
        );
    }

    /// The resolver's other indirection mode, exercised end to end: the
    /// stored value is a whole-value reference, and what arrives at the
    /// endpoint is what the reference names.
    #[tokio::test]
    async fn an_env_referenced_header_arrives_resolved() {
        unsafe {
            std::env::set_var("SPACEBOT_GATE_TEST_TOKEN", "s3cret-token");
        }
        let mut endpoint =
            start_endpoint("HTTP/1.1 200 OK", "", br#"{"status":"green"}"#.to_vec()).await;
        let client = gate_client_builder().build().expect("client");

        let evaluation = evaluate_http_with_client(
            &json!({
                "url": endpoint.url,
                "pointer": "/status",
                "headers": {"authorization": "env:SPACEBOT_GATE_TEST_TOKEN"},
            }),
            &client,
        )
        .await;

        assert_eq!(
            evaluation.result,
            GateResult::Satisfied,
            "{}",
            evaluation.detail
        );
        let request = endpoint
            .seen
            .try_recv()
            .expect("the request should have arrived");
        assert!(
            request.contains("authorization: s3cret-token"),
            "the endpoint must see the resolved value, not the reference:\n{request}"
        );
    }

    /// Redirects are off: a redirect target is a second URL that never went
    /// through the address check, so the 3xx itself is the definitive answer.
    #[tokio::test]
    async fn a_redirect_is_a_definitive_answer_not_an_invitation() {
        let endpoint = start_endpoint(
            "HTTP/1.1 302 Found",
            "location: http://127.0.0.1:1/elsewhere\r\n",
            Vec::new(),
        )
        .await;
        let client = gate_client_builder().build().expect("client");

        let evaluation =
            evaluate_http_with_client(&json!({"url": endpoint.url, "expect_status": 200}), &client)
                .await;

        // Had the redirect been followed, the connection to port 1 would fail
        // and this would read Erroring. Pending says the 302 was the answer.
        assert_eq!(
            evaluation.result,
            GateResult::Pending,
            "{}",
            evaluation.detail
        );
        assert!(evaluation.detail.contains("302"), "{}", evaluation.detail);
    }
}
