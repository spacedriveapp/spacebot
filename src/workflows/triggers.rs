//! Triggers: something other than a person starting a run.
//!
//! Two of the three triggers need storage and both live here. A schedule is a
//! stored intent plus a cursor; a webhook is a stored secret plus a stored
//! payload mapping. The third — the `launch_workflow` tool — needs neither, and
//! is in `crate::tools::launch_workflow`.
//!
//! Neither of these executes a pipeline. Both of them call
//! [`WorkflowStore::launch_as`] with a [`LaunchIdentity`] whose `launched_by`
//! names the trigger rather than a person, and everything downstream of that —
//! the ready sweep, the claim, the failure budget, the run assessment — is the
//! machinery that was already there. A trigger is an identity and a reason to
//! call `launch`, and deliberately nothing else.
//!
//! ## The distinction both outcome enums exist to make
//!
//! "The schedule fired and the launch was refused" and "the schedule fired and
//! the run started" are not the same event, and neither is "the schedule fired
//! and the database was down". They recover differently — by editing a
//! template, by doing nothing, and by waiting — so they are three labels, and
//! the sweep acts differently on each. A single `success` boolean would have
//! made a schedule that needs a person indistinguishable from one that needs
//! ten seconds, which is the failure this codebase has repeatedly paid for.

use crate::error::Result;
use crate::tasks::TaskStore;
use crate::workflows::store::{LaunchError, LaunchIdentity, WorkflowStore};

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use sqlx::{Row as _, SqlitePool};

/// Timezone every workflow cron expression is read in.
///
/// Fixed, where a per-agent cron job takes its zone from its owning agent's
/// config. A workflow schedule has no owning agent: it is instance-level and
/// whichever agent's supervisor tick reaches it first is the one that fires it.
/// Taking the zone from the sweeper would make the same row fire at a different
/// hour depending on who noticed, which is worse than a fixed zone that is
/// occasionally inconvenient.
const SCHEDULE_TIMEZONE: chrono_tz::Tz = chrono_tz::UTC;

/// Header carrying a webhook's shared secret.
pub const WEBHOOK_SECRET_HEADER: &str = "x-spacebot-webhook-secret";

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// A schedule attached to a workflow, launching with a stored input.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowSchedule {
    pub id: String,
    pub workflow_id: String,
    pub name: String,
    /// 5-field cron expression, read in UTC. `None` uses `interval_secs`.
    pub cron_expr: Option<String>,
    pub interval_secs: i64,
    /// The launch payload. A literal, because a schedule cannot prompt.
    pub inputs: Value,
    /// Which agent owns and executes the emitted tasks.
    pub agent_id: String,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_fired_at: Option<String>,
    pub last_outcome: Option<ScheduleOutcome>,
    pub last_detail: Option<String>,
    pub last_run_id: Option<String>,
    pub created_at: String,
}

/// What came of one schedule fire.
///
/// Three values because there are three recoveries. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleOutcome {
    /// A run started. `last_run_id` names it. Nothing to do.
    Launched,
    /// The launch was validly rejected: the template contradicts itself, or the
    /// stored input does not match its schema. Deterministic — the next fire
    /// refuses identically — so this outcome **disables the schedule**, because
    /// a timer firing into the same error forever is a log nobody reads.
    Refused,
    /// The launch could not be attempted: storage failed. Transient, says
    /// nothing about the template, and the schedule stays on.
    Errored,
}

impl ScheduleOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ScheduleOutcome::Launched => "launched",
            ScheduleOutcome::Refused => "refused",
            ScheduleOutcome::Errored => "errored",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "launched" => Some(ScheduleOutcome::Launched),
            "refused" => Some(ScheduleOutcome::Refused),
            "errored" => Some(ScheduleOutcome::Errored),
            _ => None,
        }
    }

    /// Whether reaching this outcome should stop the schedule.
    ///
    /// Only a refusal. It is the one outcome that will repeat identically for
    /// as long as nobody edits anything, and the one whose recovery is a
    /// person. An error will very likely not repeat, and disabling on it would
    /// turn a thirty-second database blip into a silently dead schedule.
    pub fn should_disable(self) -> bool {
        matches!(self, ScheduleOutcome::Refused)
    }
}

impl std::fmt::Display for ScheduleOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// One schedule fire, as it happened.
#[derive(Debug, Clone)]
pub struct ScheduleFire {
    pub schedule_id: String,
    pub schedule_name: String,
    pub workflow_id: String,
    pub agent_id: String,
    pub outcome: ScheduleOutcome,
    pub detail: String,
    /// Set exactly when `outcome` is `launched`.
    pub run_id: Option<String>,
    /// Whether this fire switched the schedule off.
    pub disabled: bool,
}

/// An inbound endpoint mapping a payload to a run input.
///
/// Note what is *not* on this struct: the secret. It goes in as plaintext once,
/// is hashed immediately, and is never read back out — there is no field here
/// that could be serialised into a response by accident.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowWebhook {
    pub workflow_id: String,
    /// `{ "<run input key>": "<JSON Pointer into the payload>" }`.
    pub input_pointers: Map<String, Value>,
    pub agent_id: String,
    pub enabled: bool,
    pub last_delivery_at: Option<String>,
    pub last_outcome: Option<DeliveryOutcome>,
    pub last_detail: Option<String>,
    pub last_run_id: Option<String>,
    pub created_at: String,
}

/// What came of one accepted webhook delivery.
///
/// Only ever written for a delivery that authenticated. A rejected delivery
/// writes nothing at all — see [`WorkflowTriggerStore::authenticate_webhook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// A run started; `last_run_id` names it.
    Launched,
    /// A configured pointer found nothing in the payload. Fix the pointer, or
    /// the sender. Distinct from `refused` because the thing to change is the
    /// webhook's mapping rather than the pipeline.
    Unmapped,
    /// The mapped input reached `launch` and was rejected. Fix the template.
    Refused,
    /// Storage failed. Fix nothing; try again.
    Errored,
}

impl DeliveryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryOutcome::Launched => "launched",
            DeliveryOutcome::Unmapped => "unmapped",
            DeliveryOutcome::Refused => "refused",
            DeliveryOutcome::Errored => "errored",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "launched" => Some(DeliveryOutcome::Launched),
            "unmapped" => Some(DeliveryOutcome::Unmapped),
            "refused" => Some(DeliveryOutcome::Refused),
            "errored" => Some(DeliveryOutcome::Errored),
            _ => None,
        }
    }
}

impl std::fmt::Display for DeliveryOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Why a webhook delivery was turned away at the door.
///
/// **Every variant renders to the caller identically.** The distinction is for
/// the operator reading the log, and it must not reach the wire: telling an
/// unauthenticated stranger that a workflow exists but its webhook is switched
/// off, or that the secret was wrong rather than missing, hands them a
/// narrowing oracle for free. What they need to know is "no", and that is all
/// any of these produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookRejection {
    /// This workflow has no webhook configured. The default, and the reason the
    /// default posture is off: an endpoint with no row behind it does nothing.
    NotConfigured,
    /// Configured, and switched off.
    Disabled,
    /// Configured and on, and the presented secret did not match — including
    /// the case where none was presented at all.
    BadSecret,
}

impl WebhookRejection {
    /// What the operator sees. Never sent to the caller.
    pub fn operator_detail(self) -> &'static str {
        match self {
            WebhookRejection::NotConfigured => {
                "no webhook is configured for this workflow — configure one to enable it"
            }
            WebhookRejection::Disabled => {
                "a webhook is configured for this workflow but is disabled — enable it to accept \
                 deliveries"
            }
            WebhookRejection::BadSecret => {
                "the delivery presented no valid shared secret for this workflow's webhook"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// Hash a shared secret for storage. Never store or log the secret itself.
pub fn hash_webhook_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Whether a presented secret matches a stored hash, in constant time.
///
/// Both sides are reduced to a 32-byte digest before anything is compared, and
/// the comparison then accumulates differences across all 32 bytes rather than
/// returning at the first one. Digesting first is not ceremony: it is what
/// removes the length side-channel, because a naive comparison of the secrets
/// themselves leaks how many leading bytes were right through how long it took
/// to say no, and leaks the secret's length through the same channel.
///
/// A stored hash that is not 32 bytes of hex — a truncated row, a hand-edited
/// one — is treated as a digest no input can produce, so a malformed row fails
/// closed rather than matching something.
pub fn webhook_secret_matches(presented: &str, stored_hash: &str) -> bool {
    let presented_digest = Sha256::digest(presented.as_bytes());

    let mut stored_digest = [0u8; 32];
    let decoded = hex::decode(stored_hash).unwrap_or_default();
    if decoded.len() == 32 {
        stored_digest.copy_from_slice(&decoded);
    } else {
        // Deliberately leaves the all-zero digest in place: no input hashes to
        // it, so the comparison below still runs its full length and still
        // says no. Returning early here would reintroduce the timing
        // distinction the digest was there to remove.
        stored_digest = [0u8; 32];
    }

    let mut difference = 0u8;
    for index in 0..32 {
        difference |= presented_digest[index] ^ stored_digest[index];
    }
    difference == 0
}

// ---------------------------------------------------------------------------
// Payload mapping
// ---------------------------------------------------------------------------

/// A pointer that selected nothing in the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedInput {
    pub input_key: String,
    pub pointer: String,
}

/// Build a run input from a payload and a pointer map.
///
/// RFC 6901 pointers, the same vocabulary bindings and gates already use — an
/// empty pointer means the whole document, exactly as it does in a run-input
/// binding.
///
/// Every pointer must resolve. A missing one is an error rather than an omitted
/// key, because the alternative is launching a pipeline with a hole in its
/// input and discovering it three steps later as an unresolvable contract —
/// which is precisely the failure `launch` validates the run input up front to
/// avoid.
pub fn map_payload(
    pointers: &Map<String, Value>,
    payload: &Value,
) -> std::result::Result<Value, Vec<UnmappedInput>> {
    let mut mapped = Map::new();
    let mut unmapped = Vec::new();

    for (input_key, pointer) in pointers {
        let pointer = pointer.as_str().unwrap_or_default();
        let value = if pointer.is_empty() {
            Some(payload)
        } else {
            payload.pointer(pointer)
        };

        match value {
            Some(value) => {
                mapped.insert(input_key.clone(), value.clone());
            }
            None => unmapped.push(UnmappedInput {
                input_key: input_key.clone(),
                pointer: pointer.to_string(),
            }),
        }
    }

    if unmapped.is_empty() {
        Ok(Value::Object(mapped))
    } else {
        Err(unmapped)
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct WorkflowTriggerStore {
    pool: SqlitePool,
}

const SCHEDULE_COLUMNS: &str = "SELECT id, workflow_id, name, cron_expr, interval_secs, inputs, \
     agent_id, enabled, next_run_at, last_fired_at, last_outcome, last_detail, last_run_id, \
     created_at";

const WEBHOOK_COLUMNS: &str = "SELECT workflow_id, input_pointers, agent_id, enabled, \
     last_delivery_at, last_outcome, last_detail, last_run_id, created_at";

impl WorkflowTriggerStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // -- Schedules ----------------------------------------------------------

    /// Create or replace a schedule.
    ///
    /// The cursor is deliberately not written here. A save that set
    /// `next_run_at` would have to decide what "now" means for a schedule
    /// nobody has swept yet, and the sweep already answers that on first sight;
    /// leaving it NULL is how a newly saved schedule says "work out when I am
    /// next due". Editing the timing clears it for the same reason: a cursor
    /// computed from the old expression is a fire at the old time.
    pub async fn put_schedule(&self, schedule: &WorkflowSchedule) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_schedules \
                 (id, workflow_id, name, cron_expr, interval_secs, inputs, agent_id, enabled) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                 workflow_id = excluded.workflow_id, \
                 name = excluded.name, \
                 cron_expr = excluded.cron_expr, \
                 interval_secs = excluded.interval_secs, \
                 inputs = excluded.inputs, \
                 agent_id = excluded.agent_id, \
                 enabled = excluded.enabled, \
                 next_run_at = CASE \
                     WHEN NOT (cron_expr IS excluded.cron_expr) \
                          OR interval_secs != excluded.interval_secs \
                     THEN NULL \
                     ELSE next_run_at \
                 END",
        )
        .bind(&schedule.id)
        .bind(&schedule.workflow_id)
        .bind(&schedule.name)
        .bind(schedule.cron_expr.as_deref())
        .bind(schedule.interval_secs.max(1))
        .bind(schedule.inputs.to_string())
        .bind(&schedule.agent_id)
        .bind(i64::from(schedule.enabled))
        .execute(&self.pool)
        .await
        .context("failed to save a workflow schedule")?;

        Ok(())
    }

    pub async fn get_schedule(&self, id: &str) -> Result<Option<WorkflowSchedule>> {
        let row = sqlx::query(&format!(
            "{SCHEDULE_COLUMNS} FROM workflow_schedules WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch a workflow schedule")?;

        row.map(schedule_from_row).transpose()
    }

    pub async fn list_schedules(&self, workflow_id: &str) -> Result<Vec<WorkflowSchedule>> {
        let rows = sqlx::query(&format!(
            "{SCHEDULE_COLUMNS} FROM workflow_schedules WHERE workflow_id = ? \
             ORDER BY created_at ASC"
        ))
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow schedules")?;

        rows.into_iter().map(schedule_from_row).collect()
    }

    pub async fn delete_schedule(&self, id: &str) -> Result<bool> {
        let deleted = sqlx::query("DELETE FROM workflow_schedules WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete a workflow schedule")?;

        Ok(deleted.rows_affected() > 0)
    }

    /// Enabled schedules, cursor-first. The sweep's only read.
    async fn live_schedules(&self) -> Result<Vec<WorkflowSchedule>> {
        let rows = sqlx::query(&format!(
            "{SCHEDULE_COLUMNS} FROM workflow_schedules WHERE enabled = 1 \
             ORDER BY created_at ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .context("failed to list live workflow schedules")?;

        rows.into_iter().map(schedule_from_row).collect()
    }

    /// Set a schedule's cursor if it does not have one yet.
    ///
    /// `false` means somebody else set it first, which is not an error — it is
    /// two supervisor ticks noticing the same new schedule, and the loser reads
    /// the winner's value on its next pass.
    pub async fn initialize_next_run_at(&self, id: &str, next_run_at: &str) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE workflow_schedules SET next_run_at = ? WHERE id = ? AND next_run_at IS NULL",
        )
        .bind(next_run_at)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to initialize a workflow schedule cursor")?;

        Ok(updated.rows_affected() > 0)
    }

    /// Claim one fire by advancing the cursor past it.
    ///
    /// `false` means another process claimed this fire, and the caller **must
    /// not launch**. This conditional UPDATE is the entire once-only story: the
    /// supervisor tick that sweeps schedules runs in every agent, so several
    /// processes seeing the same due schedule on the same second is the normal
    /// case rather than a race to be avoided. The `next_run_at = ?` guard means
    /// exactly one of them advances it, and it is the same latch
    /// `claim_next_ready` uses to hand one task to one worker and `settle_run`
    /// uses to send one notification.
    pub async fn claim_fire(
        &self,
        id: &str,
        expected_next_run_at: &str,
        next_run_at: &str,
    ) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE workflow_schedules SET next_run_at = ? \
             WHERE id = ? AND enabled = 1 AND next_run_at = ?",
        )
        .bind(next_run_at)
        .bind(id)
        .bind(expected_next_run_at)
        .execute(&self.pool)
        .await
        .context("failed to claim a workflow schedule fire")?;

        Ok(updated.rows_affected() > 0)
    }

    /// Record what a fire came to, and switch the schedule off if that outcome
    /// says it should.
    pub async fn record_fire(
        &self,
        id: &str,
        outcome: ScheduleOutcome,
        detail: &str,
        run_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE workflow_schedules SET \
                 last_fired_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), \
                 last_outcome = ?, last_detail = ?, last_run_id = COALESCE(?, last_run_id), \
                 enabled = CASE WHEN ? = 1 THEN 0 ELSE enabled END \
             WHERE id = ?",
        )
        .bind(outcome.as_str())
        .bind(detail)
        .bind(run_id)
        .bind(i64::from(outcome.should_disable()))
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to record a workflow schedule fire")?;

        Ok(())
    }

    /// Fire every schedule that is due, and say what came of each.
    ///
    /// Shaped like `sweep_runs`, and for the same reasons: one question per
    /// row, only transitions written, and one schedule that cannot be handled
    /// does not abandon the pass over the others — a storage failure firing the
    /// 03:00 job must not stop the 03:05 one from ever being looked at.
    pub async fn sweep_schedules(
        &self,
        workflows: &WorkflowStore,
        task_store: &TaskStore,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScheduleFire>> {
        let schedules = self.live_schedules().await?;
        let mut fires = Vec::new();

        for schedule in schedules {
            let Some(cursor) = self.cursor_for(&schedule, now).await else {
                continue;
            };

            if cursor > now {
                continue;
            }

            // Where the next fire goes, computed before this one is claimed:
            // the claim *is* the advance, so there is nothing to write without
            // it. A schedule whose expression stopped producing future fires
            // has no cursor to advance to and is left alone rather than being
            // fired repeatedly against a stuck cursor.
            let Some(following) = self.following_fire(&schedule, now) else {
                tracing::warn!(
                    schedule_id = %schedule.id,
                    cron_expr = ?schedule.cron_expr,
                    "workflow schedule has no future fire; leaving its cursor alone"
                );
                continue;
            };

            let claimed = self
                .claim_fire(
                    &schedule.id,
                    &format_timestamp(cursor),
                    &format_timestamp(following),
                )
                .await?;
            if !claimed {
                continue;
            }

            fires.push(self.fire(workflows, task_store, &schedule).await?);
        }

        Ok(fires)
    }

    /// One schedule's due time, initialising it on first sight.
    async fn cursor_for(
        &self,
        schedule: &WorkflowSchedule,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        if let Some(cursor) = schedule.next_run_at.as_deref().and_then(parse_timestamp) {
            return Some(cursor);
        }

        let first = self.following_fire(schedule, now)?;
        match self
            .initialize_next_run_at(&schedule.id, &format_timestamp(first))
            .await
        {
            // Lost the initialisation race. The winner's cursor is authoritative
            // and this pass simply has nothing to do with this schedule.
            Ok(false) => None,
            Ok(true) => Some(first),
            Err(error) => {
                tracing::warn!(
                    %error,
                    schedule_id = %schedule.id,
                    "failed to initialize a workflow schedule cursor"
                );
                None
            }
        }
    }

    fn following_fire(
        &self,
        schedule: &WorkflowSchedule,
        after: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        match schedule.cron_expr.as_deref() {
            Some(expr) => crate::cron::scheduler::next_cron_fire(expr, after, SCHEDULE_TIMEZONE),
            None => Some(after + chrono::Duration::seconds(schedule.interval_secs.max(1))),
        }
    }

    /// Launch one claimed fire and record what happened.
    async fn fire(
        &self,
        workflows: &WorkflowStore,
        task_store: &TaskStore,
        schedule: &WorkflowSchedule,
    ) -> Result<ScheduleFire> {
        let identity = LaunchIdentity::triggered_by(
            schedule.agent_id.clone(),
            schedule_launcher_id(&schedule.id),
        );

        let (outcome, detail, run_id) = match workflows
            .launch_as(
                task_store,
                &schedule.workflow_id,
                &schedule.inputs,
                &identity,
            )
            .await
        {
            Ok(launched) => {
                // Same reason the HTTP handler does this: every emitted task
                // starts in backlog, and without a sweep the entry step waits
                // for the next tick for no reason anybody could see.
                if let Err(error) = task_store.recompute_ready(&schedule.agent_id).await {
                    tracing::warn!(
                        %error,
                        run_id = %launched.run.id,
                        "scheduled launch succeeded but the ready sweep failed; \
                         the next tick will pick it up"
                    );
                }
                (
                    ScheduleOutcome::Launched,
                    format!(
                        "launched run {} with {} task(s)",
                        launched.run.id,
                        launched.task_numbers.len()
                    ),
                    Some(launched.run.id),
                )
            }
            // The split that matters. A storage failure says nothing about the
            // template and will very likely not repeat; every other variant is
            // a statement about the template or the stored input, is therefore
            // exactly as true on the next fire, and needs a person.
            Err(LaunchError::Storage(error)) => (
                ScheduleOutcome::Errored,
                format!("the launch could not be attempted: {error}"),
                None,
            ),
            Err(error) => (ScheduleOutcome::Refused, error.to_string(), None),
        };

        self.record_fire(&schedule.id, outcome, &detail, run_id.as_deref())
            .await?;

        Ok(ScheduleFire {
            schedule_id: schedule.id.clone(),
            schedule_name: schedule.name.clone(),
            workflow_id: schedule.workflow_id.clone(),
            agent_id: schedule.agent_id.clone(),
            outcome,
            detail,
            run_id,
            disabled: outcome.should_disable(),
        })
    }

    // -- Webhooks -----------------------------------------------------------

    /// Create or replace a workflow's webhook.
    ///
    /// `secret` is plaintext on the way in and is hashed before it touches the
    /// database. There is no path that stores or returns it.
    pub async fn put_webhook(
        &self,
        workflow_id: &str,
        secret: &str,
        input_pointers: &Map<String, Value>,
        agent_id: &str,
        enabled: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_webhooks \
                 (workflow_id, secret_hash, input_pointers, agent_id, enabled) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(workflow_id) DO UPDATE SET \
                 secret_hash = excluded.secret_hash, \
                 input_pointers = excluded.input_pointers, \
                 agent_id = excluded.agent_id, \
                 enabled = excluded.enabled",
        )
        .bind(workflow_id)
        .bind(hash_webhook_secret(secret))
        .bind(Value::Object(input_pointers.clone()).to_string())
        .bind(agent_id)
        .bind(i64::from(enabled))
        .execute(&self.pool)
        .await
        .context("failed to save a workflow webhook")?;

        Ok(())
    }

    pub async fn get_webhook(&self, workflow_id: &str) -> Result<Option<WorkflowWebhook>> {
        let row = sqlx::query(&format!(
            "{WEBHOOK_COLUMNS} FROM workflow_webhooks WHERE workflow_id = ?"
        ))
        .bind(workflow_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch a workflow webhook")?;

        row.map(webhook_from_row).transpose()
    }

    pub async fn delete_webhook(&self, workflow_id: &str) -> Result<bool> {
        let deleted = sqlx::query("DELETE FROM workflow_webhooks WHERE workflow_id = ?")
            .bind(workflow_id)
            .execute(&self.pool)
            .await
            .context("failed to delete a workflow webhook")?;

        Ok(deleted.rows_affected() > 0)
    }

    /// The gate every delivery passes through before any work happens.
    ///
    /// Three ways to be turned away and one way in, and the three refusals are
    /// one answer on the wire. `Ok(Err(_))` is a refusal the caller renders
    /// identically whatever it contains; `Err(_)` is our storage failing, which
    /// is also a refusal but not the caller's fault.
    ///
    /// Nothing here writes. An unauthenticated endpoint that records its own
    /// rejections is a free write amplification for anyone who finds the URL.
    pub async fn authenticate_webhook(
        &self,
        workflow_id: &str,
        presented_secret: Option<&str>,
    ) -> Result<std::result::Result<WorkflowWebhook, WebhookRejection>> {
        let row = sqlx::query(&format!(
            "{WEBHOOK_COLUMNS}, secret_hash FROM workflow_webhooks WHERE workflow_id = ?"
        ))
        .bind(workflow_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch a workflow webhook")?;

        let Some(row) = row else {
            return Ok(Err(WebhookRejection::NotConfigured));
        };

        let secret_hash: String = row
            .try_get("secret_hash")
            .context("failed to read a webhook secret hash")?;
        let webhook = webhook_from_row(row)?;

        if !webhook.enabled {
            return Ok(Err(WebhookRejection::Disabled));
        }

        // An absent header is compared as an empty secret rather than
        // short-circuited, so "no secret" and "wrong secret" take the same path
        // through the same comparison. They are the same answer to the caller
        // and there is no reason to make them different amounts of work.
        if !webhook_secret_matches(presented_secret.unwrap_or_default(), &secret_hash) {
            return Ok(Err(WebhookRejection::BadSecret));
        }

        Ok(Ok(webhook))
    }

    /// Record what came of an *authenticated* delivery.
    pub async fn record_delivery(
        &self,
        workflow_id: &str,
        outcome: DeliveryOutcome,
        detail: &str,
        run_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE workflow_webhooks SET \
                 last_delivery_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), \
                 last_outcome = ?, last_detail = ?, last_run_id = COALESCE(?, last_run_id) \
             WHERE workflow_id = ?",
        )
        .bind(outcome.as_str())
        .bind(detail)
        .bind(run_id)
        .bind(workflow_id)
        .execute(&self.pool)
        .await
        .context("failed to record a workflow webhook delivery")?;

        Ok(())
    }

    /// Map an authenticated payload and launch, recording the outcome.
    ///
    /// The whole of what a delivery does after it is let in, in one place, so
    /// the HTTP handler is transport and this is policy.
    pub async fn deliver(
        &self,
        workflows: &WorkflowStore,
        task_store: &TaskStore,
        webhook: &WorkflowWebhook,
        payload: &Value,
    ) -> Result<std::result::Result<crate::workflows::InstantiatedRun, (DeliveryOutcome, String)>>
    {
        let inputs = match map_payload(&webhook.input_pointers, payload) {
            Ok(inputs) => inputs,
            Err(unmapped) => {
                let detail = format!(
                    "the payload does not contain {}",
                    unmapped
                        .iter()
                        .map(|item| format!("`{}` at `{}`", item.input_key, item.pointer))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                self.record_delivery(
                    &webhook.workflow_id,
                    DeliveryOutcome::Unmapped,
                    &detail,
                    None,
                )
                .await?;
                return Ok(Err((DeliveryOutcome::Unmapped, detail)));
            }
        };

        let identity = LaunchIdentity::triggered_by(
            webhook.agent_id.clone(),
            webhook_launcher_id(&webhook.workflow_id),
        );

        match workflows
            .launch_as(task_store, &webhook.workflow_id, &inputs, &identity)
            .await
        {
            Ok(launched) => {
                if let Err(error) = task_store.recompute_ready(&webhook.agent_id).await {
                    tracing::warn!(
                        %error,
                        run_id = %launched.run.id,
                        "webhook launch succeeded but the ready sweep failed; \
                         the next tick will pick it up"
                    );
                }
                self.record_delivery(
                    &webhook.workflow_id,
                    DeliveryOutcome::Launched,
                    &format!("launched run {}", launched.run.id),
                    Some(&launched.run.id),
                )
                .await?;
                Ok(Ok(launched))
            }
            Err(LaunchError::Storage(error)) => {
                let detail = format!("the launch could not be attempted: {error}");
                self.record_delivery(
                    &webhook.workflow_id,
                    DeliveryOutcome::Errored,
                    &detail,
                    None,
                )
                .await?;
                Ok(Err((DeliveryOutcome::Errored, detail)))
            }
            Err(error) => {
                let detail = error.to_string();
                self.record_delivery(
                    &webhook.workflow_id,
                    DeliveryOutcome::Refused,
                    &detail,
                    None,
                )
                .await?;
                Ok(Err((DeliveryOutcome::Refused, detail)))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Launch identities
// ---------------------------------------------------------------------------

/// `launched_by` for a run a schedule started.
///
/// Prefixed and parseable for the same reason `task:<n>` is: a run's origin has
/// to be readable back off the row, and a bare id would be indistinguishable
/// from an agent name.
pub fn schedule_launcher_id(schedule_id: &str) -> String {
    format!("schedule:{schedule_id}")
}

/// `launched_by` for a run a webhook started.
pub fn webhook_launcher_id(workflow_id: &str) -> String {
    format!("webhook:{workflow_id}")
}

// ---------------------------------------------------------------------------
// Row decoding
// ---------------------------------------------------------------------------

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.to_utc())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|naive| naive.and_utc())
        })
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Read a nullable TEXT column, treating NULL and empty as absent.
///
/// `try_get::<String, _>` on a SQLite NULL hands back an empty string rather
/// than failing, so the tempting `row.try_get(col).ok()` yields `Some("")` for
/// a column nobody set — which then reads as "there is a cron expression, and
/// it is the empty one". Every nullable column here goes through this instead.
/// Empty is folded into `None` for the same reason: a form that posts a blank
/// field means "unset", and storing the difference would be a third state.
fn optional_text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn schedule_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkflowSchedule> {
    let inputs: String = row
        .try_get("inputs")
        .context("failed to read a schedule's inputs")?;

    Ok(WorkflowSchedule {
        id: row.try_get("id").context("failed to read a schedule id")?,
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read a schedule workflow_id")?,
        name: row
            .try_get("name")
            .context("failed to read a schedule name")?,
        cron_expr: optional_text(&row, "cron_expr"),
        interval_secs: row
            .try_get("interval_secs")
            .context("failed to read a schedule interval")?,
        inputs: serde_json::from_str(&inputs).unwrap_or_else(|_| Value::Object(Map::new())),
        agent_id: row
            .try_get("agent_id")
            .context("failed to read a schedule agent_id")?,
        enabled: row
            .try_get::<i64, _>("enabled")
            .context("failed to read a schedule enabled flag")?
            != 0,
        next_run_at: optional_text(&row, "next_run_at"),
        last_fired_at: optional_text(&row, "last_fired_at"),
        last_outcome: optional_text(&row, "last_outcome")
            .as_deref()
            .and_then(ScheduleOutcome::parse),
        last_detail: optional_text(&row, "last_detail"),
        last_run_id: optional_text(&row, "last_run_id"),
        created_at: row
            .try_get("created_at")
            .context("failed to read a schedule created_at")?,
    })
}

fn webhook_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkflowWebhook> {
    let pointers: String = row
        .try_get("input_pointers")
        .context("failed to read a webhook's input pointers")?;

    Ok(WorkflowWebhook {
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read a webhook workflow_id")?,
        input_pointers: serde_json::from_str::<Value>(&pointers)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default(),
        agent_id: row
            .try_get("agent_id")
            .context("failed to read a webhook agent_id")?,
        enabled: row
            .try_get::<i64, _>("enabled")
            .context("failed to read a webhook enabled flag")?
            != 0,
        last_delivery_at: optional_text(&row, "last_delivery_at"),
        last_outcome: optional_text(&row, "last_outcome")
            .as_deref()
            .and_then(DeliveryOutcome::parse),
        last_detail: optional_text(&row, "last_detail"),
        last_run_id: optional_text(&row, "last_run_id"),
        created_at: row
            .try_get("created_at")
            .context("failed to read a webhook created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::workflows::{BindingSource, StepBinding, WorkflowStep};
    use sqlx::sqlite::SqlitePoolOptions;

    /// A live `deploy` workflow with one step reading `/tag` from the run
    /// input, plus the three stores that act on it.
    async fn fixture() -> (WorkflowStore, TaskStore, WorkflowTriggerStore, String) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::tasks::store::create_task_schema(&pool).await;
        crate::workflows::store::create_workflow_schema(&pool).await;
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("seed sequence");

        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool.clone());
        let triggers = WorkflowTriggerStore::new(pool);

        let workflow = workflows
            .create_workflow(
                "deploy",
                None,
                Some(&serde_json::json!({
                    "type": "object",
                    "required": ["tag"],
                    "properties": {"tag": {"type": "string"}},
                })),
            )
            .await
            .expect("create workflow");

        workflows
            .put_step(&WorkflowStep {
                workflow_id: workflow.id.clone(),
                step_key: "build".into(),
                title: "build".into(),
                description: None,
                assigned_agent_id: None,
                required_capabilities: None,
                priority: crate::tasks::TaskPriority::Medium,
                input_schema: None,
                output_schema: None,
                system_prompt: None,
                repo_id: None,
                position: 0,
                for_each_step_key: None,
                for_each_pointer: None,
                for_each_key: None,
                loop_group: None,
                loop_max_iterations: None,
                loop_until: None,
                kind: crate::workflows::StepKind::Agent,
                command: None,
                command_timeout_secs: None,
                expect_exit_code: None,
                worktree_mode: crate::workflows::WorktreeMode::Inherit,
                worktree_base_ref: None,
            })
            .await
            .expect("put step");
        workflows
            .put_binding(&StepBinding {
                workflow_id: workflow.id.clone(),
                step_key: "build".into(),
                input_key: "tag".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind run input");

        (workflows, tasks, triggers, workflow.id)
    }

    fn schedule(workflow_id: &str, inputs: serde_json::Value) -> WorkflowSchedule {
        WorkflowSchedule {
            id: "nightly".into(),
            workflow_id: workflow_id.to_string(),
            name: "nightly deploy".into(),
            cron_expr: None,
            // One second, so the schedule is due almost immediately and the
            // test does not have to reach into the cursor by hand.
            interval_secs: 1,
            inputs,
            agent_id: "agent-1".into(),
            enabled: true,
            next_run_at: None,
            last_fired_at: None,
            last_outcome: None,
            last_detail: None,
            last_run_id: None,
            created_at: String::new(),
        }
    }

    /// The capability: a schedule launches the pipeline with the payload it was
    /// written with. The input is a stored literal because a schedule cannot
    /// prompt — there is nobody to ask at 03:00 — so if the literal does not
    /// reach the run's bindings the trigger is decorative.
    #[tokio::test]
    async fn a_cron_trigger_fires_a_launch_with_its_stored_input() {
        let (workflows, tasks, triggers, workflow_id) = fixture().await;
        triggers
            .put_schedule(&schedule(
                &workflow_id,
                serde_json::json!({"tag": "v9.9.9"}),
            ))
            .await
            .expect("save schedule");

        // The first sweep initialises the cursor; it is not due until a period
        // has passed, which is what stops a newly saved schedule firing the
        // instant it is written.
        let first = triggers
            .sweep_schedules(&workflows, &tasks, Utc::now())
            .await
            .expect("first sweep");
        assert!(
            first.is_empty(),
            "a schedule fires on its period, not on being noticed"
        );

        let fires = triggers
            .sweep_schedules(
                &workflows,
                &tasks,
                Utc::now() + chrono::Duration::seconds(5),
            )
            .await
            .expect("second sweep");

        assert_eq!(fires.len(), 1);
        let fire = &fires[0];
        assert_eq!(fire.outcome, ScheduleOutcome::Launched);
        assert!(!fire.disabled);

        let run_id = fire.run_id.clone().expect("a launched fire names its run");
        let run = workflows
            .get_run(&run_id)
            .await
            .expect("fetch run")
            .expect("run exists");
        assert_eq!(run.inputs, serde_json::json!({"tag": "v9.9.9"}));
        assert_eq!(
            run.launched_by,
            schedule_launcher_id("nightly"),
            "the launch identity has to name the schedule, or a run has no origin"
        );

        // The stored literal reached the step's input, not just the run row.
        let emitted = tasks
            .list_by_workflow_run(&run_id)
            .await
            .expect("list run tasks");
        assert_eq!(emitted.len(), 1);
        let bindings = tasks
            .list_input_bindings(emitted[0].task_number)
            .await
            .expect("bindings");
        assert_eq!(
            bindings[0].literal_value,
            Some(serde_json::json!("v9.9.9")),
            "the schedule's stored input must be frozen onto the emitted task"
        );
    }

    /// "The schedule fired and the launch was refused" and "the schedule fired
    /// and the run started" are not the same event. A refusal is deterministic
    /// — the same template and the same stored input refuse identically forever
    /// — so it stops the schedule and says why, rather than firing into the
    /// same error every period until somebody reads a log nobody reads.
    #[tokio::test]
    async fn a_refused_fire_switches_the_schedule_off_and_records_why() {
        let (workflows, tasks, triggers, workflow_id) = fixture().await;
        // An input the workflow's schema rejects: no amount of retrying fixes
        // it, because nothing about it will be different next time.
        triggers
            .put_schedule(&schedule(&workflow_id, serde_json::json!({"tag": 7})))
            .await
            .expect("save schedule");

        triggers
            .sweep_schedules(&workflows, &tasks, Utc::now())
            .await
            .expect("cursor init");
        let fires = triggers
            .sweep_schedules(
                &workflows,
                &tasks,
                Utc::now() + chrono::Duration::seconds(5),
            )
            .await
            .expect("sweep");

        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].outcome, ScheduleOutcome::Refused);
        assert!(fires[0].disabled, "a refusal must stop the schedule");
        assert!(
            fires[0].detail.contains("schema"),
            "the recorded reason must name what to change: {}",
            fires[0].detail
        );

        let stored = triggers
            .get_schedule("nightly")
            .await
            .expect("fetch")
            .expect("exists");
        assert!(!stored.enabled);
        assert_eq!(stored.last_outcome, Some(ScheduleOutcome::Refused));
        assert!(stored.last_run_id.is_none(), "no run was started");

        // And it stays off: a disabled schedule is not swept again, so the same
        // refusal does not repeat every period.
        let again = triggers
            .sweep_schedules(
                &workflows,
                &tasks,
                Utc::now() + chrono::Duration::seconds(60),
            )
            .await
            .expect("sweep again");
        assert!(again.is_empty());
    }

    /// The other half of that split, and the reason it is three outcomes rather
    /// than a boolean: a storage failure says nothing about the template, will
    /// very likely not repeat, and must not disable anything. Treating it as a
    /// refusal would turn a database blip into a silently dead schedule.
    #[test]
    fn only_a_refusal_disables_a_schedule() {
        assert!(ScheduleOutcome::Refused.should_disable());
        assert!(!ScheduleOutcome::Errored.should_disable());
        assert!(!ScheduleOutcome::Launched.should_disable());
    }

    /// Two supervisor ticks seeing the same due schedule is the normal case,
    /// not a race to be avoided — every agent sweeps every schedule. The
    /// conditional cursor advance is what makes one fire produce one run, and
    /// without it a nightly deploy runs once per agent on the instance.
    #[tokio::test]
    async fn two_sweeps_of_the_same_due_schedule_launch_one_run() {
        let (workflows, tasks, triggers, workflow_id) = fixture().await;
        triggers
            .put_schedule(&schedule(&workflow_id, serde_json::json!({"tag": "v1"})))
            .await
            .expect("save schedule");

        triggers
            .sweep_schedules(&workflows, &tasks, Utc::now())
            .await
            .expect("cursor init");

        let due = Utc::now() + chrono::Duration::seconds(5);
        let first = triggers
            .sweep_schedules(&workflows, &tasks, due)
            .await
            .expect("first");
        let second = triggers
            .sweep_schedules(&workflows, &tasks, due)
            .await
            .expect("second");

        assert_eq!(first.len(), 1, "the first sweep claims the fire");
        assert!(
            second.is_empty(),
            "the second sweep must find the cursor already advanced"
        );
        assert_eq!(
            workflows
                .list_runs(&workflow_id)
                .await
                .expect("list runs")
                .len(),
            1
        );
    }

    // -- Webhook ------------------------------------------------------------

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    async fn configure_webhook(triggers: &WorkflowTriggerStore, workflow_id: &str, enabled: bool) {
        let mut pointers = Map::new();
        pointers.insert("tag".into(), Value::from("/head_commit/id"));
        triggers
            .put_webhook(workflow_id, SECRET, &pointers, "agent-1", enabled)
            .await
            .expect("configure webhook");
    }

    /// The default posture, and the whole reason this shipped before instance
    /// authentication exists. A workflow with no webhook row accepts nothing —
    /// not because a check remembered to run, but because there is nothing to
    /// authenticate against. Every workflow that has ever existed is in this
    /// state until somebody deliberately leaves it.
    #[tokio::test]
    async fn a_webhook_with_no_configured_secret_is_refused() {
        let (_, _, triggers, workflow_id) = fixture().await;

        let outcome = triggers
            .authenticate_webhook(&workflow_id, Some(SECRET))
            .await
            .expect("authenticate");

        assert_eq!(
            outcome.err(),
            Some(WebhookRejection::NotConfigured),
            "an unconfigured workflow must refuse even a plausible secret"
        );
    }

    /// Configured is not enabled. An operator setting a webhook up to look at
    /// it must not thereby be exposing the pipeline, or "I configured it" and
    /// "I want strangers running this" become the same action.
    #[tokio::test]
    async fn a_configured_but_disabled_webhook_is_refused_even_with_the_right_secret() {
        let (_, _, triggers, workflow_id) = fixture().await;
        configure_webhook(&triggers, &workflow_id, false).await;

        let outcome = triggers
            .authenticate_webhook(&workflow_id, Some(SECRET))
            .await
            .expect("authenticate");

        assert_eq!(outcome.err(), Some(WebhookRejection::Disabled));
    }

    /// A wrong secret and an absent one are the same refusal.
    ///
    /// Not merely the same status code — the same *variant*, so they travel
    /// through one code path to one renderer and there is no branch where they
    /// could come to differ. If "wrong" and "absent" were distinguishable, an
    /// attacker would learn from a probe with no header whether a workflow has
    /// a live webhook at all, which turns guessing the secret into a target
    /// worth guessing at.
    #[tokio::test]
    async fn a_wrong_secret_and_an_absent_one_are_the_same_refusal() {
        let (_, _, triggers, workflow_id) = fixture().await;
        configure_webhook(&triggers, &workflow_id, true).await;

        let wrong = triggers
            .authenticate_webhook(&workflow_id, Some("ffffffffffffffffffffffffffffffff"))
            .await
            .expect("authenticate");
        let absent = triggers
            .authenticate_webhook(&workflow_id, None)
            .await
            .expect("authenticate");
        let empty = triggers
            .authenticate_webhook(&workflow_id, Some(""))
            .await
            .expect("authenticate");

        assert_eq!(wrong.err(), Some(WebhookRejection::BadSecret));
        assert_eq!(absent.err(), Some(WebhookRejection::BadSecret));
        assert_eq!(empty.err(), Some(WebhookRejection::BadSecret));
    }

    /// A secret that is one character off is as wrong as one that shares
    /// nothing, and takes the same shape of work to reject. The digest is what
    /// buys that: comparing the secrets themselves would leak both the length
    /// and the number of correct leading bytes through timing.
    #[test]
    fn secret_comparison_is_over_digests_and_rejects_near_misses() {
        let stored = hash_webhook_secret(SECRET);

        assert!(webhook_secret_matches(SECRET, &stored));
        assert!(!webhook_secret_matches(
            &SECRET[..SECRET.len() - 1],
            &stored
        ));
        assert!(!webhook_secret_matches(&format!("{SECRET}x"), &stored));
        assert!(!webhook_secret_matches("", &stored));

        assert!(
            !stored.contains(SECRET),
            "the stored form must not contain the secret"
        );

        // A malformed stored hash fails closed rather than matching anything.
        assert!(!webhook_secret_matches(SECRET, ""));
        assert!(!webhook_secret_matches(SECRET, "not-hex"));
        assert!(!webhook_secret_matches("", ""));
    }

    /// The point of the whole trigger: a payload from outside becomes a run
    /// input by pointer, and a pipeline starts. Pointers rather than a template
    /// language because they are the vocabulary bindings and gates already use.
    #[tokio::test]
    async fn a_webhook_with_the_right_secret_maps_its_payload_to_the_run_input_by_pointer() {
        let (workflows, tasks, triggers, workflow_id) = fixture().await;
        configure_webhook(&triggers, &workflow_id, true).await;

        let webhook = triggers
            .authenticate_webhook(&workflow_id, Some(SECRET))
            .await
            .expect("authenticate")
            .expect("the right secret gets in");

        let payload = serde_json::json!({
            "ref": "refs/heads/main",
            "head_commit": {"id": "v3.1.4", "message": "ship it"},
        });

        let launched = triggers
            .deliver(&workflows, &tasks, &webhook, &payload)
            .await
            .expect("delivery")
            .expect("the mapped input satisfies the workflow schema");

        let run = workflows
            .get_run(&launched.run.id)
            .await
            .expect("fetch run")
            .expect("run exists");
        assert_eq!(
            run.inputs,
            serde_json::json!({"tag": "v3.1.4"}),
            "only the pointed-at fields become the run input — the rest of an \
             unauthenticated stranger's payload does not enter the pipeline"
        );
        assert_eq!(run.launched_by, webhook_launcher_id(&workflow_id));

        let stored = triggers
            .get_webhook(&workflow_id)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(stored.last_outcome, Some(DeliveryOutcome::Launched));
        assert_eq!(stored.last_run_id, Some(launched.run.id));
    }

    /// Authenticated, and the payload did not have what the mapping wanted.
    /// Its own outcome because its recovery is its own: the pointer or the
    /// sender is wrong, and the pipeline is fine. Reported as a refusal it
    /// would send somebody to read a template that has nothing wrong with it.
    #[tokio::test]
    async fn a_payload_missing_a_mapped_pointer_is_unmapped_rather_than_refused() {
        let (workflows, tasks, triggers, workflow_id) = fixture().await;
        configure_webhook(&triggers, &workflow_id, true).await;

        let webhook = triggers
            .authenticate_webhook(&workflow_id, Some(SECRET))
            .await
            .expect("authenticate")
            .expect("in");

        let (outcome, detail) = triggers
            .deliver(
                &workflows,
                &tasks,
                &webhook,
                &serde_json::json!({"ref": "x"}),
            )
            .await
            .expect("delivery")
            .expect_err("a payload with no /head_commit/id cannot be mapped");

        assert_eq!(outcome, DeliveryOutcome::Unmapped);
        assert!(
            detail.contains("/head_commit/id"),
            "the reason must name the pointer that found nothing: {detail}"
        );
        assert!(
            workflows
                .list_runs(&workflow_id)
                .await
                .expect("list runs")
                .is_empty(),
            "nothing may be emitted for a delivery that could not be mapped"
        );
    }

    /// A rejected delivery must leave no trace it could have written. This
    /// endpoint is reachable without the instance token, so a rejection that
    /// updated a row would hand anyone who found the URL an unauthenticated
    /// write, repeatable as fast as they can send it.
    #[tokio::test]
    async fn a_rejected_delivery_writes_nothing() {
        let (_, _, triggers, workflow_id) = fixture().await;
        configure_webhook(&triggers, &workflow_id, true).await;

        for _ in 0..5 {
            let refused = triggers
                .authenticate_webhook(&workflow_id, Some("wrong-and-long-enough-to-be-plausible"))
                .await
                .expect("authenticate");
            assert_eq!(refused.err(), Some(WebhookRejection::BadSecret));
        }

        let stored = triggers
            .get_webhook(&workflow_id)
            .await
            .expect("fetch")
            .expect("exists");
        assert!(stored.last_delivery_at.is_none());
        assert!(stored.last_outcome.is_none());
        assert!(stored.last_detail.is_none());
    }

    /// An empty pointer selects the whole document, exactly as it does in a
    /// run-input binding. Same vocabulary, same edge case, one answer.
    #[test]
    fn an_empty_pointer_maps_the_whole_payload() {
        let mut pointers = Map::new();
        pointers.insert("tag".into(), Value::from("/a"));
        pointers.insert("everything".into(), Value::from(""));

        let payload = serde_json::json!({"a": "v1"});
        let mapped = map_payload(&pointers, &payload).expect("both pointers resolve");

        assert_eq!(mapped["tag"], serde_json::json!("v1"));
        assert_eq!(mapped["everything"], payload);
    }
}
