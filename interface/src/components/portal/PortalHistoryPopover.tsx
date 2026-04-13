import { useState } from "react";
import { SearchBar } from "@spacedrive/primitives";
import type { PortalConversationSummary } from "@/api/types";

interface PortalHistoryPopoverProps {
	conversations: PortalConversationSummary[];
	activeConversationId: string | null;
	onSelect: (id: string) => void;
	onDelete: (id: string) => void;
	onArchive: (id: string, archived: boolean) => void;
}

function formatTimestamp(iso: string): string {
	const date = new Date(iso);
	const now = new Date();
	const diff = now.getTime() - date.getTime();
	const minutes = Math.floor(diff / 60_000);
	if (minutes < 1) return "just now";
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h`;
	const days = Math.floor(hours / 24);
	if (days < 7) return `${days}d`;
	return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function PortalHistoryPopover({
	conversations,
	activeConversationId,
	onSelect,
	onDelete,
	onArchive,
}: PortalHistoryPopoverProps) {
	const [search, setSearch] = useState("");

	const q = search.trim().toLowerCase();
	const filtered = conversations.filter((conv) => {
		if (!q) return true;
		return (
			conv.title.toLowerCase().includes(q) ||
			(conv.last_message_preview?.toLowerCase().includes(q) ?? false)
		);
	});

	const active = filtered.filter((c) => !c.archived);
	const archived = filtered.filter((c) => c.archived);

	return (
		<div className="flex max-h-[70vh] w-full flex-col">
			<div className="border-b border-app-line/40 p-2">
				<SearchBar
					value={search}
					onChange={setSearch}
					placeholder="Search conversations..."
				/>
			</div>

			<div className="flex-1 overflow-y-auto p-1">
				{active.length === 0 && archived.length === 0 && (
					<div className="px-3 py-6 text-center text-xs text-ink-faint">
						{q ? "No matching conversations" : "No conversations yet"}
					</div>
				)}

				{active.length > 0 && (
					<div className="space-y-px">
						{active.map((conv) => (
							<HistoryRow
								key={conv.id}
								conv={conv}
								isActive={conv.id === activeConversationId}
								onSelect={() => onSelect(conv.id)}
								onArchive={() => onArchive(conv.id, true)}
								onDelete={() => onDelete(conv.id)}
							/>
						))}
					</div>
				)}

				{archived.length > 0 && (
					<div className="mt-3">
						<div className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-ink-faint">
							Archived
						</div>
						<div className="space-y-px">
							{archived.map((conv) => (
								<HistoryRow
									key={conv.id}
									conv={conv}
									isActive={conv.id === activeConversationId}
									onSelect={() => onSelect(conv.id)}
									onArchive={() => onArchive(conv.id, false)}
									onDelete={() => onDelete(conv.id)}
									archived
								/>
							))}
						</div>
					</div>
				)}
			</div>
		</div>
	);
}

function HistoryRow({
	conv,
	isActive,
	onSelect,
	onArchive,
	onDelete,
	archived = false,
}: {
	conv: PortalConversationSummary;
	isActive: boolean;
	onSelect: () => void;
	onArchive: () => void;
	onDelete: () => void;
	archived?: boolean;
}) {
	return (
		<div
			className={`group flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-xs transition-colors ${
				isActive ? "bg-app-hover text-ink" : "text-ink-dull hover:bg-app-hover/50 hover:text-ink"
			}`}
			onClick={onSelect}
		>
			<div className="min-w-0 flex-1">
				<div className="truncate font-medium">{conv.title}</div>
				{conv.last_message_preview && (
					<div className="mt-0.5 truncate text-[10px] text-ink-faint">
						{conv.last_message_preview}
					</div>
				)}
			</div>
			<span className="shrink-0 text-[10px] text-ink-faint group-hover:hidden">
				{formatTimestamp(conv.updated_at)}
			</span>
			<div className="hidden shrink-0 items-center gap-0.5 group-hover:flex">
				<button
					onClick={(e) => {
						e.stopPropagation();
						onArchive();
					}}
					className="rounded p-0.5 text-ink-faint hover:bg-app-hover hover:text-ink"
					title={archived ? "Unarchive" : "Archive"}
				>
					<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
						<path d="M21 8v13H3V8M1 3h22v5H1zM10 12h4" />
					</svg>
				</button>
				<button
					onClick={(e) => {
						e.stopPropagation();
						onDelete();
					}}
					className="rounded p-0.5 text-ink-faint hover:bg-red-500/20 hover:text-red-400"
					title="Delete"
				>
					<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
						<path d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
					</svg>
				</button>
			</div>
		</div>
	);
}
