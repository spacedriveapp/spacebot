import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faHourglassHalf,
	faScissors,
	faTerminal,
} from "@fortawesome/free-solid-svg-icons";
import type {TaskItem} from "@/api/client";
import {
	EXPECT_EXIT_UNSET,
	OMISSION_MARKER,
	expectExitCodeMeaning,
	formatDuration,
	formatTimeout,
	readCommandOutputs,
	type CommandOutputs,
} from "./commands";

/**
 * What a command step actually did.
 *
 * A JSON blob is the wrong shape for this. Three of the six fields are numbers
 * a reader wants at a glance and two are logs they want to *read*, and
 * `JSON.stringify` renders a build log as one line with `\n` in it — which is
 * the single most common thing anybody opens this panel to look at.
 *
 * The exit code leads because every downstream decision reads it, and it is
 * coloured by what it *means* rather than by whether it is zero: with no
 * declared expectation a non-zero code is a successful step whose answer is
 * "there are problems", and painting that red would teach people that a working
 * lint step is a broken one.
 */
export function CommandResult({
	task,
	outputs,
}: {
	task: TaskItem;
	outputs: CommandOutputs;
}) {
	const expected = task.expect_exit_code ?? null;
	const met = expected == null || outputs.exit_code === expected;
	// Three states, not two. `unmet` is a task failure; `data` is the default
	// and the case the whole feature exists for.
	const verdict = outputs.exit_code === 0 ? "clean" : met ? "data" : "unmet";

	return (
		<div className="mt-1.5">
			<h4 className="mb-1 flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-wide text-ink-faint">
				<FontAwesomeIcon icon={faTerminal} className="text-[9px]" />
				Command
			</h4>

			{task.command && (
				<pre className="mb-1.5 overflow-x-auto rounded border border-app-line bg-app-box/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed text-ink-dull">
					{task.command}
				</pre>
			)}

			<div className="mb-2 flex flex-wrap items-center gap-1.5">
				<span
					className={`inline-flex items-center gap-1.5 rounded border px-2 py-0.5 font-mono text-xs ${
						verdict === "clean"
							? "border-status-success/50 bg-status-success/10 text-status-success"
							: verdict === "data"
								? "border-status-info/50 bg-status-info/10 text-status-info"
								: "border-status-error/50 bg-status-error/10 text-status-error"
					}`}
					title={
						expected == null
							? EXPECT_EXIT_UNSET
							: expectExitCodeMeaning(expected)
					}
				>
					exit {outputs.exit_code}
				</span>

				{outputs.duration_ms != null && (
					<span
						className="rounded border border-app-line bg-app-box/50 px-1.5 py-0.5 font-mono text-[10px] text-ink-dull"
						title={`${outputs.duration_ms}ms of wall clock${
							task.command_timeout_secs != null
								? `, against a ${task.command_timeout_secs}s timeout`
								: ""
						}`}
					>
						{formatDuration(outputs.duration_ms)}
					</span>
				)}

				{task.command_timeout_secs != null && (
					<span
						className="inline-flex items-center gap-1 rounded border border-app-line bg-app-box/50 px-1.5 py-0.5 text-[10px] text-ink-faint"
						title={`The step's hard timeout. A command killed by it never reported, so the task fails rather than producing an exit code.`}
					>
						<FontAwesomeIcon icon={faHourglassHalf} className="text-[8px]" />
						{formatTimeout(task.command_timeout_secs)} limit
					</span>
				)}
			</div>

			{/* The rule, in words, wherever the number is shown. Somebody reading a
			    non-zero exit on a task marked done needs to know that is the design
			    and not a bug. */}
			<p
				className={`mb-2 rounded border px-2 py-1 text-[10px] ${
					verdict === "unmet"
						? "border-status-error/30 bg-status-error/5 text-status-error"
						: "border-app-line bg-app-box/40 text-ink-faint"
				}`}
			>
				{verdict === "unmet"
					? `This step requires exit ${expected}. The command ran, so its output is here — it is the result that is wrong, not the run.`
					: expected != null
						? `This step requires exit ${expected}, and got it.`
						: "The command ran, so the step succeeded. The exit code is data for whatever reads it next."}
			</p>

			<LogBlock
				label="stdout"
				text={outputs.stdout}
				truncated={outputs.stdout_truncated}
			/>
			<LogBlock
				label="stderr"
				text={outputs.stderr}
				truncated={outputs.stderr_truncated}
				muted
			/>
		</div>
	);
}

/**
 * One stream, as a log rather than as a JSON string.
 *
 * Truncation is said **twice**, and that is the point. A capped log that looks
 * complete is how a person — or a fix step reading it as input — draws a
 * confident conclusion from half the evidence, so there is a badge on the
 * header for someone scanning and a drawn seam at the omission for someone
 * reading. The seam is where the bytes went missing, which is the only place it
 * can be noticed by someone who started at the top and kept going.
 */
function LogBlock({
	label,
	text,
	truncated,
	muted,
}: {
	label: string;
	text: string;
	truncated: boolean;
	muted?: boolean;
}) {
	const empty = text === "";
	return (
		<div className="mt-1.5">
			<h4 className="mb-0.5 flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-wide text-ink-faint">
				{label}
				{truncated && (
					<span
						className="inline-flex items-center gap-1 rounded-full border border-status-warning/50 bg-status-warning/10 px-1.5 normal-case tracking-normal text-status-warning"
						title="This stream was capped. The head and the tail are kept and the middle is gone — read the marker in the log for how much."
					>
						<FontAwesomeIcon icon={faScissors} className="text-[8px]" />
						truncated
					</span>
				)}
				{empty && (
					<span className="normal-case tracking-normal text-ink-faint/70">
						empty
					</span>
				)}
			</h4>
			{!empty && (
				<div
					className={`max-h-56 overflow-auto rounded border bg-app-box/40 font-mono text-[11px] leading-relaxed ${
						truncated ? "border-status-warning/40" : "border-app-line"
					} ${muted ? "text-ink-faint" : "text-ink-dull"}`}
				>
					{splitAtOmission(text).map((chunk, index) =>
						chunk.omission ? (
							<div
								key={index}
								className="my-1 flex items-center gap-2 border-y border-status-warning/40 bg-status-warning/10 px-2 py-1 text-[10px] text-status-warning"
							>
								<FontAwesomeIcon icon={faScissors} className="shrink-0 text-[9px]" />
								<span className="min-w-0">
									{formatBytes(chunk.omitted)} missing here — the head and tail of{" "}
									{formatBytes(chunk.total)} were kept, the middle was not. What
									you are reading is not the whole log.
								</span>
							</div>
						) : (
							<pre
								key={index}
								className="whitespace-pre-wrap break-words px-2 py-1.5"
							>
								{chunk.text}
							</pre>
						),
					)}
				</div>
			)}
		</div>
	);
}

type LogChunk =
	| {omission: false; text: string}
	| {omission: true; omitted: number; total: number};

/**
 * Split a capped stream at the marker `cap_output` wrote into it.
 *
 * Matched on its exact shape rather than searched for loosely: a build log that
 * happens to contain the word "omitted" must not be drawn with a seam through
 * it, because a fake truncation notice is worse than none.
 */
function splitAtOmission(text: string): LogChunk[] {
	const lines = text.split("\n");
	const chunks: LogChunk[] = [];
	let buffer: string[] = [];

	const flush = () => {
		if (buffer.length === 0) return;
		// The marker is written surrounded by blank lines; they belong to the
		// seam, not to the log either side of it.
		while (buffer.length > 0 && buffer[buffer.length - 1] === "") buffer.pop();
		while (buffer.length > 0 && buffer[0] === "") buffer.shift();
		if (buffer.length > 0) chunks.push({omission: false, text: buffer.join("\n")});
		buffer = [];
	};

	for (const line of lines) {
		const match = OMISSION_MARKER.exec(line);
		if (match) {
			flush();
			chunks.push({
				omission: true,
				omitted: Number(match[1]),
				total: Number(match[2]),
			});
			continue;
		}
		buffer.push(line);
	}
	flush();
	return chunks.length > 0 ? chunks : [{omission: false, text}];
}

function formatBytes(bytes: number): string {
	if (bytes < 1024) return `${bytes} bytes`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The command outputs on a task, or `null` when they are not a command's. */
export function commandOutputsOf(task: TaskItem): CommandOutputs | null {
	if (task.kind !== "command") return null;
	return readCommandOutputs(task.outputs);
}
