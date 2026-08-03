//! Workflow template storage and instantiation.

use crate::error::Result;
use crate::tasks::{
    ContractProblem, ContractSide, CreateTaskInput, TaskInputBinding, TaskPriority,
    TaskProjectBinding, TaskStatus, TaskStore,
};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};
use std::collections::{HashMap, HashSet};

/// A reusable pipeline definition.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// JSON Schema for the input a whole run is launched with.
    pub input_schema: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// One step of a pipeline. Becomes exactly one task per launch.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowStep {
    pub workflow_id: String,
    /// Stable name that edges and bindings reference.
    pub step_key: String,
    pub title: String,
    pub description: Option<String>,
    /// `None` means the agent that launched the run.
    pub assigned_agent_id: Option<String>,
    pub priority: TaskPriority,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    /// Per-step instructions appended to the worker prompt at pickup.
    pub system_prompt: Option<String>,
    pub repo_id: Option<String>,
    pub position: i64,
}

/// Where a step's input comes from.
///
/// Named rather than inferred from which column is populated. The task-level
/// table infers "literal" from a NULL source, which works for rows the store
/// writes and is a trap for rows a human edits: a malformed binding is
/// indistinguishable from a deliberate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingSource {
    /// Read another step's output at `source_pointer`.
    Step,
    /// A value baked into the template.
    Literal,
    /// Read the run's own launch input at `source_pointer`.
    ///
    /// This is what lets one input drive a whole pipeline: a step binds
    /// straight into the launch payload, so the entry point is declarative and
    /// there is no special-cased first step.
    RunInput,
}

impl BindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingSource::Step => "step",
            BindingSource::Literal => "literal",
            BindingSource::RunInput => "run_input",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "step" => Some(BindingSource::Step),
            "literal" => Some(BindingSource::Literal),
            "run_input" => Some(BindingSource::RunInput),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct StepBinding {
    pub workflow_id: String,
    pub step_key: String,
    pub input_key: String,
    pub source: BindingSource,
    pub source_step_key: Option<String>,
    /// RFC 6901 JSON Pointer. Empty selects the whole document.
    pub source_pointer: Option<String>,
    pub literal_value: Option<Value>,
}

/// One launch of a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_id: String,
    pub inputs: Value,
    pub launched_by: String,
    pub created_at: String,
}

/// The result of a successful launch.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstantiatedRun {
    pub run: WorkflowRun,
    /// Emitted task numbers, keyed by the step they came from.
    pub task_numbers: HashMap<String, i64>,
}

/// Why a launch was refused.
///
/// Every variant is something a person can act on. A launch that fails with
/// "invalid workflow" sends someone reading rows; naming the step and the key
/// points at the line to change.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LaunchError {
    #[error("workflow {id} does not exist")]
    UnknownWorkflow { id: String },
    #[error("workflow {id} has no steps")]
    NoSteps { id: String },
    #[error(
        "step `{step_key}` binds `{input_key}` to step `{missing}`, which is not in this workflow"
    )]
    UnknownStepReference {
        step_key: String,
        input_key: String,
        missing: String,
    },
    #[error("edge {parent} -> {child} references a step that is not in this workflow")]
    UnknownEdgeReference { parent: String, child: String },
    #[error(
        "the steps form a cycle and cannot be ordered: {}",
        .cycle.join(" -> ")
    )]
    Cycle { cycle: Vec<String> },
    #[error("launch input does not match the workflow's schema: {details}")]
    InvalidInput { details: String },
    #[error(
        "step `{step_key}` binds `{input_key}` to run input at `{pointer}`, which the launch input does not contain"
    )]
    MissingRunInput {
        step_key: String,
        input_key: String,
        pointer: String,
    },
    #[error(
        "step `{step_key}` declares `{input_key}` as required but nothing binds it — \
         add a binding, or drop it from the step's input schema"
    )]
    UnboundRequiredInput { step_key: String, input_key: String },
    #[error("workflow storage error: {0}")]
    Storage(String),
}

pub struct WorkflowStore {
    pool: SqlitePool,
}

impl WorkflowStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // -- Templates ----------------------------------------------------------

    pub async fn create_workflow(
        &self,
        name: &str,
        description: Option<&str>,
        input_schema: Option<&Value>,
    ) -> Result<Workflow> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO workflows (id, name, description, input_schema) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(description)
        .bind(input_schema.map(|value| value.to_string()))
        .execute(&self.pool)
        .await
        .context("failed to create workflow")?;

        self.get_workflow(&id)
            .await?
            .context("workflow inserted but not found")
            .map_err(Into::into)
    }

    /// Replace a template's own fields. Steps, edges, and bindings are
    /// addressed separately and are untouched by this.
    pub async fn update_workflow(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        input_schema: Option<&Value>,
    ) -> Result<Option<Workflow>> {
        let result = sqlx::query(
            "UPDATE workflows SET name = ?, description = ?, input_schema = ?, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?",
        )
        .bind(name)
        .bind(description)
        .bind(input_schema.map(|value| value.to_string()))
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to update workflow")?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_workflow(id).await
    }

    pub async fn get_workflow(&self, id: &str) -> Result<Option<Workflow>> {
        let row = sqlx::query(
            "SELECT id, name, description, input_schema, created_at, updated_at \
             FROM workflows WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch workflow")?;

        row.map(workflow_from_row).transpose()
    }

    pub async fn list_workflows(&self) -> Result<Vec<Workflow>> {
        let rows = sqlx::query(
            "SELECT id, name, description, input_schema, created_at, updated_at \
             FROM workflows ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflows")?;

        rows.into_iter().map(workflow_from_row).collect()
    }

    pub async fn delete_workflow(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM workflows WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete workflow")?;
        Ok(result.rows_affected() > 0)
    }

    /// Add or replace a step. Keyed on `step_key`, so editing a step is one
    /// call and cannot leave a duplicate behind.
    pub async fn put_step(&self, step: &WorkflowStep) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_steps \
                 (workflow_id, step_key, title, description, assigned_agent_id, priority, \
                  input_schema, output_schema, system_prompt, repo_id, position) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (workflow_id, step_key) DO UPDATE SET \
                 title = excluded.title, description = excluded.description, \
                 assigned_agent_id = excluded.assigned_agent_id, priority = excluded.priority, \
                 input_schema = excluded.input_schema, output_schema = excluded.output_schema, \
                 system_prompt = excluded.system_prompt, repo_id = excluded.repo_id, \
                 position = excluded.position",
        )
        .bind(&step.workflow_id)
        .bind(&step.step_key)
        .bind(&step.title)
        .bind(&step.description)
        .bind(&step.assigned_agent_id)
        .bind(step.priority.as_str())
        .bind(step.input_schema.as_ref().map(|v| v.to_string()))
        .bind(step.output_schema.as_ref().map(|v| v.to_string()))
        .bind(&step.system_prompt)
        .bind(&step.repo_id)
        .bind(step.position)
        .execute(&self.pool)
        .await
        .context("failed to write workflow step")?;
        Ok(())
    }

    pub async fn list_steps(&self, workflow_id: &str) -> Result<Vec<WorkflowStep>> {
        let rows = sqlx::query(
            "SELECT workflow_id, step_key, title, description, assigned_agent_id, priority, \
                    input_schema, output_schema, system_prompt, repo_id, position \
             FROM workflow_steps WHERE workflow_id = ? ORDER BY position ASC, step_key ASC",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow steps")?;

        rows.into_iter().map(step_from_row).collect()
    }

    /// Remove a step, and everything that referenced it.
    ///
    /// The cascade is the point. An edge or binding naming a step that no
    /// longer exists makes *every* future launch fail validation, and the
    /// dangling row is invisible in a step-oriented editor — so the template
    /// would be permanently unlaunchable with nothing on screen to explain it.
    /// Deleting a step is the only moment we can be sure those rows are stale.
    pub async fn delete_step(&self, workflow_id: &str, step_key: &str) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM workflow_steps WHERE workflow_id = ? AND step_key = ?")
                .bind(workflow_id)
                .bind(step_key)
                .execute(&self.pool)
                .await
                .context("failed to delete workflow step")?;

        sqlx::query(
            "DELETE FROM workflow_step_edges WHERE workflow_id = ? \
             AND (parent_step_key = ? OR child_step_key = ?)",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(step_key)
        .execute(&self.pool)
        .await
        .context("failed to delete edges of a removed step")?;

        // Both directions: bindings *on* the step, and bindings elsewhere that
        // read *from* it.
        sqlx::query(
            "DELETE FROM workflow_step_bindings WHERE workflow_id = ? \
             AND (step_key = ? OR source_step_key = ?)",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(step_key)
        .execute(&self.pool)
        .await
        .context("failed to delete bindings of a removed step")?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn link_steps(&self, workflow_id: &str, parent: &str, child: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO workflow_step_edges \
                 (workflow_id, parent_step_key, child_step_key) VALUES (?, ?, ?)",
        )
        .bind(workflow_id)
        .bind(parent)
        .bind(child)
        .execute(&self.pool)
        .await
        .context("failed to link workflow steps")?;
        Ok(())
    }

    /// Would adding `parent -> child` close a loop? If so, the path that does.
    ///
    /// Checked when the edge is *saved*, not only at launch. A template that
    /// accepts a cycle and refuses to run is a trap: the author gets a success,
    /// walks away, and finds out much later — possibly from someone else. The
    /// path is returned rather than a bare yes so the refusal can name the loop.
    pub async fn cycle_from_edge(
        &self,
        workflow_id: &str,
        parent: &str,
        child: &str,
    ) -> Result<Option<Vec<String>>> {
        if parent == child {
            return Ok(Some(vec![parent.to_string(), child.to_string()]));
        }

        let edges = self.list_edges(workflow_id).await?;

        // Walk forward from `child`. Reaching `parent` means `parent` is
        // already downstream of `child`, so the new edge would close the ring.
        // Iterative: the graph is hand-built and a deep chain must not blow the
        // stack.
        let mut frontier = vec![vec![child.to_string()]];
        let mut seen: HashSet<String> = HashSet::from([child.to_string()]);

        while let Some(path) = frontier.pop() {
            let tip = path.last().expect("a path always has a tip").clone();
            for (from, to) in &edges {
                if from != &tip {
                    continue;
                }
                if to == parent {
                    let mut cycle = path.clone();
                    cycle.push(to.clone());
                    return Ok(Some(cycle));
                }
                if seen.insert(to.clone()) {
                    let mut next = path.clone();
                    next.push(to.clone());
                    frontier.push(next);
                }
            }
        }

        Ok(None)
    }

    /// The next free display position, so steps added without one do not stack.
    pub async fn next_step_position(&self, workflow_id: &str) -> Result<i64> {
        let highest: Option<i64> =
            sqlx::query_scalar("SELECT MAX(position) FROM workflow_steps WHERE workflow_id = ?")
                .bind(workflow_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to read the highest step position")?;
        Ok(highest.map(|value| value + 1).unwrap_or(0))
    }

    pub async fn unlink_steps(&self, workflow_id: &str, parent: &str, child: &str) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workflow_step_edges WHERE workflow_id = ? \
             AND parent_step_key = ? AND child_step_key = ?",
        )
        .bind(workflow_id)
        .bind(parent)
        .bind(child)
        .execute(&self.pool)
        .await
        .context("failed to unlink workflow steps")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_edges(&self, workflow_id: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT parent_step_key, child_step_key FROM workflow_step_edges \
             WHERE workflow_id = ? ORDER BY parent_step_key, child_step_key",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow edges")?;

        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("parent_step_key")
                        .context("failed to read parent_step_key")?,
                    row.try_get("child_step_key")
                        .context("failed to read child_step_key")?,
                ))
            })
            .collect()
    }

    pub async fn put_binding(&self, binding: &StepBinding) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_step_bindings \
                 (workflow_id, step_key, input_key, source, source_step_key, source_pointer, \
                  literal_value) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (workflow_id, step_key, input_key) DO UPDATE SET \
                 source = excluded.source, source_step_key = excluded.source_step_key, \
                 source_pointer = excluded.source_pointer, \
                 literal_value = excluded.literal_value",
        )
        .bind(&binding.workflow_id)
        .bind(&binding.step_key)
        .bind(&binding.input_key)
        .bind(binding.source.as_str())
        .bind(&binding.source_step_key)
        .bind(&binding.source_pointer)
        .bind(binding.literal_value.as_ref().map(|v| v.to_string()))
        .execute(&self.pool)
        .await
        .context("failed to write workflow binding")?;
        Ok(())
    }

    pub async fn delete_binding(
        &self,
        workflow_id: &str,
        step_key: &str,
        input_key: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workflow_step_bindings WHERE workflow_id = ? \
             AND step_key = ? AND input_key = ?",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(input_key)
        .execute(&self.pool)
        .await
        .context("failed to delete workflow binding")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_bindings(&self, workflow_id: &str) -> Result<Vec<StepBinding>> {
        let rows = sqlx::query(
            "SELECT workflow_id, step_key, input_key, source, source_step_key, source_pointer, \
                    literal_value \
             FROM workflow_step_bindings WHERE workflow_id = ? ORDER BY step_key, input_key",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow bindings")?;

        rows.into_iter().map(binding_from_row).collect()
    }

    // -- Launch -------------------------------------------------------------

    /// Compile a workflow into tasks, edges, and bindings, and record the run.
    ///
    /// Everything is validated before anything is written, because a half-built
    /// graph is worse than none: the scheduler would immediately start running
    /// the part that exists. Validation cannot be the whole story though —
    /// writing spans two stores and can still fail on its own (a closed pool, a
    /// disk error), so a failure part-way through unwinds what it emitted
    /// before returning. See `rollback_run`.
    pub async fn launch(
        &self,
        task_store: &TaskStore,
        workflow_id: &str,
        inputs: &Value,
        launched_by: &str,
    ) -> std::result::Result<InstantiatedRun, LaunchError> {
        let workflow = self
            .get_workflow(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?
            .ok_or_else(|| LaunchError::UnknownWorkflow {
                id: workflow_id.to_string(),
            })?;

        let steps = self
            .list_steps(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        if steps.is_empty() {
            return Err(LaunchError::NoSteps {
                id: workflow_id.to_string(),
            });
        }

        let edges = self
            .list_edges(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        let bindings = self
            .list_bindings(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;

        // 1. The launch input, checked once, here — where a person is still
        //    watching. Deferring it means the failure surfaces as an
        //    unresolvable binding inside step three, a long way from the cause.
        if let Some(schema) = &workflow.input_schema {
            let problems = validate(schema, inputs);
            if !problems.is_empty() {
                return Err(LaunchError::InvalidInput {
                    details: problems.join("; "),
                });
            }
        }

        let known: HashSet<&str> = steps.iter().map(|step| step.step_key.as_str()).collect();

        // 2. Every reference resolves. Checked before the sort so a typo is
        //    reported as a typo rather than as a missing graph node.
        for (parent, child) in &edges {
            if !known.contains(parent.as_str()) || !known.contains(child.as_str()) {
                return Err(LaunchError::UnknownEdgeReference {
                    parent: parent.clone(),
                    child: child.clone(),
                });
            }
        }
        for binding in &bindings {
            if binding.source == BindingSource::Step {
                let target = binding.source_step_key.as_deref().unwrap_or("");
                if !known.contains(target) {
                    return Err(LaunchError::UnknownStepReference {
                        step_key: binding.step_key.clone(),
                        input_key: binding.input_key.clone(),
                        missing: target.to_string(),
                    });
                }
            }
        }

        // 3. Every input a step says it needs is actually wired.
        //
        //    Without this the mistake surfaces at run time as an unresolvable
        //    contract, one step into a pipeline someone thought was fine — the
        //    same class of failure as an unknown step reference, and equally
        //    knowable at launch. Only `required` keys are checked: an optional
        //    input with no binding is a deliberate default, not an omission.
        for step in &steps {
            let Some(required) = step
                .input_schema
                .as_ref()
                .and_then(|schema| schema.get("required"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for key in required.iter().filter_map(Value::as_str) {
                let bound = bindings
                    .iter()
                    .any(|binding| binding.step_key == step.step_key && binding.input_key == key);
                if !bound {
                    return Err(LaunchError::UnboundRequiredInput {
                        step_key: step.step_key.clone(),
                        input_key: key.to_string(),
                    });
                }
            }
        }

        // 4. Acyclic. A cycle here would deadlock silently at run time: every
        //    step waiting on a parent that is itself waiting.
        topologically_ordered(&steps, &edges)?;

        // 5. Resolve run-input bindings now, while the payload is in hand.
        //    Frozen to literals rather than kept as live references so a step
        //    retried an hour later sees exactly what the run was launched with.
        let mut frozen: HashMap<(String, String), Value> = HashMap::new();
        for binding in &bindings {
            if binding.source != BindingSource::RunInput {
                continue;
            }
            let pointer = binding.source_pointer.as_deref().unwrap_or("");
            let value = if pointer.is_empty() {
                Some(inputs)
            } else {
                inputs.pointer(pointer)
            };
            let value = value.ok_or_else(|| LaunchError::MissingRunInput {
                step_key: binding.step_key.clone(),
                input_key: binding.input_key.clone(),
                pointer: pointer.to_string(),
            })?;
            frozen.insert(
                (binding.step_key.clone(), binding.input_key.clone()),
                value.clone(),
            );
        }

        // 6. Emit. Everything above passed, so this should not fail on content.
        let run_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO workflow_runs (id, workflow_id, inputs, launched_by) VALUES (?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(workflow_id)
        .bind(inputs.to_string())
        .bind(launched_by)
        .execute(&self.pool)
        .await
        .map_err(|error| LaunchError::Storage(error.to_string()))?;

        let mut task_numbers: HashMap<String, i64> = HashMap::new();
        if let Err(error) = self
            .emit_graph(
                task_store,
                &run_id,
                &steps,
                &edges,
                &bindings,
                &frozen,
                launched_by,
                &mut task_numbers,
            )
            .await
        {
            self.rollback_run(task_store, &run_id, &task_numbers).await;
            return Err(error);
        }

        let run = self
            .get_run(&run_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?
            .ok_or_else(|| LaunchError::Storage("run inserted but not found".to_string()))?;

        Ok(InstantiatedRun { run, task_numbers })
    }

    /// Write the tasks, edges, and bindings for a validated launch.
    ///
    /// `task_numbers` is an out-parameter rather than a return value so the
    /// caller can clean up whatever was emitted before a failure.
    #[allow(clippy::too_many_arguments)]
    async fn emit_graph(
        &self,
        task_store: &TaskStore,
        run_id: &str,
        steps: &[WorkflowStep],
        edges: &[(String, String)],
        bindings: &[StepBinding],
        frozen: &HashMap<(String, String), Value>,
        launched_by: &str,
        task_numbers: &mut HashMap<String, i64>,
    ) -> std::result::Result<(), LaunchError> {
        for step in steps {
            let task = task_store
                .create(CreateTaskInput {
                    owner_agent_id: launched_by.to_string(),
                    assigned_agent_id: step
                        .assigned_agent_id
                        .clone()
                        .unwrap_or_else(|| launched_by.to_string()),
                    title: step.title.clone(),
                    description: step.description.clone(),
                    // Every step starts in backlog, entry steps included. The
                    // sweep decides what is eligible by the same rule it uses
                    // for everything else, rather than the instantiator
                    // guessing which step is first. A pipeline whose first step
                    // has an unsatisfiable binding then stalls visibly instead
                    // of being claimed and immediately blocked.
                    status: TaskStatus::Backlog,
                    priority: step.priority,
                    created_by: launched_by.to_string(),
                    binding: TaskProjectBinding {
                        repo_id: step.repo_id.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;

            sqlx::query(
                "UPDATE tasks SET workflow_run_id = ?, workflow_step_key = ?, \
                 input_schema = ?, output_schema = ?, system_prompt = ? WHERE task_number = ?",
            )
            .bind(run_id)
            .bind(&step.step_key)
            .bind(step.input_schema.as_ref().map(|v| v.to_string()))
            .bind(step.output_schema.as_ref().map(|v| v.to_string()))
            .bind(&step.system_prompt)
            .bind(task.task_number)
            .execute(&self.pool)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;

            task_numbers.insert(step.step_key.clone(), task.task_number);
        }

        for (parent, child) in edges {
            let (Some(parent_number), Some(child_number)) =
                (task_numbers.get(parent), task_numbers.get(child))
            else {
                continue;
            };
            task_store
                .link_tasks(*parent_number, *child_number)
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        for binding in bindings {
            let Some(child_number) = task_numbers.get(&binding.step_key) else {
                continue;
            };

            let translated = match binding.source {
                // The whole translation: a step key becomes the task number it
                // was compiled into.
                BindingSource::Step => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: binding
                        .source_step_key
                        .as_ref()
                        .and_then(|key| task_numbers.get(key))
                        .copied(),
                    source_pointer: binding.source_pointer.clone(),
                    literal_value: None,
                },
                BindingSource::Literal => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: None,
                    source_pointer: None,
                    literal_value: binding.literal_value.clone(),
                },
                BindingSource::RunInput => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: None,
                    source_pointer: None,
                    literal_value: frozen
                        .get(&(binding.step_key.clone(), binding.input_key.clone()))
                        .cloned(),
                },
            };

            task_store
                .set_input_binding(&translated)
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        Ok(())
    }

    /// Undo a launch that failed part-way through emitting.
    ///
    /// Best-effort by necessity — the reason we are here is that writes are
    /// failing. Every failure is logged rather than propagated, because the
    /// caller already has the error that matters and replacing it with a
    /// cleanup error would hide the cause.
    ///
    /// Tasks go first. A leftover `workflow_runs` row is an orphan nobody
    /// reads; a leftover *task* gets picked up and run, which is the outcome
    /// this whole path exists to prevent.
    async fn rollback_run(
        &self,
        task_store: &TaskStore,
        run_id: &str,
        task_numbers: &HashMap<String, i64>,
    ) {
        for (step_key, number) in task_numbers {
            if let Err(error) = task_store.delete(*number).await {
                tracing::error!(
                    %error, run_id, step_key, task_number = number,
                    "failed to remove a task from a rolled-back workflow launch; \
                     it may run on its own"
                );
            }
        }

        if let Err(error) = sqlx::query("DELETE FROM workflow_runs WHERE id = ?")
            .bind(run_id)
            .execute(&self.pool)
            .await
        {
            tracing::error!(%error, run_id, "failed to remove a rolled-back workflow run");
        }
    }

    pub async fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>> {
        let row = sqlx::query(
            "SELECT id, workflow_id, inputs, launched_by, created_at FROM workflow_runs \
             WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch workflow run")?;

        row.map(run_from_row).transpose()
    }

    pub async fn list_runs(&self, workflow_id: &str) -> Result<Vec<WorkflowRun>> {
        let rows = sqlx::query(
            "SELECT id, workflow_id, inputs, launched_by, created_at FROM workflow_runs \
             WHERE workflow_id = ? ORDER BY created_at DESC",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow runs")?;

        rows.into_iter().map(run_from_row).collect()
    }
}

/// Kahn's algorithm. Returns the order, or the steps left over in a cycle.
///
/// The order itself is not used for scheduling — the dependency edges do that —
/// but a workflow that cannot be ordered cannot be run, and finding out at
/// launch is far better than finding out never.
fn topologically_ordered(
    steps: &[WorkflowStep],
    edges: &[(String, String)],
) -> std::result::Result<Vec<String>, LaunchError> {
    let mut indegree: HashMap<&str, usize> = steps
        .iter()
        .map(|step| (step.step_key.as_str(), 0usize))
        .collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();

    for (parent, child) in edges {
        children
            .entry(parent.as_str())
            .or_default()
            .push(child.as_str());
        *indegree.entry(child.as_str()).or_insert(0) += 1;
    }

    let mut queue: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(key, _)| *key)
        .collect();
    queue.sort_unstable();

    let mut ordered = Vec::new();
    while let Some(key) = queue.pop() {
        ordered.push(key.to_string());
        for child in children.get(key).into_iter().flatten() {
            let degree = indegree.entry(child).or_insert(0);
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                queue.push(child);
            }
        }
    }

    if ordered.len() != steps.len() {
        // Whatever never reached indegree zero is in or downstream of a cycle.
        let mut cycle: Vec<String> = indegree
            .iter()
            .filter(|(_, degree)| **degree > 0)
            .map(|(key, _)| (*key).to_string())
            .collect();
        cycle.sort();
        return Err(LaunchError::Cycle { cycle });
    }

    Ok(ordered)
}

fn validate(schema: &Value, value: &Value) -> Vec<String> {
    match jsonschema::validator_for(schema) {
        Ok(validator) => validator
            .iter_errors(value)
            .map(|error| {
                ContractProblem::SchemaViolation {
                    side: ContractSide::Input,
                    path: error.instance_path().to_string(),
                    message: error.to_string(),
                }
                .to_string()
            })
            .collect(),
        Err(error) => vec![format!("schema is not valid JSON Schema: {error}")],
    }
}

fn read_json(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<Value> {
    row.try_get::<Option<String>, _>(column)
        .ok()
        .flatten()
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn workflow_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Workflow> {
    Ok(Workflow {
        id: row.try_get("id").context("failed to read workflow id")?,
        name: row
            .try_get("name")
            .context("failed to read workflow name")?,
        description: row.try_get("description").ok().flatten(),
        input_schema: read_json(&row, "input_schema"),
        created_at: row
            .try_get("created_at")
            .context("failed to read workflow created_at")?,
        updated_at: row
            .try_get("updated_at")
            .context("failed to read workflow updated_at")?,
    })
}

fn step_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkflowStep> {
    let priority: String = row.try_get("priority").unwrap_or_else(|_| "medium".into());
    Ok(WorkflowStep {
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read step workflow_id")?,
        step_key: row.try_get("step_key").context("failed to read step_key")?,
        title: row.try_get("title").context("failed to read step title")?,
        description: row.try_get("description").ok().flatten(),
        assigned_agent_id: row.try_get("assigned_agent_id").ok().flatten(),
        priority: TaskPriority::parse(&priority).unwrap_or(TaskPriority::Medium),
        input_schema: read_json(&row, "input_schema"),
        output_schema: read_json(&row, "output_schema"),
        system_prompt: row.try_get("system_prompt").ok().flatten(),
        repo_id: row.try_get("repo_id").ok().flatten(),
        position: row.try_get("position").unwrap_or(0),
    })
}

fn binding_from_row(row: sqlx::sqlite::SqliteRow) -> Result<StepBinding> {
    let source: String = row
        .try_get("source")
        .context("failed to read binding source")?;
    Ok(StepBinding {
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read binding workflow_id")?,
        step_key: row
            .try_get("step_key")
            .context("failed to read binding step_key")?,
        input_key: row
            .try_get("input_key")
            .context("failed to read binding input_key")?,
        source: BindingSource::parse(&source).unwrap_or(BindingSource::Literal),
        source_step_key: row.try_get("source_step_key").ok().flatten(),
        source_pointer: row.try_get("source_pointer").ok().flatten(),
        literal_value: read_json(&row, "literal_value"),
    })
}

fn run_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkflowRun> {
    Ok(WorkflowRun {
        id: row.try_get("id").context("failed to read run id")?,
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read run workflow_id")?,
        inputs: read_json(&row, "inputs").unwrap_or(Value::Null),
        launched_by: row
            .try_get("launched_by")
            .context("failed to read run launched_by")?,
        created_at: row
            .try_get("created_at")
            .context("failed to read run created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn fixture() -> (WorkflowStore, TaskStore, String) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::tasks::store::create_task_schema(&pool).await;
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("seed sequence");
        create_workflow_schema(&pool).await;

        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool);
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

        (workflows, tasks, workflow.id)
    }

    fn step(workflow_id: &str, key: &str, position: i64) -> WorkflowStep {
        WorkflowStep {
            workflow_id: workflow_id.to_string(),
            step_key: key.to_string(),
            title: format!("step {key}"),
            description: None,
            assigned_agent_id: None,
            priority: TaskPriority::Medium,
            input_schema: None,
            output_schema: None,
            system_prompt: None,
            repo_id: None,
            position,
        }
    }

    /// The headline case: one input, a chain of steps, everything wired.
    #[tokio::test]
    async fn a_launch_compiles_a_template_into_a_runnable_graph() {
        let (workflows, tasks, id) = fixture().await;

        for (index, key) in ["build", "test", "deploy"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("put step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");
        workflows
            .link_steps(&id, "test", "deploy")
            .await
            .expect("link");

        // The entry step reads straight from the run's launch payload.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "build".into(),
                input_key: "tag".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind run input");
        // A later step reads an earlier step's output, by name.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "deploy".into(),
                input_key: "artifact".into(),
                source: BindingSource::Step,
                source_step_key: Some("build".into()),
                source_pointer: Some("/artifact".into()),
                literal_value: None,
            })
            .await
            .expect("bind step output");

        let launched = workflows
            .launch(
                &tasks,
                &id,
                &serde_json::json!({"tag": "v2.0.0"}),
                "agent-1",
            )
            .await
            .expect("launch should succeed");

        assert_eq!(launched.task_numbers.len(), 3);

        // Every step lands in backlog — the sweep decides what is eligible,
        // not the instantiator.
        for number in launched.task_numbers.values() {
            let task = tasks
                .get_by_number(*number)
                .await
                .expect("fetch")
                .expect("exists");
            assert_eq!(task.status, TaskStatus::Backlog);
            assert_eq!(
                task.workflow_run_id.as_deref(),
                Some(launched.run.id.as_str())
            );
        }

        // Edges were translated from step keys to task numbers.
        let build = launched.task_numbers["build"];
        let test = launched.task_numbers["test"];
        let deploy = launched.task_numbers["deploy"];
        assert_eq!(
            tasks.list_parents(test).await.expect("parents"),
            vec![build]
        );
        assert_eq!(
            tasks.list_parents(deploy).await.expect("parents"),
            vec![test]
        );

        // The step binding points at the compiled task number, not a name.
        let deploy_bindings = tasks.list_input_bindings(deploy).await.expect("bindings");
        assert_eq!(deploy_bindings.len(), 1);
        assert_eq!(deploy_bindings[0].source_task_number, Some(build));
        assert_eq!(
            deploy_bindings[0].source_pointer.as_deref(),
            Some("/artifact")
        );

        // The run input was frozen into a literal at launch.
        let build_bindings = tasks.list_input_bindings(build).await.expect("bindings");
        assert_eq!(
            build_bindings[0].literal_value,
            Some(serde_json::json!("v2.0.0"))
        );
        assert_eq!(
            build_bindings[0].source_task_number, None,
            "a frozen run input must not depend on anything at run time"
        );
    }

    /// The entry step must be immediately runnable: no parents, and its inputs
    /// already resolve. If this fails the pipeline never starts.
    #[tokio::test]
    async fn the_entry_step_is_promotable_immediately_after_launch() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        workflows
            .put_step(&step(&id, "deploy", 1))
            .await
            .expect("step");
        workflows
            .link_steps(&id, "build", "deploy")
            .await
            .expect("link");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "build".into(),
                input_key: "tag".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![launched.task_numbers["build"]],
            "the entry step should be the only thing the sweep promotes"
        );
        assert!(sweep.stalled.is_empty(), "a fresh launch must not stall");
    }

    #[tokio::test]
    async fn a_launch_input_that_misses_the_schema_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": 42}), "agent-1")
            .await
            .expect_err("a number where a string was declared must be refused");
        assert!(
            matches!(error, LaunchError::InvalidInput { .. }),
            "{error:?}"
        );

        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty(),
            "a refused launch must not leave tasks behind"
        );
    }

    /// A cycle would deadlock silently at run time — every step waiting on a
    /// parent that is itself waiting. Refuse at launch and name the steps.
    #[tokio::test]
    async fn a_cyclic_template_is_refused_before_anything_is_written() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["a", "b", "c"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows.link_steps(&id, "a", "b").await.expect("link");
        workflows.link_steps(&id, "b", "c").await.expect("link");
        workflows.link_steps(&id, "c", "a").await.expect("link");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a cycle must be refused");
        match error {
            LaunchError::Cycle { cycle } => assert_eq!(cycle, vec!["a", "b", "c"]),
            other => panic!("expected Cycle, got {other:?}"),
        }
        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_binding_naming_an_unknown_step_is_refused_with_the_name() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "deploy", 0))
            .await
            .expect("step");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "deploy".into(),
                input_key: "artifact".into(),
                source: BindingSource::Step,
                source_step_key: Some("buidl".into()), // typo on purpose
                source_pointer: Some("/artifact".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("an unknown step reference must be refused");
        match error {
            LaunchError::UnknownStepReference { missing, .. } => assert_eq!(missing, "buidl"),
            other => panic!("expected UnknownStepReference, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_run_input_pointer_that_misses_is_refused_at_launch() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "build".into(),
                input_key: "digest".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/image/digest".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a pointer the input does not contain must be refused");
        assert!(
            matches!(error, LaunchError::MissingRunInput { .. }),
            "{error:?}"
        );
    }

    /// Per-step instructions have to reach the task, or the field is decoration.
    #[tokio::test]
    async fn a_step_stamps_its_system_prompt_onto_the_task_it_becomes() {
        let (workflows, tasks, id) = fixture().await;
        let mut build = step(&id, "build", 0);
        build.system_prompt = Some("Answer in British English.".into());
        workflows.put_step(&build).await.expect("step");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");

        let task = tasks
            .get_by_number(launched.task_numbers["build"])
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            task.system_prompt.as_deref(),
            Some("Answer in British English."),
        );
    }

    /// Deleting a step must not leave an edge or binding pointing at it.
    ///
    /// A dangling reference fails *every* future launch, and it is invisible in
    /// a step-oriented editor — the template would be permanently unlaunchable
    /// with nothing on screen to explain why.
    #[tokio::test]
    async fn deleting_a_step_takes_its_edges_and_bindings_with_it() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["build", "test", "deploy"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");
        workflows
            .link_steps(&id, "test", "deploy")
            .await
            .expect("link");
        // deploy reads from test — a reference *into* the step being removed.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "deploy".into(),
                input_key: "report".into(),
                source: BindingSource::Step,
                source_step_key: Some("test".into()),
                source_pointer: Some("/report".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        assert!(workflows.delete_step(&id, "test").await.expect("delete"));

        assert!(
            workflows.list_edges(&id).await.expect("edges").is_empty(),
            "both edges touched the removed step"
        );
        assert!(
            workflows
                .list_bindings(&id)
                .await
                .expect("bindings")
                .is_empty(),
            "the binding read from the removed step"
        );

        // The real assertion: the template still launches.
        workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("a workflow with a step removed must still launch");
    }

    /// A launch that dies part-way through must not leave runnable tasks.
    ///
    /// Validation catches bad templates, but emitting spans two stores and can
    /// fail on its own. Whatever is left behind gets *picked up and run*, which
    /// is worse than the failure itself.
    #[tokio::test]
    async fn a_launch_that_fails_while_emitting_leaves_nothing_runnable() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["build", "test"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");

        // Emit half a graph by hand, then roll it back — the same call the
        // launch path makes when a write fails under it.
        let run_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO workflow_runs (id, workflow_id, inputs, launched_by) VALUES (?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(&id)
        .bind("{}")
        .bind("agent-1")
        .execute(workflows.pool())
        .await
        .expect("insert run");

        let mut emitted = HashMap::new();
        workflows
            .emit_graph(
                &tasks,
                &run_id,
                &workflows.list_steps(&id).await.expect("steps"),
                &workflows.list_edges(&id).await.expect("edges"),
                &[],
                &HashMap::new(),
                "agent-1",
                &mut emitted,
            )
            .await
            .expect("emit");
        assert_eq!(emitted.len(), 2);

        workflows.rollback_run(&tasks, &run_id, &emitted).await;

        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty(),
            "a rolled-back launch must leave no task the sweep could promote"
        );
        assert!(
            workflows.get_run(&run_id).await.expect("run").is_none(),
            "the run row goes too"
        );
    }

    /// A step that says it needs an input, with nothing wired to it, is a
    /// mistake that is fully knowable at launch. Letting it through means
    /// finding out one step into a pipeline someone thought was fine.
    #[tokio::test]
    async fn a_required_input_with_no_binding_is_refused_at_launch() {
        let (workflows, tasks, id) = fixture().await;
        let mut deploy = step(&id, "deploy", 0);
        deploy.input_schema = Some(serde_json::json!({
            "type": "object",
            "required": ["artifact"],
            "properties": {"artifact": {"type": "string"}},
        }));
        workflows.put_step(&deploy).await.expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a declared, unbound input must be refused");
        match error {
            LaunchError::UnboundRequiredInput {
                step_key,
                input_key,
            } => {
                assert_eq!(step_key, "deploy");
                assert_eq!(input_key, "artifact");
            }
            other => panic!("expected UnboundRequiredInput, got {other:?}"),
        }
    }

    /// An optional input with no binding is a default, not an omission.
    #[tokio::test]
    async fn an_optional_input_needs_no_binding() {
        let (workflows, tasks, id) = fixture().await;
        let mut deploy = step(&id, "deploy", 0);
        deploy.input_schema = Some(serde_json::json!({
            "type": "object",
            "properties": {"artifact": {"type": "string"}},
        }));
        workflows.put_step(&deploy).await.expect("step");

        workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("an optional input is not a missing one");
    }

    /// A cycle must be refused when the edge is *saved*, not only at launch.
    /// A template that accepts one and then refuses to run is a trap.
    #[tokio::test]
    async fn an_edge_that_would_close_a_loop_is_detectable_before_it_is_saved() {
        let (workflows, _tasks, id) = fixture().await;
        for (index, key) in ["a", "b", "c"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows.link_steps(&id, "a", "b").await.expect("link");
        workflows.link_steps(&id, "b", "c").await.expect("link");

        let cycle = workflows
            .cycle_from_edge(&id, "c", "a")
            .await
            .expect("check")
            .expect("c -> a closes the ring a -> b -> c");
        assert_eq!(cycle, vec!["a", "b", "c"], "the path should name the loop");

        assert!(
            workflows
                .cycle_from_edge(&id, "a", "c")
                .await
                .expect("check")
                .is_none(),
            "a shortcut along the existing direction is not a cycle"
        );
        assert!(
            workflows
                .cycle_from_edge(&id, "a", "a")
                .await
                .expect("check")
                .is_some(),
            "a step cannot wait for itself"
        );
    }

    /// Steps added without an explicit position must not all land on zero.
    #[tokio::test]
    async fn positions_do_not_collide_when_left_unspecified() {
        let (workflows, _tasks, id) = fixture().await;
        assert_eq!(workflows.next_step_position(&id).await.expect("next"), 0);

        workflows.put_step(&step(&id, "a", 0)).await.expect("step");
        assert_eq!(workflows.next_step_position(&id).await.expect("next"), 1);

        workflows.put_step(&step(&id, "b", 1)).await.expect("step");
        assert_eq!(workflows.next_step_position(&id).await.expect("next"), 2);
    }

    /// Two launches must not share tasks. A run is the unit of "this happened".
    #[tokio::test]
    async fn two_launches_produce_independent_graphs() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");

        let first = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("first launch");
        let second = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v2"}), "agent-1")
            .await
            .expect("second launch");

        assert_ne!(first.run.id, second.run.id);
        assert_ne!(first.task_numbers["build"], second.task_numbers["build"]);
    }

    #[cfg(test)]
    async fn create_workflow_schema(pool: &SqlitePool) {
        for statement in [
            "CREATE TABLE workflows (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL UNIQUE, \
             description TEXT, input_schema TEXT, \
             created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')), \
             updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')))",
            "CREATE TABLE workflow_steps (workflow_id TEXT NOT NULL, step_key TEXT NOT NULL, \
             title TEXT NOT NULL, description TEXT, assigned_agent_id TEXT, \
             priority TEXT NOT NULL DEFAULT 'medium', input_schema TEXT, output_schema TEXT, \
             system_prompt TEXT, repo_id TEXT, position INTEGER NOT NULL DEFAULT 0, \
             PRIMARY KEY (workflow_id, step_key))",
            "CREATE TABLE workflow_step_edges (workflow_id TEXT NOT NULL, \
             parent_step_key TEXT NOT NULL, child_step_key TEXT NOT NULL, \
             PRIMARY KEY (workflow_id, parent_step_key, child_step_key))",
            "CREATE TABLE workflow_step_bindings (workflow_id TEXT NOT NULL, \
             step_key TEXT NOT NULL, input_key TEXT NOT NULL, source TEXT NOT NULL, \
             source_step_key TEXT, source_pointer TEXT, literal_value TEXT, \
             PRIMARY KEY (workflow_id, step_key, input_key))",
            "CREATE TABLE workflow_runs (id TEXT PRIMARY KEY NOT NULL, workflow_id TEXT NOT NULL, \
             inputs TEXT NOT NULL, launched_by TEXT NOT NULL, \
             created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')))",
        ] {
            sqlx::query(statement)
                .execute(pool)
                .await
                .expect("workflow schema should be created");
        }
    }
}
