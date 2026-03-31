import {useState} from "react";
import {Button} from "@/ui/Button";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
} from "@/ui/Dialog";
import {Input} from "@/ui/Input";
import type {PortalConversationSummary} from "@/api/types";

interface ConversationsSidebarProps {
	conversations: PortalConversationSummary[];
	activeConversationId: string | null;
	onSelectConversation: (id: string) => void;
	onCreateConversation: () => void;
	onDeleteConversation: (id: string) => void;
	onRenameConversation: (id: string, title: string) => void;
	onArchiveConversation: (id: string, archived: boolean) => void;
	isLoading: boolean;
}

export function ConversationsSidebar({
	conversations,
	activeConversationId,
	onSelectConversation,
	onCreateConversation,
	onDeleteConversation,
	onRenameConversation,
	onArchiveConversation,
	isLoading,
}: ConversationsSidebarProps) {
	const [renameDialogOpen, setRenameDialogOpen] = useState(false);
	const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
	const [selectedConversation, setSelectedConversation] =
		useState<PortalConversationSummary | null>(null);
	const [newTitle, setNewTitle] = useState("");

	const activeConversations = conversations.filter((c) => !c.archived);
	const archivedConversations = conversations.filter((c) => c.archived);

	const handleRename = (conv: PortalConversationSummary) => {
		setSelectedConversation(conv);
		setNewTitle(conv.title);
		setRenameDialogOpen(true);
	};

	const handleDelete = (conv: PortalConversationSummary) => {
		setSelectedConversation(conv);
		setDeleteDialogOpen(true);
	};

	const confirmRename = () => {
		if (selectedConversation && newTitle.trim()) {
			onRenameConversation(selectedConversation.id, newTitle.trim());
			setRenameDialogOpen(false);
			setSelectedConversation(null);
		}
	};

	const confirmDelete = () => {
		if (selectedConversation) {
			onDeleteConversation(selectedConversation.id);
			setDeleteDialogOpen(false);
			setSelectedConversation(null);
		}
	};

	const formatDate = (dateStr: string) => {
		const date = new Date(dateStr);
		const now = new Date();
		const diffDays = Math.floor(
			(now.getTime() - date.getTime()) / (1000 * 60 * 60 * 24),
		);

		if (diffDays === 0) {
			return date.toLocaleTimeString([], {hour: "2-digit", minute: "2-digit"});
		} else if (diffDays === 1) {
			return "Yesterday";
		} else if (diffDays < 7) {
			return date.toLocaleDateString([], {weekday: "short"});
		} else {
			return date.toLocaleDateString([], {month: "short", day: "numeric"});
		}
	};

	return (
		<div className="flex h-full w-56 shrink-0 flex-col border-r border-app-line">
			{/* New conversation button */}
			<div className="border-b border-app-line p-2">
				<Button
					variant="outline"
					className="w-full !h-8"
					onClick={onCreateConversation}
				>
					New Conversation
				</Button>
			</div>
			{/* Conversations List */}
			<div className="flex-1 overflow-y-auto py-2">
				{isLoading ? (
					<div className="px-3 py-4 text-center text-xs text-ink-faint">
						Loading...
					</div>
				) : activeConversations.length === 0 ? (
					<div className="px-3 py-4 text-center text-xs text-ink-faint">
						No conversations yet
					</div>
				) : (
					<div className="space-y-px px-2">
						{activeConversations.map((conv) => (
							<div
								key={conv.id}
								className={`group relative flex cursor-pointer items-center rounded-md px-2 py-1.5 text-xs transition-colors ${
									activeConversationId === conv.id
										? "bg-app-hover text-ink"
										: "text-ink-dull hover:bg-app-hover/50 hover:text-ink"
								}`}
								onClick={() => onSelectConversation(conv.id)}
							>
								<div className="min-w-0 flex-1">
									<div className="truncate">{conv.title}</div>
								</div>
								<span className="ml-2 shrink-0 text-[10px] text-ink-faint group-hover:hidden">
									{formatDate(conv.updated_at)}
								</span>
								<div className="ml-2 hidden shrink-0 items-center gap-0.5 group-hover:flex">
									<button
										onClick={(e) => {
											e.stopPropagation();
											handleRename(conv);
										}}
										className="rounded p-0.5 text-ink-faint hover:bg-app-hover hover:text-ink"
										title="Rename"
									>
										<svg
											width="11"
											height="11"
											viewBox="0 0 24 24"
											fill="none"
											stroke="currentColor"
											strokeWidth="2"
										>
											<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
											<path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z" />
										</svg>
									</button>
									<button
										onClick={(e) => {
											e.stopPropagation();
											onArchiveConversation(conv.id, true);
										}}
										className="rounded p-0.5 text-ink-faint hover:bg-app-hover hover:text-ink"
										title="Archive"
									>
										<svg
											width="11"
											height="11"
											viewBox="0 0 24 24"
											fill="none"
											stroke="currentColor"
											strokeWidth="2"
										>
											<path d="M21 8v13H3V8M1 3h22v5H1zM10 12h4" />
										</svg>
									</button>
									<button
										onClick={(e) => {
											e.stopPropagation();
											handleDelete(conv);
										}}
										className="rounded p-0.5 text-ink-faint hover:bg-red-500/20 hover:text-red-400"
										title="Delete"
									>
										<svg
											width="11"
											height="11"
											viewBox="0 0 24 24"
											fill="none"
											stroke="currentColor"
											strokeWidth="2"
										>
											<path d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
										</svg>
									</button>
								</div>
							</div>
						))}
					</div>
				)}

				{/* Archived Section */}
				{archivedConversations.length > 0 && (
					<div className="mt-4">
						<div className="px-3 py-1 text-xs font-medium text-ink-faint uppercase tracking-wider">
							Archived
						</div>
						<div className="space-y-0.5 px-2">
							{archivedConversations.map((conv) => (
								<div
									key={conv.id}
									className={`group relative flex cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm transition-colors ${
										activeConversationId === conv.id
											? "bg-app-hover text-ink"
											: "text-ink-dull hover:bg-app-hover/50"
									}`}
									onClick={() => onSelectConversation(conv.id)}
								>
									<div className="flex-1 min-w-0">
										<div className="truncate">{conv.title}</div>
									</div>
									<button
										onClick={(e) => {
											e.stopPropagation();
											onArchiveConversation(conv.id, false);
										}}
										className="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-app-hover text-ink-faint hover:text-ink"
										title="Unarchive"
									>
										<svg
											width="12"
											height="12"
											viewBox="0 0 24 24"
											fill="none"
											stroke="currentColor"
											strokeWidth="2"
										>
											<path d="M21 8v13H3V8M1 3h22v5H1z" />
										</svg>
									</button>
								</div>
							))}
						</div>
					</div>
				)}
			</div>

			{/* Rename Dialog */}
			<Dialog open={renameDialogOpen} onOpenChange={setRenameDialogOpen}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Rename Conversation</DialogTitle>
					</DialogHeader>
					<Input
						value={newTitle}
						onChange={(e) => setNewTitle(e.target.value)}
						placeholder="Conversation title"
						onKeyDown={(e) => {
							if (e.key === "Enter") confirmRename();
						}}
					/>
					<DialogFooter>
						<Button
							variant="outline"
							onClick={() => setRenameDialogOpen(false)}
						>
							Cancel
						</Button>
						<Button onClick={confirmRename}>Rename</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			{/* Delete Dialog */}
			<Dialog open={deleteDialogOpen} onOpenChange={setDeleteDialogOpen}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Delete Conversation</DialogTitle>
					</DialogHeader>
					<p className="text-sm text-ink-dull">
						Are you sure you want to delete "{selectedConversation?.title}"?
						This cannot be undone.
					</p>
					<DialogFooter>
						<Button
							variant="outline"
							onClick={() => setDeleteDialogOpen(false)}
						>
							Cancel
						</Button>
						<Button variant="destructive" onClick={confirmDelete}>
							Delete
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}
