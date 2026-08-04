import type {StepKind, WorkflowStep, WorktreeMode} from "@/api/client";

/**
 * The vocabulary of command steps and provisioned checkouts, in one place.
 *
 * The step editor, the canvas node and the run view all have to say the same
 * things about the same two features — most importantly what `expect_exit_code`
 * does, which is the field this feature can be got backwards. One copy of that
 * sentence rather than three is the difference between a form that explains
 * itself everywhere and one that explains itself where somebody remembered.
 */

/**
 * The ceiling the server enforces on `command_timeout_secs`.
 *
 * Mirrors `MAX_COMMAND_TIMEOUT_SECS` in `agent/command_step.rs`. Checked here
 * so the refusal arrives while the number is still being typed, and checked
 * there because a bundle ships separately from the binary.
 */
export const MAX_COMMAND_TIMEOUT_SECS = 1800;

/** `kind` is optional on the wire, and absent means every step written before command steps existed. */
export function stepKindOf(step: {kind?: StepKind | null}): StepKind {
	return step.kind === "command" ? "command" : "agent";
}

export function isCommandStep(step: {kind?: StepKind | null}): boolean {
	return stepKindOf(step) === "command";
}

export const STEP_KIND_LABEL: Record<StepKind, string> = {
	agent: "Agent",
	command: "Command",
	decision: "Decision",
};

export const STEP_KIND_HINT: Record<StepKind, string> = {
	agent: "A worker with a full tool loop reads the brief and decides what to do.",
	command:
		"A process runs, exits, and its exit code and output become the step's outputs. Nothing is asked of a model.",
	decision:
		"A person is asked a question and their answer becomes the step's outputs. Nothing else can produce them, which is what makes the answer known to be a person's.",
};

/**
 * What `expect_exit_code` means, said as a rule rather than as a field name.
 *
 * The default is the load-bearing decision of the whole feature and it is the
 * counter-intuitive direction: a command that ran and reported a problem is a
 * step that **succeeded**, with the code as data. Getting it backwards makes a
 * lint step charge its failure budget for working correctly, and park itself
 * before the fix loop has run twice.
 */
export const EXPECT_EXIT_UNSET =
	"Any exit code is a success. The command ran, so the step worked; the code is data for whatever reads it next.";

export function expectExitCodeMeaning(expected: number): string {
	return `Only exit ${expected} is a success. Every other code fails the task and charges the failure budget.`;
}

/** Which task-level event a command produced, in the words the design doc uses. */
export const RAN_VERSUS_FAILED =
	"A command that could not run at all — missing binary, timeout, killed — is a task failure either way.";

export const WORKTREE_MODE_LABEL: Record<WorktreeMode, string> = {
	inherit: "Inherit the binding",
	per_run: "A checkout of its own",
	per_branch: "One checkout per branch",
};

export const WORKTREE_MODE_HINT: Record<WorktreeMode, string> = {
	inherit:
		"Runs wherever the task binding already points — the repo, or a worktree somebody made. Nothing is created.",
	per_run:
		"One worktree, created at launch and used by this step alone. Nothing else in the run touches it.",
	per_branch:
		"One worktree per fan-out branch, created as the fan-out expands. Parallel branches of one repo stop trampling each other.",
};

export function worktreeModeOf(step: {
	worktree_mode?: WorktreeMode | null;
}): WorktreeMode {
	const mode = step.worktree_mode;
	return mode === "per_run" || mode === "per_branch" ? mode : "inherit";
}

/** Whether the mode causes a checkout to be created, rather than reusing one. */
export function provisionsWorktree(mode: WorktreeMode): boolean {
	return mode === "per_run" || mode === "per_branch";
}

/** A step is a fan-out exactly when it names a step to iterate. */
export function isFanOut(step: Pick<WorkflowStep, "for_each_step_key">): boolean {
	return !!step.for_each_step_key;
}

/**
 * The outputs a command step produces.
 *
 * Not a declared `output_schema` — it is implicit, and bindings, gates,
 * `loop_until` and conditions all read it with the pointers they already use.
 */
export interface CommandOutputs {
	exit_code: number;
	stdout: string;
	stderr: string;
	duration_ms: number | null;
	stdout_truncated: boolean;
	stderr_truncated: boolean;
}

/**
 * Read a task's outputs as a command result, or `null` if they are not one.
 *
 * Shape-checked rather than trusted from `task.kind`, because a command task
 * that never ran has no outputs at all and an agent task's outputs must never
 * be rendered as an exit code. `exit_code` is the one field that has to be
 * there: without it there is nothing to be prominent about.
 */
export function readCommandOutputs(value: unknown): CommandOutputs | null {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return null;
	}
	const record = value as Record<string, unknown>;
	if (typeof record.exit_code !== "number") return null;
	return {
		exit_code: record.exit_code,
		stdout: typeof record.stdout === "string" ? record.stdout : "",
		stderr: typeof record.stderr === "string" ? record.stderr : "",
		duration_ms:
			typeof record.duration_ms === "number" ? record.duration_ms : null,
		stdout_truncated: record.stdout_truncated === true,
		stderr_truncated: record.stderr_truncated === true,
	};
}

/** `840` → `0.84s`, `99` → `99ms`, `95000` → `1m 35s`. */
export function formatDuration(ms: number): string {
	if (ms < 1000) return `${ms}ms`;
	if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)}s`;
	const minutes = Math.floor(ms / 60_000);
	const seconds = Math.round((ms % 60_000) / 1000);
	return `${minutes}m ${seconds}s`;
}

/** `60` → `60s`, `1800` → `30m`. */
export function formatTimeout(secs: number): string {
	if (secs % 60 === 0 && secs >= 60) {
		const minutes = secs / 60;
		return `${minutes}m`;
	}
	return `${secs}s`;
}

/**
 * The marker `cap_output` writes into the middle of a capped stream.
 *
 * Head and tail are kept in preference to the head alone — the useful part of a
 * failing build log is usually at the end — and the byte count of the gap goes
 * in the text. Matched here so the log viewer can draw it as a seam rather than
 * letting it scroll past as one more line of output.
 */
export const OMISSION_MARKER =
	/^\[\.\.\. (\d+) bytes omitted from the middle of (\d+) total; head and tail kept \.\.\.\]$/;

/**
 * Whether a `capability` block is the containment refusal.
 *
 * A command step refuses to run when the sandbox is requested-but-inert. That
 * is a configuration problem with a named fix, not a failure of the pipeline,
 * and rendering it in the same red as a crashed task sends somebody looking at
 * the wrong thing.
 */
export function isContainmentRefusal(reason: string | null | undefined): boolean {
	if (!reason) return false;
	return (
		reason.includes("sandbox.mode") && reason.includes("no backend was detected")
	);
}
