import { useState, useCallback } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
	api,
	type PromptInspectResponse,
	type PromptSnapshotSummary,
} from "@/api/client";
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, Toggle } from "@/ui";

interface PromptInspectModalProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	channelId: string;
}

type View = "current" | "history";

export function PromptInspectModal({ open, onOpenChange, channelId }: PromptInspectModalProps) {
	const [view, setView] = useState<View>("current");
	const [selectedSnapshot, setSelectedSnapshot] = useState<number | null>(null);
	const queryClient = useQueryClient();

	const { data, isLoading, error } = useQuery<PromptInspectResponse>({
		queryKey: ["inspectPrompt", channelId],
		queryFn: () => api.inspectPrompt(channelId),
		enabled: open,
		staleTime: 0,
	});

	const { data: snapshotList } = useQuery({
		queryKey: ["promptSnapshots", channelId],
		queryFn: () => api.listPromptSnapshots(channelId),
		enabled: open && view === "history",
		staleTime: 0,
	});

	const { data: snapshotDetail, isLoading: snapshotLoading } = useQuery({
		queryKey: ["promptSnapshot", channelId, selectedSnapshot],
		queryFn: () => api.getPromptSnapshot(channelId, selectedSnapshot!),
		enabled: open && selectedSnapshot !== null,
		staleTime: 0,
	});

	const captureMutation = useMutation({
		mutationFn: (enabled: boolean) => api.setPromptCapture(channelId, enabled),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["inspectPrompt", channelId] });
			queryClient.invalidateQueries({ queryKey: ["promptSnapshots", channelId] });
		},
	});

	const captureEnabled = data?.capture_enabled ?? false;

	const handleToggleCapture = useCallback(() => {
		captureMutation.mutate(!captureEnabled);
	}, [captureMutation, captureEnabled]);

	// Determine what to display
	const systemPrompt = view === "current" ? data?.system_prompt : snapshotDetail?.system_prompt;
	const history = view === "current" ? data?.history : snapshotDetail?.history;
	const totalChars = view === "current" ? (data?.total_chars ?? 0) : (snapshotDetail?.system_prompt_chars ?? 0);
	const historyLength = view === "current" ? (data?.history_length ?? 0) : (snapshotDetail?.history_length ?? 0);
	const showContent = view === "current" || selectedSnapshot !== null;
	const contentLoading = view === "current" ? isLoading : snapshotLoading;

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="!flex h-[85vh] max-w-5xl !flex-col !gap-0 overflow-hidden !p-0">
				<DialogHeader className="flex-shrink-0 border-b border-app-line/50 px-6 pt-6 pb-4">
					<DialogTitle>Prompt Inspector</DialogTitle>
					{showContent && !contentLoading && systemPrompt != null && (
						<div className="mt-1 flex items-center gap-4 text-tiny text-ink-faint">
							<span>{totalChars.toLocaleString()} chars</span>
							<span>{historyLength} history messages</span>
							{view === "history" && snapshotDetail && (
								<span>{new Date(snapshotDetail.timestamp_ms).toLocaleString()}</span>
							)}
						</div>
					)}
				</DialogHeader>

				<div className="flex min-h-0 flex-1">
					{/* Sidebar */}
					<div className="flex w-48 flex-shrink-0 flex-col border-r border-app-line/50 bg-app-darkBox/30">
						<div className="flex flex-col gap-0.5 p-2">
							<SidebarButton
								active={view === "current"}
								onClick={() => { setView("current"); setSelectedSnapshot(null); }}
							>
								Current
							</SidebarButton>
							<SidebarButton
								active={view === "history"}
								onClick={() => { setView("history"); setSelectedSnapshot(null); }}
							>
								History
							</SidebarButton>
						</div>

						<div className="mx-2 border-t border-app-line/30" />

						<div className="flex items-center justify-between px-3 py-3">
							<span className="text-tiny text-ink-faint">Capture</span>
							<Toggle
								size="sm"
								checked={captureEnabled}
								onCheckedChange={handleToggleCapture}
								disabled={captureMutation.isPending}
							/>
						</div>

						{view === "history" && (
							<div className="flex-1 overflow-y-auto">
								{!captureEnabled && (
									<p className="px-3 py-2 text-tiny text-ink-faint">
										Enable capture to record prompt snapshots on each LLM turn.
									</p>
								)}
								{captureEnabled && snapshotList?.snapshots.length === 0 && (
									<p className="px-3 py-2 text-tiny text-ink-faint">
										No snapshots yet. Send a message to capture.
									</p>
								)}
								{snapshotList?.snapshots.map((snapshot) => (
									<SnapshotListItem
										key={snapshot.timestamp_ms}
										snapshot={snapshot}
										selected={selectedSnapshot === snapshot.timestamp_ms}
										onClick={() => setSelectedSnapshot(snapshot.timestamp_ms)}
									/>
								))}
							</div>
						)}
					</div>

					{/* Main content */}
					<div className="flex-1 overflow-y-auto">
						{contentLoading && (
							<div className="flex items-center justify-center py-12">
								<span className="text-sm text-ink-faint">Loading...</span>
							</div>
						)}
						{error && (
							<div className="m-6 rounded-md border border-red-500/20 bg-red-500/10 px-4 py-3 text-sm text-red-400">
								Failed to load prompt: {error instanceof Error ? error.message : "Unknown error"}
							</div>
						)}
						{data?.error && (
							<div className="m-6 rounded-md border border-amber-500/20 bg-amber-500/10 px-4 py-3 text-sm text-amber-300">
								{data.message}
							</div>
						)}
						{view === "history" && selectedSnapshot === null && !snapshotLoading && (
							<div className="flex items-center justify-center py-12">
								<span className="text-sm text-ink-faint">
									{captureEnabled
										? "Select a snapshot from the sidebar"
										: "Enable capture to start recording prompt snapshots"}
								</span>
							</div>
						)}
						{showContent && !contentLoading && systemPrompt != null && (
							<pre className="whitespace-pre-wrap break-words p-6 font-mono text-xs text-ink-dull leading-relaxed">
								<span className="text-ink-faint">{"--- SYSTEM PROMPT ---\n\n"}</span>
								{systemPrompt}
								{history != null && (
									<>
										<span className="text-ink-faint">{"\n\n--- MESSAGES ---\n"}</span>
										{renderRawHistory(history)}
									</>
								)}
							</pre>
						)}
					</div>
				</div>

				<div className="flex flex-shrink-0 items-center justify-end border-t border-app-line/50 px-6 py-3">
					<Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
						Close
					</Button>
				</div>
			</DialogContent>
		</Dialog>
	);
}

/** Render the message history as raw text, exactly as the model sees it. */
function renderRawHistory(history: unknown): string {
	const messages = Array.isArray(history) ? history : [];
	if (messages.length === 0) return "\n(empty)";

	const lines: string[] = [];
	for (const message of messages) {
		const role = message.role ?? "unknown";
		const parts = extractTextParts(message);
		lines.push(`\n[${role}]`);
		if (parts.length === 0) {
			lines.push("(empty)");
		} else {
			lines.push(parts.join("\n"));
		}
	}
	return lines.join("\n");
}

/**
 * Extract all text parts from a rig Message, including tool call/result
 * representations. Returns the text exactly as the model would interpret it.
 *
 * Rig serializes:
 * - UserContent with `#[serde(tag = "type")]` -> `{type: "text", text: "..."}`
 * - AssistantContent with `#[serde(untagged)]` -> `{text: "..."}` (no type field),
 *   tool calls are `{id, function: {name, arguments}}`
 */
function extractTextParts(message: any): string[] {
	const parts: string[] = [];
	const content = message.content;

	if (typeof content === "string") {
		parts.push(content);
	} else if (Array.isArray(content)) {
		for (const block of content) {
			if (block.type === "text" && typeof block.text === "string") {
				parts.push(block.text);
			} else if (!block.type && typeof block.text === "string") {
				parts.push(block.text);
			} else if (block.type === "toolresult") {
				const resultText = formatToolResultText(block.content);
				parts.push(`[tool_result id=${block.id}] ${resultText}`);
			} else if (block.function && typeof block.function === "object") {
				const args = typeof block.function.arguments === "string"
					? block.function.arguments
					: JSON.stringify(block.function.arguments);
				parts.push(`[tool_use ${block.function.name}] ${args}`);
			} else if (Array.isArray(block.reasoning)) {
				parts.push(`[thinking] ${block.reasoning.join("\n")}`);
			}
		}
	} else if (content && typeof content === "object") {
		if (typeof content.text === "string") {
			parts.push(content.text);
		} else if (content.function) {
			const args = typeof content.function.arguments === "string"
				? content.function.arguments
				: JSON.stringify(content.function.arguments);
			parts.push(`[tool_use ${content.function.name}] ${args}`);
		} else if (content.type === "toolresult") {
			const resultText = formatToolResultText(content.content);
			parts.push(`[tool_result id=${content.id}] ${resultText}`);
		}
	}

	return parts;
}

function formatToolResultText(content: any): string {
	if (typeof content === "string") return content;
	if (Array.isArray(content)) {
		return content
			.map((c: any) => (typeof c.text === "string" ? c.text : JSON.stringify(c)))
			.join(" ");
	}
	return JSON.stringify(content);
}

function SidebarButton({
	active,
	onClick,
	children,
}: {
	active: boolean;
	onClick: () => void;
	children: React.ReactNode;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			className={`rounded-md px-3 py-1.5 text-left text-sm transition-colors ${
				active
					? "bg-accent/15 text-accent font-medium"
					: "text-ink-dull hover:bg-app-hover hover:text-ink"
			}`}
		>
			{children}
		</button>
	);
}

function SnapshotListItem({
	snapshot,
	selected,
	onClick,
}: {
	snapshot: PromptSnapshotSummary;
	selected: boolean;
	onClick: () => void;
}) {
	const time = new Date(snapshot.timestamp_ms);
	const timeStr = time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
	const dateStr = time.toLocaleDateString([], { month: "short", day: "numeric" });
	const preview = snapshot.user_message.length > 60
		? snapshot.user_message.slice(0, 60) + "..."
		: snapshot.user_message;

	return (
		<button
			type="button"
			onClick={onClick}
			className={`w-full border-b border-app-line/20 px-3 py-2 text-left transition-colors ${
				selected
					? "bg-accent/10 border-l-2 border-l-accent"
					: "hover:bg-app-hover"
			}`}
		>
			<div className="flex items-center gap-2 text-tiny text-ink-faint">
				<span>{dateStr}</span>
				<span>{timeStr}</span>
			</div>
			<p className="mt-0.5 text-tiny text-ink-dull leading-snug truncate">{preview || "(empty)"}</p>
			<div className="mt-0.5 flex gap-2 text-tiny text-ink-faint/60">
				<span>{snapshot.system_prompt_chars.toLocaleString()} ch</span>
				<span>{snapshot.history_length} msgs</span>
			</div>
		</button>
	);
}
