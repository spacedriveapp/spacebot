import { useCallback, useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faRobot, faUser, faGear, faChevronDown, faChevronRight } from "@fortawesome/free-solid-svg-icons";
import { Badge, Button } from "@spacedrive/primitives";
import { api, type TaskComment, type TaskCommentAuthor } from "@/api/client";
import { useLiveContext } from "@/hooks/useLiveContext";

const PAGE_SIZE = 50;

/** Longest comment the backend accepts, in bytes. Mirrors MAX_COMMENT_BODY_BYTES. */
const MAX_BODY_BYTES = 4000;

const AUTHOR_ICON: Record<TaskCommentAuthor, typeof faUser> = {
	user: faUser,
	agent: faRobot,
	worker: faGear,
};

const AUTHOR_VARIANT: Record<TaskCommentAuthor, "info" | "success" | "default"> = {
	user: "info",
	agent: "success",
	worker: "default",
};

function formatTimestamp(value: string): string {
	const parsed = new Date(value);
	return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

/**
 * A worker-authored or worker-tagged comment links back to the run that
 * produced it. The body stays the agent's summary; the full output is fetched
 * only when the user asks for it.
 */
function WorkerOutput({ agentId, workerId }: { agentId: string; workerId: string }) {
	const [expanded, setExpanded] = useState(false);

	const { data, isLoading, error } = useQuery({
		queryKey: ["worker-detail", agentId, workerId],
		queryFn: () => api.workerDetail(agentId, workerId),
		enabled: expanded,
		staleTime: 60_000,
	});

	return (
		<div className="mt-1.5">
			<button
				type="button"
				onClick={() => setExpanded((open) => !open)}
				className="inline-flex items-center gap-1.5 text-[11px] text-ink-dull hover:text-ink"
			>
				<FontAwesomeIcon
					icon={expanded ? faChevronDown : faChevronRight}
					className="text-[9px]"
				/>
				<Badge variant="default" size="sm">
					<FontAwesomeIcon icon={faGear} className="text-[10px]" />
					<span className="font-mono">{workerId.slice(0, 8)}</span>
				</Badge>
			</button>

			{expanded && (
				<div className="mt-1.5 rounded border border-app-line/60 bg-app-box/40 p-2">
					{isLoading ? (
						<span className="text-[11px] text-ink-faint">Loading worker output…</span>
					) : error ? (
						<span className="text-[11px] text-red-400">
							Worker run is no longer available.
						</span>
					) : (
						<pre className="max-h-60 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-ink-dull">
							{data?.result?.trim() || "This worker recorded no output."}
						</pre>
					)}
				</div>
			)}
		</div>
	);
}

function CommentRow({
	comment,
	agentId,
	resolveAgentName,
}: {
	comment: TaskComment;
	agentId?: string;
	resolveAgentName?: (agentId: string) => string;
}) {
	const author =
		comment.author_type === "agent" && comment.author_id
			? (resolveAgentName?.(comment.author_id) ?? comment.author_id)
			: comment.author_type === "worker"
				? "Worker"
				: (comment.author_id ?? "You");

	return (
		<li className="border-b border-app-line/40 py-2.5 last:border-b-0">
			<div className="mb-1 flex items-center gap-2">
				<Badge variant={AUTHOR_VARIANT[comment.author_type]} size="sm">
					<FontAwesomeIcon icon={AUTHOR_ICON[comment.author_type]} className="text-[10px]" />
					<span>{author}</span>
				</Badge>
				<span className="text-[10px] text-ink-faint">
					{formatTimestamp(comment.created_at)}
				</span>
			</div>
			<p className="whitespace-pre-wrap break-words text-xs leading-relaxed text-ink-dull">
				{comment.body}
			</p>
			{comment.worker_id && agentId && (
				<WorkerOutput agentId={agentId} workerId={comment.worker_id} />
			)}
		</li>
	);
}

/**
 * Chronological comment thread for a task, with a composer.
 *
 * Comments are append-only: there is no edit or delete affordance because the
 * store has no such operation. `agentId` enables the worker-output links; it is
 * the agent the task is assigned to, which is the pool a worker run lives in.
 */
export function TaskComments({
	taskNumber,
	agentId,
	resolveAgentName,
}: {
	taskNumber: number;
	agentId?: string;
	resolveAgentName?: (agentId: string) => string;
}) {
	const queryClient = useQueryClient();
	const { taskEventVersion } = useLiveContext();
	const queryKey = ["task-comments", taskNumber];

	// A comment from an autonomy run arrives over SSE as a task event.
	const previousVersion = useRef(taskEventVersion);
	useEffect(() => {
		if (taskEventVersion !== previousVersion.current) {
			previousVersion.current = taskEventVersion;
			void queryClient.invalidateQueries({ queryKey });
		}
	}, [taskEventVersion, queryClient, taskNumber]);

	const { data, isLoading, error } = useQuery({
		queryKey,
		queryFn: () => api.listTaskComments(taskNumber, { limit: PAGE_SIZE }),
	});

	const [draft, setDraft] = useState("");

	const createMutation = useMutation({
		mutationFn: (body: string) => api.createTaskComment(taskNumber, { body }),
		onSuccess: () => {
			setDraft("");
			void queryClient.invalidateQueries({ queryKey });
		},
	});

	const handleSubmit = useCallback(() => {
		const body = draft.trim();
		if (body.length < 4 || body.length > MAX_BODY_BYTES) return;
		createMutation.mutate(body);
	}, [draft, createMutation]);

	const comments = data?.comments ?? [];
	const total = data?.total ?? 0;
	const hasMore = data?.next_cursor !== undefined && data?.next_cursor !== null;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Comments{total > 0 ? ` (${total})` : ""}
			</h3>

			{isLoading ? (
				<p className="text-xs text-ink-faint">Loading comments…</p>
			) : error ? (
				<p className="text-xs text-red-400">Failed to load comments.</p>
			) : comments.length === 0 ? (
				<p className="text-xs text-ink-faint">
					No comments yet. Findings from autonomy runs land here.
				</p>
			) : (
				<>
					<ul className="mb-2">
						{comments.map((comment) => (
							<CommentRow
								key={comment.id}
								comment={comment}
								agentId={agentId}
								resolveAgentName={resolveAgentName}
							/>
						))}
					</ul>
					{hasMore && (
						<p className="mb-2 text-[11px] text-ink-faint">
							Showing the first {comments.length} of {total}.
						</p>
					)}
				</>
			)}

			<div className="mt-2">
				<textarea
					value={draft}
					onChange={(event) => setDraft(event.target.value)}
					placeholder="Add a comment…"
					rows={3}
					maxLength={MAX_BODY_BYTES}
					className="w-full resize-y rounded border border-app-line bg-app-input px-2 py-1.5 text-xs text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
				/>
				<div className="mt-1.5 flex items-center justify-between">
					<span className="text-[10px] text-ink-faint">
						{createMutation.isError ? "Failed to post comment." : "Comments are permanent."}
					</span>
					<Button
						size="sm"
						disabled={draft.trim().length < 4 || createMutation.isPending}
						onClick={handleSubmit}
					>
						{createMutation.isPending ? "Posting…" : "Comment"}
					</Button>
				</div>
			</div>
		</div>
	);
}
