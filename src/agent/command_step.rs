//! Running a workflow step that is a command rather than an agent.
//!
//! `bun run lint` is not a question anybody needs a model to answer. The value
//! is not the cost or the latency — it is that a step whose whole purpose is to
//! be an objective check should not route its answer through something that can
//! be mistaken about it. `{"exit_code": 0}` is ground truth; "I ran the linter
//! and it looked clean" is testimony, and every loop and branch predicate
//! downstream reads the former.
//!
//! The load-bearing decision in this file is the split between **ran** and
//! **could not run**, and it is derived at the process level rather than
//! configured. See [`CommandExecution`].

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use crate::sandbox::Sandbox;

/// How much of each stream survives into the outputs.
///
/// `stdout` feeds the next step's prompt, so it is both the point and the risk.
/// Sized to be generous for a lint run and still far short of anything that
/// would dominate a context window.
pub const MAX_COMMAND_OUTPUT_BYTES: usize = 16 * 1024;

/// The longest a command step may be allowed to run, whatever the template says.
///
/// A ceiling rather than a default: the timeout itself is a required field on
/// the step, because the author is the only person who knows whether this is a
/// two-second linter or a four-minute build. This is the wall that stops a
/// template asking for a day.
pub const MAX_COMMAND_TIMEOUT_SECS: i64 = 1800;

/// Everything a command task needs, frozen onto the task at launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub command: String,
    pub timeout_secs: u64,
    /// The exit code that means success, when non-zero really is a failure.
    /// `None` — the common case — means the code is data and any completed run
    /// is a successful task.
    pub expect_exit_code: Option<i64>,
}

/// What the process did.
///
/// Two variants, because they are two different events with two different
/// recoveries, and this codebase has paid five times for one label covering
/// both:
///
/// - **`Ran`** — the command started and exited. The exit code is *data*. A
///   linter reporting `exit 1` is a task that succeeded and whose answer is
///   "there are problems". Charging that to the failure budget would park a lint
///   step before its fix loop had run twice, which is the loop dying of the
///   exact condition it exists to fix.
/// - **`NotRun`** — no exit status was ever produced: the binary is missing, the
///   timeout fired, the process was signalled. Nothing was measured, so there is
///   nothing to report downstream, and the *task* failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandExecution {
    Ran(CommandRun),
    NotRun(CommandNotRun),
}

/// A command that started and exited, whatever it exited with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// A command that never produced an exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandNotRun {
    /// `spawn()` errored — usually a missing interpreter or an unreadable cwd.
    SpawnFailed { detail: String },
    /// The hard timeout fired. `kill_on_drop` takes the process tree with it.
    TimedOut { after_secs: u64 },
    /// The kernel killed it (OOM, an external `kill`). Not an exit code: the
    /// command did not choose to stop, so it did not report anything either.
    Signalled { signal: i32 },
    /// A person stopped the run. Its own variant rather than a spawn failure,
    /// because the recovery is completely different: a missing binary is a
    /// template to fix and is worth retrying under the budget, and somebody
    /// pressing stop is a decision that must not cost the task an attempt.
    Cancelled,
}

impl CommandNotRun {
    pub fn reason(&self) -> String {
        match self {
            CommandNotRun::SpawnFailed { detail } => {
                format!("the command could not be started: {detail}")
            }
            CommandNotRun::TimedOut { after_secs } => {
                format!("the command did not finish within its {after_secs}s timeout")
            }
            CommandNotRun::Signalled { signal } => {
                format!("the command was killed by signal {signal} before it could report")
            }
            CommandNotRun::Cancelled => {
                "the run was stopped before the command reported".to_string()
            }
        }
    }

    /// How the attempt log should classify this.
    ///
    /// `Cancelled` is the one that must not be folded in with the rest:
    /// `TaskRunOutcome::Cancelled` deliberately does not count against the
    /// failure budget, because a person pressing stop is not the task failing.
    pub fn run_outcome(&self) -> crate::tasks::TaskRunOutcome {
        match self {
            CommandNotRun::TimedOut { .. } => crate::tasks::TaskRunOutcome::Timeout,
            CommandNotRun::Cancelled => crate::tasks::TaskRunOutcome::Cancelled,
            CommandNotRun::SpawnFailed { .. } | CommandNotRun::Signalled { .. } => {
                crate::tasks::TaskRunOutcome::Failed
            }
        }
    }
}

impl CommandRun {
    /// The outputs a command step produces, in the shape every downstream
    /// binding, gate, `loop_until` and condition already knows how to read.
    ///
    /// No new plumbing anywhere below here: a command step is just a task that
    /// produces outputs like any other.
    pub fn outputs(&self) -> serde_json::Value {
        serde_json::json!({
            "exit_code": self.exit_code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "duration_ms": self.duration_ms,
            // Machine-visible, not only a marker inside the text. A fix step
            // reading a half log needs to be able to tell that is what it has.
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
        })
    }

    /// Whether the *task* succeeded, given the step's expectation.
    ///
    /// With no `expect_exit_code` this is always true — the code is the answer,
    /// not the verdict.
    pub fn satisfies(&self, expect_exit_code: Option<i64>) -> bool {
        match expect_exit_code {
            None => true,
            Some(expected) => i64::from(self.exit_code) == expected,
        }
    }
}

/// Why a command step was refused before anything was spawned.
///
/// Separate from [`CommandNotRun`] on purpose. These are configuration faults a
/// person fixes once — they park the task as a `capability` block and spend no
/// failure budget, because retrying an uncontained host two more times does not
/// install bubblewrap.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CommandRefusal {
    #[error(
        "this task has no project, repo or worktree binding, so there is no directory to run \
         `{command}` in — bind the step to a repo rather than letting it default to the workspace"
    )]
    NoWorkingDirectory { command: String },
    #[error("task #{task_number} is bound to a directory a command step may not use: {detail}")]
    DirectoryNotAllowed { task_number: i64, detail: String },
    #[error(
        "sandbox.mode is \"enabled\" but no backend was detected, so this command would run with \
         full host access — {remediation}. A command step is stored, repeated and unattended, so \
         it fails closed here rather than inheriting a worker's watched-in-the-moment risk \
         posture. To run uncontained deliberately, set sandbox.mode = \"disabled\": containment is \
         identical either way on this host, and saying so out loud is the difference between a \
         decision and a surprise."
    )]
    ContainmentInert { remediation: String },
}

/// Refuse a command step whose containment is only nominal.
///
/// `containment_status()` distinguishes three states, and the two that read
/// alike from the config surface are exactly the ones that matter here:
/// `mode_enabled()` says yes while `containment_active()` says no. A worker
/// running a command in that state is a considered risk — an agent, watched, as
/// part of work someone asked for. A command step is different in kind: stored
/// in a template, run repeatedly, and unattended.
///
/// Only [`crate::sandbox::ContainmentStatus::RequestedButInert`] is refused.
/// [`crate::sandbox::ContainmentStatus::Disabled`] is an operator's explicit choice and is the
/// escape hatch — on a host with no backend the enforcement is identical, so the
/// only thing that changes is whether the config tells the truth about it.
pub fn check_containment(sandbox: &Sandbox) -> Result<(), CommandRefusal> {
    if sandbox.containment_status().is_inert() {
        return Err(CommandRefusal::ContainmentInert {
            remediation: crate::sandbox::missing_backend_remediation().to_string(),
        });
    }
    Ok(())
}

/// Run a command step to completion.
///
/// Everything here is borrowed rather than invented: [`Sandbox::wrap`], the
/// read/write allowlists, the cwd rule `resolve_worker_working_dir` already
/// enforces, and `kill_on_drop` so a timeout cannot orphan a process tree. The
/// one deliberate difference from the shell tool is that **tool secrets are not
/// injected**: a worker gets them because a worker is trusted to use them, and a
/// template command is authored once and run forever.
pub async fn execute_command(
    sandbox: &Sandbox,
    spec: &CommandSpec,
    working_dir: &Path,
) -> CommandExecution {
    let empty_env: HashMap<String, String> = HashMap::new();
    let mut cmd = if cfg!(target_os = "windows") {
        sandbox.wrap_without_tool_secrets("cmd", &["/C", &spec.command], working_dir, &empty_env)
    } else {
        sandbox.wrap_without_tool_secrets("sh", &["-c", &spec.command], working_dir, &empty_env)
    };

    // `kill_on_drop` on the outermost process — the one `wrap` returned — is
    // what makes the timeout below actually stop work rather than merely stop
    // waiting for it.
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let started = Instant::now();
    let timeout = tokio::time::Duration::from_secs(spec.timeout_secs);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => {
            return CommandExecution::NotRun(CommandNotRun::TimedOut {
                after_secs: spec.timeout_secs,
            });
        }
        Ok(Err(error)) => {
            return CommandExecution::NotRun(CommandNotRun::SpawnFailed {
                detail: error.to_string(),
            });
        }
        Ok(Ok(output)) => output,
    };

    // No exit code means the process was signalled rather than having exited.
    // That is the process-level derivation the whole feature rests on: it did
    // not choose to stop, so it did not report anything either.
    let Some(exit_code) = output.status.code() else {
        return CommandExecution::NotRun(CommandNotRun::Signalled {
            signal: signal_of(&output.status),
        });
    };

    let (stdout, stdout_truncated) = cap_output(
        &String::from_utf8_lossy(&output.stdout),
        MAX_COMMAND_OUTPUT_BYTES,
    );
    let (stderr, stderr_truncated) = cap_output(
        &String::from_utf8_lossy(&output.stderr),
        MAX_COMMAND_OUTPUT_BYTES,
    );

    CommandExecution::Ran(CommandRun {
        exit_code,
        stdout,
        stderr,
        duration_ms: started.elapsed().as_millis() as u64,
        stdout_truncated,
        stderr_truncated,
    })
}

/// What settling a finished command task decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSettlement {
    /// The command ran and its answer is the task's answer. `done`, budget
    /// cleared, outputs written for whatever reads them next.
    Succeeded { exit_code: i32 },
    /// The command ran, the outputs were kept, and the step's declared
    /// expectation was not met. A task failure that still has evidence.
    ExpectationMissed {
        expected: i64,
        actual: i32,
        disposition: crate::tasks::FailureDisposition,
    },
    /// No exit status was ever produced. A task failure with nothing to report.
    CouldNotRun {
        reason: String,
        disposition: crate::tasks::FailureDisposition,
    },
    /// A person stopped the run. The task goes back to `ready` and the budget
    /// is untouched — a stop is a decision, not a failure, and marking it
    /// `done` would let downstream steps read outputs that were never produced.
    Cancelled,
    /// The outputs did not match the step's declared `output_schema`.
    ///
    /// Distinct from the two above because it is neither the command's fault nor
    /// the host's: somebody declared a schema a command step cannot produce.
    OutputsRejected {
        problems: Vec<String>,
        disposition: crate::tasks::FailureDisposition,
    },
}

/// Turn a finished execution into a settled task.
///
/// This is where the ran-vs-failed distinction stops being an enum and starts
/// costing something. Getting it backwards makes every check step burn its
/// failure budget on a working check, so the branch below is deliberately the
/// only place either verdict is written.
pub async fn settle_command_task(
    task_store: &crate::tasks::TaskStore,
    task: &crate::tasks::Task,
    run_id: Option<&str>,
    spec: &CommandSpec,
    execution: &CommandExecution,
) -> anyhow::Result<CommandSettlement> {
    let run = match execution {
        // Split out from the other `NotRun` cases before the budget is
        // consulted at all. `record_failure` short-circuits on an outcome that
        // does not count, which would leave the task sitting in `in_progress`
        // until the reaper charged it an abandonment — the budget arriving by
        // the back door for a stop somebody asked for.
        CommandExecution::NotRun(CommandNotRun::Cancelled) => {
            let reason = CommandNotRun::Cancelled.reason();
            if let Some(run_id) = run_id {
                task_store
                    .finish_run(
                        run_id,
                        crate::tasks::TaskRunOutcome::Cancelled,
                        None,
                        Some(&reason),
                    )
                    .await?;
            }
            task_store
                .update(
                    task.task_number,
                    crate::tasks::UpdateTaskInput {
                        status: Some(crate::tasks::TaskStatus::Ready),
                        clear_worker_id: true,
                        ..Default::default()
                    },
                )
                .await?;
            return Ok(CommandSettlement::Cancelled);
        }
        CommandExecution::NotRun(not_run) => {
            let reason = not_run.reason();
            if let Some(run_id) = run_id {
                task_store
                    .finish_run(run_id, not_run.run_outcome(), None, Some(&reason))
                    .await?;
            }
            let disposition = task_store
                .record_failure(task.task_number, not_run.run_outcome(), &reason)
                .await?;
            return Ok(CommandSettlement::CouldNotRun {
                reason,
                disposition,
            });
        }
        CommandExecution::Ran(run) => run,
    };

    // The outputs are written whatever the verdict. An `exit 1` from a linter is
    // the answer downstream steps were waiting for, and a missed
    // `expect_exit_code` still leaves a person needing to see what the command
    // said. Withholding them on failure would make the failing case the one with
    // the least information.
    let outputs = run.outputs();
    let submission = task_store
        .submit_outputs(task.task_number, &outputs)
        .await?;
    if let crate::tasks::OutputSubmission::Rejected { problems } = submission {
        let problems: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        let reason = format!(
            "the command's outputs do not match this step's declared output_schema: {} — a \
             command step always produces {{exit_code, stdout, stderr, duration_ms}}, so the \
             schema is what has to change",
            problems.join("; ")
        );
        if let Some(run_id) = run_id {
            task_store
                .finish_run(
                    run_id,
                    crate::tasks::TaskRunOutcome::Failed,
                    None,
                    Some(&reason),
                )
                .await?;
        }
        let disposition = task_store
            .record_failure(
                task.task_number,
                crate::tasks::TaskRunOutcome::Failed,
                &reason,
            )
            .await?;
        return Ok(CommandSettlement::OutputsRejected {
            problems,
            disposition,
        });
    }

    let summary = format!(
        "`{}` exited {} in {}ms",
        spec.command, run.exit_code, run.duration_ms
    );

    // The expectation, and *only* the expectation, can turn a completed run into
    // a failed task. Without one, every exit code is data.
    if !run.satisfies(spec.expect_exit_code) {
        let expected = spec.expect_exit_code.unwrap_or_default();
        let reason = format!(
            "{summary}, and this step requires exit {expected} — the command ran, so its output \
             is on the task; it is the result that is wrong, not the run"
        );
        if let Some(run_id) = run_id {
            task_store
                .finish_run(
                    run_id,
                    crate::tasks::TaskRunOutcome::Failed,
                    Some(&summary),
                    Some(&reason),
                )
                .await?;
        }
        let disposition = task_store
            .record_failure(
                task.task_number,
                crate::tasks::TaskRunOutcome::Failed,
                &reason,
            )
            .await?;
        return Ok(CommandSettlement::ExpectationMissed {
            expected,
            actual: run.exit_code,
            disposition,
        });
    }

    if let Some(run_id) = run_id {
        task_store
            .finish_run(
                run_id,
                crate::tasks::TaskRunOutcome::Completed,
                Some(&summary),
                None,
            )
            .await?;
    }

    // A clean completion clears the failure budget, exactly as the worker path
    // does, so a step that failed twice and then passed does not carry a hair
    // trigger into its next run.
    if task.consecutive_failures > 0 {
        task_store.clear_failures(task.task_number).await?;
    }

    task_store
        .update(
            task.task_number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Done),
                ..Default::default()
            },
        )
        .await?;

    Ok(CommandSettlement::Succeeded {
        exit_code: run.exit_code,
    })
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().unwrap_or(-1)
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> i32 {
    -1
}

/// Cap a stream, keeping the head **and the tail**, and saying so in the gap.
///
/// Head-and-tail rather than head alone because the useful part of a failing
/// build log is usually at the end — the error, not the banner. Silently handing
/// a model half a log with no indication is how a fix loop ends up confidently
/// fixing the wrong thing, so the marker is inside the text as well as being a
/// flag in the outputs.
///
/// Returns the capped text and whether anything was dropped.
pub fn cap_output(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }

    let half = max_bytes / 2;
    let head_end = floor_char_boundary(value, half);
    let tail_start = ceil_char_boundary(value, value.len() - (max_bytes - head_end));
    let omitted = tail_start - head_end;

    let mut out = String::with_capacity(max_bytes + 96);
    out.push_str(&value[..head_end]);
    out.push_str(&format!(
        "\n\n[... {omitted} bytes omitted from the middle of {} total; head and tail kept ...]\n\n",
        value.len()
    ));
    out.push_str(&value[tail_start..]);
    (out, true)
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sandbox_with_mode(mode: crate::sandbox::SandboxMode) -> Sandbox {
        Sandbox::new_for_test(
            Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::sandbox::SandboxConfig {
                    mode,
                    ..crate::sandbox::SandboxConfig::default()
                },
            )),
            std::env::temp_dir(),
        )
    }

    /// The whole feature. A linter that reports problems has *answered the
    /// question it was asked*; if that ever reads as a failed command the task
    /// layer above will charge it to the failure budget and park the step before
    /// its fix loop has run twice.
    #[tokio::test]
    async fn a_command_that_exits_non_zero_still_counts_as_having_run() {
        let sandbox = sandbox_with_mode(crate::sandbox::SandboxMode::Disabled);
        let spec = CommandSpec {
            command: "echo problems; exit 3".to_string(),
            timeout_secs: 30,
            expect_exit_code: None,
        };

        let execution = execute_command(&sandbox, &spec, &std::env::temp_dir()).await;
        let CommandExecution::Ran(run) = execution else {
            panic!("a command that exits is a command that ran: {execution:?}");
        };
        assert_eq!(run.exit_code, 3);
        assert!(run.stdout.contains("problems"));
        assert!(
            run.satisfies(None),
            "with no expectation, any exit code is a successful task"
        );
    }

    /// The other half of the split. A command that never started produced no
    /// measurement, so there is nothing downstream can trust and the task has to
    /// fail rather than report `exit_code: 127` as if it were an answer.
    #[tokio::test]
    async fn a_command_whose_working_directory_does_not_exist_never_runs_at_all() {
        let sandbox = sandbox_with_mode(crate::sandbox::SandboxMode::Disabled);
        let spec = CommandSpec {
            command: "true".to_string(),
            timeout_secs: 30,
            expect_exit_code: None,
        };

        let missing = std::env::temp_dir().join("spacebot-no-such-dir-9a8b7c6d");
        let execution = execute_command(&sandbox, &spec, &missing).await;
        assert!(
            matches!(
                execution,
                CommandExecution::NotRun(CommandNotRun::SpawnFailed { .. })
            ),
            "a cwd that does not exist must be a spawn failure, not an exit code: {execution:?}"
        );
    }

    /// `expect_exit_code` is what a `git push` step uses. Without it the step
    /// would quietly "succeed" with exit 1 and the pipeline would carry on as if
    /// the push had landed.
    #[tokio::test]
    async fn expect_exit_code_turns_a_mismatch_into_a_failed_task_while_keeping_the_outputs() {
        let sandbox = sandbox_with_mode(crate::sandbox::SandboxMode::Disabled);
        let spec = CommandSpec {
            command: "echo nope >&2; exit 1".to_string(),
            timeout_secs: 30,
            expect_exit_code: Some(0),
        };

        let execution = execute_command(&sandbox, &spec, &std::env::temp_dir()).await;
        let CommandExecution::Ran(run) = execution else {
            panic!("the command ran; only the expectation was missed");
        };
        assert!(!run.satisfies(Some(0)));
        assert_eq!(run.outputs()["exit_code"], 1);
        assert!(
            run.stderr.contains("nope"),
            "a failed expectation must still carry what the command said"
        );
    }

    /// The timeout is not an exit code. A build killed at its ceiling reported
    /// nothing, and a downstream predicate reading `exit_code` from it would be
    /// reading a number nobody produced.
    #[tokio::test]
    async fn a_command_that_outlives_its_timeout_did_not_run() {
        let sandbox = sandbox_with_mode(crate::sandbox::SandboxMode::Disabled);
        let spec = CommandSpec {
            command: "sleep 30".to_string(),
            timeout_secs: 1,
            expect_exit_code: None,
        };

        let execution = execute_command(&sandbox, &spec, &std::env::temp_dir()).await;
        assert!(
            matches!(
                execution,
                CommandExecution::NotRun(CommandNotRun::TimedOut { after_secs: 1 })
            ),
            "{execution:?}"
        );
    }

    /// Head *and* tail. The useful part of a failing build log is at the end,
    /// and a cap that kept only the head would hand a fix step the banner and
    /// hide the error it exists to read.
    #[test]
    fn output_over_the_cap_keeps_the_head_and_the_tail_and_says_what_it_dropped() {
        let value = format!("{}MIDDLE{}", "H".repeat(4000), "T".repeat(4000));
        let (capped, truncated) = cap_output(&value, 1000);

        assert!(
            truncated,
            "the caller must be able to tell it was truncated"
        );
        assert!(capped.starts_with("HHHH"), "the head survives");
        assert!(capped.ends_with("TTTT"), "the tail survives");
        assert!(
            capped.contains("bytes omitted"),
            "the gap has to say it is a gap: {capped:.200}"
        );
        assert!(
            !capped.contains("MIDDLE"),
            "the middle is what gets dropped"
        );
    }

    /// Output that fits is passed through untouched — a marker on a complete log
    /// would be a lie of exactly the kind the marker exists to prevent.
    #[test]
    fn output_under_the_cap_is_untouched_and_reports_no_truncation() {
        let (capped, truncated) = cap_output("all of it", 1000);
        assert_eq!(capped, "all of it");
        assert!(!truncated);
    }

    /// Command output is arbitrary bytes. Splitting a multi-byte character in
    /// half would panic on the slice, taking the whole pickup pass with it.
    #[test]
    fn capping_multibyte_output_does_not_split_a_character() {
        let value = "日本語のログ".repeat(200);
        let (capped, truncated) = cap_output(&value, 100);
        assert!(truncated);
        assert!(!capped.is_empty());
    }

    // -- Task-level settlement ---------------------------------------------
    //
    // The unit tests above prove the process-level split. These prove it
    // survives contact with the failure budget, which is where getting it
    // backwards actually costs something.

    async fn task_fixture(
        command: &str,
        expect_exit_code: Option<i64>,
    ) -> (crate::tasks::TaskStore, crate::tasks::Task, CommandSpec) {
        use sqlx::sqlite::SqlitePoolOptions;

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::tasks::store::create_task_schema(&pool).await;
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("sequence seed");

        let store = crate::tasks::TaskStore::new(pool.clone());
        let task = store
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-1".into(),
                assigned_agent_id: "agent-1".into(),
                title: "lint".into(),
                created_by: "agent-1".into(),
                ..Default::default()
            })
            .await
            .expect("create task");

        sqlx::query(
            "UPDATE tasks SET kind = 'command', command = ?, command_timeout_secs = 30, \
             expect_exit_code = ?, status = 'in_progress' WHERE task_number = ?",
        )
        .bind(command)
        .bind(expect_exit_code)
        .bind(task.task_number)
        .execute(&pool)
        .await
        .expect("make it a command task");

        let task = store
            .get_by_number(task.task_number)
            .await
            .expect("read back")
            .expect("task exists");
        let spec = task.command_spec().expect("a command task has a spec");
        (store, task, spec)
    }

    /// **The load-bearing test.** A linter that exits 1 has answered the
    /// question it was asked, so the task is `done` and the code is in its
    /// outputs for the fix step to read. If this ever asserts failure instead,
    /// every check step burns two attempts of its failure budget and parks
    /// before its fix loop has run twice — the loop dying of the exact condition
    /// it exists to fix.
    #[tokio::test]
    async fn a_command_that_exits_non_zero_settles_its_task_as_done_with_the_code_in_its_outputs() {
        let (store, task, spec) = task_fixture("lint", None).await;
        let execution = CommandExecution::Ran(CommandRun {
            exit_code: 1,
            stdout: "3 problems".into(),
            stderr: String::new(),
            duration_ms: 840,
            stdout_truncated: false,
            stderr_truncated: false,
        });

        let settlement = settle_command_task(&store, &task, None, &spec, &execution)
            .await
            .expect("settle");
        assert_eq!(settlement, CommandSettlement::Succeeded { exit_code: 1 });

        let settled = store
            .get_by_number(task.task_number)
            .await
            .expect("read")
            .expect("task");
        assert_eq!(settled.status, crate::tasks::TaskStatus::Done);
        assert_eq!(
            settled.consecutive_failures, 0,
            "a reported problem is not a failed attempt"
        );
        let outputs = settled.outputs.expect("a command step always has outputs");
        assert_eq!(outputs["exit_code"], 1);
        assert_eq!(outputs["stdout"], "3 problems");
        assert_eq!(outputs["duration_ms"], 840);
    }

    /// The other side of the split, and the one that has to spend budget. A
    /// command that never started produced no measurement, so there is nothing
    /// for a downstream predicate to read and retrying is the right response —
    /// bounded, by the budget, so a permanently broken command parks instead of
    /// hot-looping.
    #[tokio::test]
    async fn a_command_that_could_not_be_spawned_fails_its_task_and_spends_a_failure() {
        let (store, task, spec) = task_fixture("does-not-exist", None).await;
        let execution = CommandExecution::NotRun(CommandNotRun::SpawnFailed {
            detail: "No such file or directory (os error 2)".into(),
        });

        let settlement = settle_command_task(&store, &task, None, &spec, &execution)
            .await
            .expect("settle");
        assert!(
            matches!(settlement, CommandSettlement::CouldNotRun { .. }),
            "{settlement:?}"
        );

        let settled = store
            .get_by_number(task.task_number)
            .await
            .expect("read")
            .expect("task");
        assert_eq!(
            settled.consecutive_failures, 1,
            "a command that could not run is a spent attempt"
        );
        assert!(
            settled.outputs.is_none(),
            "there is nothing to report when nothing ran"
        );
        assert_ne!(settled.status, crate::tasks::TaskStatus::Done);
    }

    /// `expect_exit_code` is the opt-in for the steps where non-zero really is a
    /// failure. It must fail the *task* while still leaving the command's output
    /// on the card — the failing case is the one where a person most needs to
    /// see what the command said.
    #[tokio::test]
    async fn a_missed_expect_exit_code_fails_the_task_and_spends_a_failure_but_keeps_the_output() {
        let (store, task, spec) = task_fixture("git push", Some(0)).await;
        let execution = CommandExecution::Ran(CommandRun {
            exit_code: 1,
            stdout: String::new(),
            stderr: "rejected: non-fast-forward".into(),
            duration_ms: 120,
            stdout_truncated: false,
            stderr_truncated: false,
        });

        let settlement = settle_command_task(&store, &task, None, &spec, &execution)
            .await
            .expect("settle");
        assert!(
            matches!(
                settlement,
                CommandSettlement::ExpectationMissed {
                    expected: 0,
                    actual: 1,
                    ..
                }
            ),
            "{settlement:?}"
        );

        let settled = store
            .get_by_number(task.task_number)
            .await
            .expect("read")
            .expect("task");
        assert_eq!(settled.consecutive_failures, 1);
        assert_ne!(settled.status, crate::tasks::TaskStatus::Done);
        let outputs = settled
            .outputs
            .expect("a failed expectation still ran, so it still has evidence");
        assert_eq!(outputs["stderr"], "rejected: non-fast-forward");
    }

    /// Somebody pressing stop is a decision, not a failure. Charging it to the
    /// budget — directly, or by leaving the task `in_progress` for the reaper to
    /// call abandoned — would let two stops park a perfectly good step.
    #[tokio::test]
    async fn a_stopped_command_returns_its_task_to_ready_without_spending_a_failure() {
        let (store, task, spec) = task_fixture("bun run lint", None).await;
        let execution = CommandExecution::NotRun(CommandNotRun::Cancelled);

        let settlement = settle_command_task(&store, &task, None, &spec, &execution)
            .await
            .expect("settle");
        assert_eq!(settlement, CommandSettlement::Cancelled);

        let settled = store
            .get_by_number(task.task_number)
            .await
            .expect("read")
            .expect("task");
        assert_eq!(settled.status, crate::tasks::TaskStatus::Ready);
        assert_eq!(settled.consecutive_failures, 0);
        assert!(
            settled.worker_id.is_none(),
            "a stopped command must not leave a worker id the reaper would then chase"
        );
        assert!(
            settled.outputs.is_none(),
            "marking it done would let downstream steps read outputs nothing produced"
        );
    }

    /// A command task whose row lost its command line must be reported, not run
    /// and not silently treated as an agent task. Substituting a default timeout
    /// would be inventing the one number nobody chose.
    #[tokio::test]
    async fn a_command_task_with_no_command_line_has_no_spec_to_run() {
        let (store, task, _) = task_fixture("lint", None).await;
        sqlx::query("UPDATE tasks SET command = NULL WHERE task_number = ?")
            .bind(task.task_number)
            .execute(store.pool())
            .await
            .expect("clear the command");

        let task = store
            .get_by_number(task.task_number)
            .await
            .expect("read")
            .expect("task");
        assert_eq!(task.kind, crate::tasks::TaskKind::Command);
        assert!(task.command_spec().is_none());
    }

    /// A command step is stored, repeated and unattended, so it must not inherit
    /// a worker's watched-in-the-moment risk posture. `mode = "enabled"` with no
    /// backend is the state this host is actually in, and it looks identical to
    /// a working sandbox from the config surface.
    #[test]
    fn a_command_step_refuses_to_run_when_containment_is_requested_but_inert() {
        let sandbox = sandbox_with_mode(crate::sandbox::SandboxMode::Enabled);
        // The test host has no bubblewrap; if it ever gains one this assertion
        // stops being about the case we care about, so it is asserted first.
        if !sandbox.containment_status().is_inert() {
            return;
        }

        let refusal = check_containment(&sandbox)
            .expect_err("an inert sandbox must fail closed for stored code execution");
        assert!(
            matches!(refusal, CommandRefusal::ContainmentInert { .. }),
            "{refusal:?}"
        );
        assert!(
            refusal.to_string().contains("sandbox.mode = \"disabled\""),
            "the refusal has to name the escape hatch: {refusal}"
        );
    }

    /// The escape hatch, and the reason it is not a fourth config flag. A
    /// deliberately disabled sandbox is a choice somebody made; an inert one is
    /// a surprise. Refusing on both would make the feature unusable on every
    /// host without a backend and teach people to route around it.
    #[test]
    fn a_deliberately_disabled_sandbox_is_a_choice_and_is_not_refused() {
        let sandbox = sandbox_with_mode(crate::sandbox::SandboxMode::Disabled);
        assert!(check_containment(&sandbox).is_ok());
    }
}
