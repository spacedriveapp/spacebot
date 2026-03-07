import { useState, useMemo } from "react";
import { cx } from "@/ui/utils";
import type { ActionContent, TranscriptStep } from "@/api/client";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type ToolCallStatus = "running" | "completed" | "error";

export interface ToolCallPair {
	/** The call_id linking tool_call to tool_result */
	id: string;
	/** Tool name (e.g. "browser_navigate", "shell") */
	name: string;
	/** Raw JSON args string from the tool_call */
	argsRaw: string;
	/** Parsed args object (null if parse failed) */
	args: Record<string, unknown> | null;
	/** Raw result text from tool_result (null if still running) */
	resultRaw: string | null;
	/** Parsed result object (null if not JSON or still running) */
	result: Record<string, unknown> | null;
	/** Current state */
	status: ToolCallStatus;
}

// ---------------------------------------------------------------------------
// Transcript → ToolCallPair[] pairing
// ---------------------------------------------------------------------------

/**
 * Walk a flat TranscriptStep[] and pair each tool_call with its tool_result
 * via call_id matching, plus emit standalone text steps. Returns an ordered
 * list of renderable items: text blocks and paired tool calls.
 */
export type TranscriptItem =
	| { kind: "text"; text: string }
	| { kind: "tool"; pair: ToolCallPair };

export function pairTranscriptSteps(steps: TranscriptStep[]): TranscriptItem[] {
	const items: TranscriptItem[] = [];
	const resultsById = new Map<string, { name: string; text: string }>();

	// First pass: index all tool_result steps by call_id
	for (const step of steps) {
		if (step.type === "tool_result") {
			resultsById.set(step.call_id, { name: step.name, text: step.text });
		}
	}

	// Second pass: emit items in order
	for (const step of steps) {
		if (step.type === "action") {
			for (const content of step.content) {
				if (content.type === "text") {
					items.push({ kind: "text", text: content.text });
				} else if (content.type === "tool_call") {
					const result = resultsById.get(content.id);
					const parsedArgs = tryParseJson(content.args);
					const parsedResult = result ? tryParseJson(result.text) : null;

					// Detect error: result text starts with "Error" or contains error indicators
					const isError = result
						? isErrorResult(result.text, parsedResult)
						: false;

					items.push({
						kind: "tool",
						pair: {
							id: content.id,
							name: content.name,
							argsRaw: content.args,
							args: parsedArgs,
							resultRaw: result?.text ?? null,
							result: parsedResult,
							status: result
								? isError
									? "error"
									: "completed"
								: "running",
						},
					});
				}
			}
		}
		// tool_result steps are consumed by the pairing above, not rendered standalone
	}

	return items;
}

function tryParseJson(text: string): Record<string, unknown> | null {
	if (!text || text.trim().length === 0) return null;
	try {
		const parsed = JSON.parse(text);
		if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
			return parsed as Record<string, unknown>;
		}
		return null;
	} catch {
		return null;
	}
}

function isErrorResult(
	text: string,
	parsed: Record<string, unknown> | null,
): boolean {
	if (parsed?.error) return true;
	if (parsed?.status === "error") return true;
	const lower = text.toLowerCase();
	return (
		lower.startsWith("error:") ||
		lower.startsWith("error -") ||
		lower.startsWith("failed:")
	);
}

// ---------------------------------------------------------------------------
// Tool-specific rendering
// ---------------------------------------------------------------------------

interface ToolRenderer {
	/** One-line summary shown in the collapsed header (after tool name) */
	summary(pair: ToolCallPair): string | null;
	/** Custom args display (return null to use default JSON) */
	argsView?(pair: ToolCallPair): React.ReactNode | null;
	/** Custom result display (return null to use default text) */
	resultView?(pair: ToolCallPair): React.ReactNode | null;
}

const toolRenderers: Record<string, ToolRenderer> = {
	browser_launch: {
		summary(pair) {
			const headless = pair.args?.headless;
			return headless === false ? "Launch browser (visible)" : "Launch browser";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <ResultLine text="Browser session started" />;
		},
	},

	browser_navigate: {
		summary(pair) {
			const url = pair.args?.url;
			return url ? truncate(String(url), 60) : null;
		},
		resultView(pair) {
			if (!pair.result) return <ResultText text={pair.resultRaw} />;
			const title = pair.result.title as string | undefined;
			const url = pair.result.url as string | undefined;
			return (
				<div className="flex flex-col gap-1 px-3 py-2">
					{title && (
						<p className="text-tiny text-ink-dull">
							<span className="text-ink-faint">Title: </span>
							{title}
						</p>
					)}
					{url && (
						<p className="font-mono text-tiny text-ink-faint">
							{truncate(url, 80)}
						</p>
					)}
				</div>
			);
		},
	},

	browser_snapshot: {
		summary(pair) {
			if (!pair.resultRaw) return "Taking snapshot...";
			// Count elements: look for "[N]" patterns in the ARIA snapshot
			const matches = pair.resultRaw.match(/\[\d+\]/g);
			const count = matches?.length ?? 0;
			return count > 0
				? `${count} interactive element${count !== 1 ? "s" : ""}`
				: "Page snapshot";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			// ARIA tree snapshots are YAML-like text — render in a scrollable pre
			// but cap the default view
			return <CollapsiblePre text={pair.resultRaw} maxLines={20} />;
		},
	},

	browser_click: {
		summary(pair) {
			const index = pair.args?.index;
			return index !== undefined ? `Click element [${index}]` : "Click";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <ResultLine text={pair.resultRaw} />;
		},
	},

	browser_type: {
		summary(pair) {
			const index = pair.args?.index;
			const hasSecret = pair.args?.secret !== undefined;
			const text = pair.args?.text;
			if (hasSecret) {
				return index !== undefined
					? `Type secret into [${index}]`
					: "Type secret";
			}
			if (text) {
				const display = truncate(String(text), 30);
				return index !== undefined
					? `Type "${display}" into [${index}]`
					: `Type "${display}"`;
			}
			return index !== undefined ? `Type into [${index}]` : "Type";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <ResultLine text={pair.resultRaw} />;
		},
	},

	browser_press_key: {
		summary(pair) {
			const key = pair.args?.key;
			return key ? `Press ${key}` : "Press key";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <ResultLine text={pair.resultRaw} />;
		},
	},

	browser_screenshot: {
		summary() {
			return "Capture screenshot";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			// Screenshot results may contain base64 image data or a path
			if (pair.result?.base64) {
				const mimeType =
					(pair.result.mime_type as string) ?? "image/png";
				return (
					<div className="px-3 py-2">
						<img
							src={`data:${mimeType};base64,${pair.result.base64}`}
							alt="Browser screenshot"
							className="max-h-60 rounded border border-app-line/30 object-contain"
						/>
					</div>
				);
			}
			return <ResultLine text={truncate(pair.resultRaw, 100)} />;
		},
	},

	browser_evaluate: {
		summary(pair) {
			const expression = pair.args?.expression;
			return expression
				? truncate(String(expression), 50)
				: "Evaluate JS";
		},
		argsView(pair) {
			const expression = pair.args?.expression;
			if (!expression) return null;
			return (
				<div className="border-b border-app-line/20 px-3 py-2">
					<p className="mb-1 text-tiny font-medium text-ink-faint">
						JavaScript
					</p>
					<pre className="max-h-40 overflow-auto font-mono text-tiny text-ink-dull">
						{String(expression)}
					</pre>
				</div>
			);
		},
	},

	browser_tab_open: {
		summary(pair) {
			const url = pair.args?.url;
			return url ? `Open tab: ${truncate(String(url), 50)}` : "Open new tab";
		},
	},

	browser_tab_list: {
		summary() {
			return "List tabs";
		},
	},

	browser_tab_close: {
		summary(pair) {
			const tabId = pair.args?.tab_id;
			return tabId !== undefined
				? `Close tab ${tabId}`
				: "Close tab";
		},
	},

	browser_close: {
		summary() {
			return "Close browser";
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <ResultLine text="Browser session closed" />;
		},
	},

	shell: {
		summary(pair) {
			const command = pair.args?.command;
			return command ? truncate(String(command), 60) : null;
		},
		argsView(pair) {
			const command = pair.args?.command;
			if (!command) return null;
			return (
				<div className="border-b border-app-line/20 px-3 py-2">
					<pre className="max-h-40 overflow-auto font-mono text-tiny text-ink-dull">
						<span className="select-none text-ink-faint">$ </span>
						{String(command)}
					</pre>
				</div>
			);
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <CollapsiblePre text={pair.resultRaw} maxLines={30} />;
		},
	},

	file: {
		summary(pair) {
			const path = pair.args?.path;
			const action = pair.args?.action;
			if (action && path) {
				return `${action}: ${truncate(String(path), 50)}`;
			}
			return path ? truncate(String(path), 60) : null;
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <CollapsiblePre text={pair.resultRaw} maxLines={30} />;
		},
	},

	exec: {
		summary(pair) {
			const command = pair.args?.command;
			return command ? truncate(String(command), 60) : null;
		},
		resultView(pair) {
			if (!pair.resultRaw) return null;
			return <CollapsiblePre text={pair.resultRaw} maxLines={30} />;
		},
	},

	set_status: {
		summary(pair) {
			const kind = pair.args?.kind;
			const message = pair.args?.message;
			if (kind === "outcome") {
				return message
					? `Outcome: ${truncate(String(message), 50)}`
					: "Outcome set";
			}
			return message ? truncate(String(message), 60) : null;
		},
		resultView() {
			// set_status results are not interesting — just "ok"
			return null;
		},
	},
};

// Default renderer for tools without a specific renderer
const defaultRenderer: ToolRenderer = {
	summary(pair) {
		if (!pair.argsRaw || pair.argsRaw === "{}") return null;
		return truncate(pair.argsRaw, 60);
	},
};

function getRenderer(name: string): ToolRenderer {
	return toolRenderers[name] ?? defaultRenderer;
}

// ---------------------------------------------------------------------------
// Shared sub-components
// ---------------------------------------------------------------------------

function ResultLine({ text }: { text: string }) {
	return (
		<p className="px-3 py-2 text-tiny text-ink-dull">{text}</p>
	);
}

function ResultText({ text }: { text: string | null }) {
	if (!text) return null;
	return (
		<pre className="max-h-60 overflow-auto whitespace-pre-wrap px-3 py-2 font-mono text-tiny text-ink-dull">
			{text}
		</pre>
	);
}

function CollapsiblePre({
	text,
	maxLines = 20,
}: {
	text: string;
	maxLines?: number;
}) {
	const [expanded, setExpanded] = useState(false);
	const lines = text.split("\n");
	const needsCollapse = lines.length > maxLines;
	const displayText =
		needsCollapse && !expanded
			? lines.slice(0, maxLines).join("\n") + "\n..."
			: text;

	return (
		<div>
			<pre className="max-h-80 overflow-auto whitespace-pre-wrap px-3 py-2 font-mono text-tiny text-ink-dull">
				{displayText}
			</pre>
			{needsCollapse && (
				<button
					onClick={() => setExpanded(!expanded)}
					className="w-full border-t border-app-line/20 px-3 py-1 text-center text-tiny text-ink-faint hover:text-ink-dull"
				>
					{expanded
						? "Show less"
						: `Show all ${lines.length} lines`}
				</button>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Status display helpers
// ---------------------------------------------------------------------------

const STATUS_ICONS: Record<ToolCallStatus, string> = {
	running: "\u25B6",   // ▶
	completed: "\u2713", // ✓
	error: "\u2717",     // ✗
};

const STATUS_COLORS: Record<ToolCallStatus, string> = {
	running: "text-accent",
	completed: "text-emerald-500",
	error: "text-red-400",
};

/** Human-readable tool name: browser_navigate → Navigate */
function formatToolName(name: string): string {
	// Strip common prefixes for cleaner display
	const stripped = name
		.replace(/^browser_/, "")
		.replace(/^tab_/, "Tab ");

	return stripped
		.split("_")
		.map((word) => word.charAt(0).toUpperCase() + word.slice(1))
		.join(" ");
}

/** Tool category label shown as a faint prefix */
function toolCategory(name: string): string | null {
	if (name.startsWith("browser_")) return "Browser";
	return null;
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function ToolCall({ pair }: { pair: ToolCallPair }) {
	const [expanded, setExpanded] = useState(false);
	const renderer = getRenderer(pair.name);
	const summary = renderer.summary(pair);
	const category = toolCategory(pair.name);
	const displayName = formatToolName(pair.name);

	return (
		<div
			className={cx(
				"rounded-md border bg-app-darkBox/30",
				pair.status === "error"
					? "border-red-500/30"
					: "border-app-line/50",
			)}
		>
			{/* Header — always visible */}
			<button
				onClick={() => setExpanded(!expanded)}
				className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs"
			>
				<span
					className={cx(
						STATUS_COLORS[pair.status],
						pair.status === "running" ? "animate-pulse" : "",
					)}
				>
					{STATUS_ICONS[pair.status]}
				</span>
				{category && (
					<span className="text-tiny text-ink-faint">
						{category}
					</span>
				)}
				<span className="font-medium text-ink-dull">
					{displayName}
				</span>
				{summary && !expanded && (
					<span className="flex-1 truncate text-ink-faint">
						{summary}
					</span>
				)}
				{pair.status === "running" && (
					<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
				)}
			</button>

			{/* Expanded body */}
			{expanded && (
				<div className="border-t border-app-line/30">
					{/* Args section */}
					{renderArgs(pair, renderer)}
					{/* Result section */}
					{renderResult(pair, renderer)}
				</div>
			)}
		</div>
	);
}

function renderArgs(pair: ToolCallPair, renderer: ToolRenderer): React.ReactNode {
	// Try custom args view first
	if (renderer.argsView) {
		const custom = renderer.argsView(pair);
		if (custom) return custom;
	}

	// Default: show parsed args as key-value pairs, or raw JSON
	if (pair.args && Object.keys(pair.args).length > 0) {
		return (
			<div className="border-b border-app-line/20 px-3 py-2">
				<div className="flex flex-col gap-0.5">
					{Object.entries(pair.args).map(([key, value]) => (
						<p key={key} className="text-tiny">
							<span className="text-ink-faint">{key}: </span>
							<span className="text-ink-dull">
								{formatArgValue(key, value)}
							</span>
						</p>
					))}
				</div>
			</div>
		);
	}

	if (pair.argsRaw && pair.argsRaw !== "{}" && pair.argsRaw.trim().length > 0) {
		return (
			<div className="border-b border-app-line/20 px-3 py-2">
				<pre className="max-h-40 overflow-auto font-mono text-tiny text-ink-dull">
					{pair.argsRaw}
				</pre>
			</div>
		);
	}

	return null;
}

function renderResult(pair: ToolCallPair, renderer: ToolRenderer): React.ReactNode {
	if (pair.status === "running") {
		return (
			<div className="flex items-center gap-2 px-3 py-2 text-tiny text-ink-faint">
				<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
				Running...
			</div>
		);
	}

	// Try custom result view first
	if (renderer.resultView) {
		const custom = renderer.resultView(pair);
		if (custom !== undefined) return custom;
	}

	// Default result rendering
	if (!pair.resultRaw) return null;

	// If we have parsed JSON result, show key-value pairs
	if (pair.result && Object.keys(pair.result).length > 0) {
		return (
			<div className="px-3 py-2">
				<div className="flex flex-col gap-0.5">
					{Object.entries(pair.result).map(([key, value]) => (
						<p key={key} className="text-tiny">
							<span className="text-ink-faint">{key}: </span>
							<span className="text-ink-dull">
								{typeof value === "string"
									? truncate(value, 200)
									: JSON.stringify(value)}
							</span>
						</p>
					))}
				</div>
			</div>
		);
	}

	// Plain text result
	return <CollapsiblePre text={pair.resultRaw} maxLines={20} />;
}

function formatArgValue(key: string, value: unknown): string {
	// Redact secret references
	if (key === "secret" && typeof value === "string") {
		return "***";
	}
	if (typeof value === "string") {
		return truncate(value, 100);
	}
	if (typeof value === "boolean" || typeof value === "number") {
		return String(value);
	}
	return JSON.stringify(value);
}

function truncate(text: string, maxLen: number): string {
	if (text.length <= maxLen) return text;
	return text.slice(0, maxLen) + "...";
}
