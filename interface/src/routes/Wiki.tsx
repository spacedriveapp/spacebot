import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
	api,
	type WikiPage,
	type WikiPageSummary,
	type WikiPageType,
	type CreateWikiPageRequest,
} from "@/api/client";
import { BookBookmark, Plus, MagnifyingGlass, ArrowLeft, ClockCounterClockwise, X } from "@phosphor-icons/react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

const PAGE_TYPES: WikiPageType[] = ["entity", "concept", "decision", "project", "reference"];

const PAGE_TYPE_LABELS: Record<WikiPageType, string> = {
	entity: "Entities",
	concept: "Concepts",
	decision: "Decisions",
	project: "Projects",
	reference: "Reference",
};

// ---------------------------------------------------------------------------
// Markdown renderer with wiki: link support
// ---------------------------------------------------------------------------

function WikiMarkdown({ content, onNavigate }: { content: string; onNavigate: (slug: string) => void }) {
	return (
		<div className="markdown">
			<ReactMarkdown
				remarkPlugins={[remarkGfm]}
				rehypePlugins={[rehypeRaw]}
				components={{
					a: ({ children, href, ...props }) => {
						if (href?.startsWith("wiki:")) {
							const slug = href.slice(5);
							return (
								<button
									onClick={() => onNavigate(slug)}
									className="text-accent underline underline-offset-2 hover:opacity-80"
								>
									{children}
								</button>
							);
						}
						return (
							<a href={href} target="_blank" rel="noopener noreferrer" {...props}>
								{children}
							</a>
						);
					},
				}}
			>
				{content}
			</ReactMarkdown>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Create page form
// ---------------------------------------------------------------------------

function CreatePageForm({ onCreated, onCancel }: { onCreated: (slug: string) => void; onCancel: () => void }) {
	const queryClient = useQueryClient();
	const [title, setTitle] = useState("");
	const [pageType, setPageType] = useState<WikiPageType>("reference");
	const [content, setContent] = useState("");
	const [editSummary, setEditSummary] = useState("");

	const createMutation = useMutation({
		mutationFn: (req: CreateWikiPageRequest) => api.createWikiPage(req),
		onSuccess: (data) => {
			queryClient.invalidateQueries({ queryKey: ["wiki"] });
			onCreated(data.page.slug);
		},
	});

	return (
		<div className="flex flex-col gap-4 p-4">
			<div className="flex items-center gap-2">
				<button onClick={onCancel} className="text-ink-dull hover:text-ink">
					<ArrowLeft className="size-4" />
				</button>
				<h2 className="text-sm font-semibold text-ink">New Wiki Page</h2>
			</div>

			<div className="flex flex-col gap-3">
				<div>
					<label className="mb-1 block text-xs font-medium text-ink-dull">Title</label>
					<input
						value={title}
						onChange={(e) => setTitle(e.target.value)}
						placeholder="Page title"
						className="w-full rounded-md border border-app-line bg-app-input px-3 py-1.5 text-sm text-ink outline-none focus:ring-1 focus:ring-accent"
					/>
				</div>

				<div>
					<label className="mb-1 block text-xs font-medium text-ink-dull">Type</label>
					<select
						value={pageType}
						onChange={(e) => setPageType(e.target.value as WikiPageType)}
						className="w-full rounded-md border border-app-line bg-app-input px-3 py-1.5 text-sm text-ink outline-none focus:ring-1 focus:ring-accent"
					>
						{PAGE_TYPES.map((t) => (
							<option key={t} value={t}>{t}</option>
						))}
					</select>
				</div>

				<div>
					<label className="mb-1 block text-xs font-medium text-ink-dull">Content</label>
					<textarea
						value={content}
						onChange={(e) => setContent(e.target.value)}
						placeholder="Write markdown content. Link other pages with [display text](wiki:slug)."
						rows={12}
						className="w-full rounded-md border border-app-line bg-app-input px-3 py-1.5 font-mono text-sm text-ink outline-none focus:ring-1 focus:ring-accent"
					/>
				</div>

				<div>
					<label className="mb-1 block text-xs font-medium text-ink-dull">Edit summary (optional)</label>
					<input
						value={editSummary}
						onChange={(e) => setEditSummary(e.target.value)}
						placeholder="Describe what this page is about"
						className="w-full rounded-md border border-app-line bg-app-input px-3 py-1.5 text-sm text-ink outline-none focus:ring-1 focus:ring-accent"
					/>
				</div>

				{createMutation.error && (
					<p className="text-xs text-red-400">{String(createMutation.error)}</p>
				)}

				<div className="flex gap-2">
					<button
						onClick={() => createMutation.mutate({ title, page_type: pageType, content, edit_summary: editSummary || undefined })}
						disabled={!title || createMutation.isPending}
						className="rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-white hover:bg-accent/90 disabled:opacity-50"
					>
						{createMutation.isPending ? "Creating…" : "Create Page"}
					</button>
					<button onClick={onCancel} className="rounded-md px-3 py-1.5 text-sm font-medium text-ink-dull hover:text-ink">
						Cancel
					</button>
				</div>
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Page detail view
// ---------------------------------------------------------------------------

function PageDetail({ slug, onBack, onNavigate }: { slug: string; onBack: () => void; onNavigate: (slug: string) => void }) {
	const [showHistory, setShowHistory] = useState(false);

	const { data, isLoading } = useQuery({
		queryKey: ["wiki", "page", slug],
		queryFn: () => api.getWikiPage(slug),
	});

	const { data: historyData } = useQuery({
		queryKey: ["wiki", "history", slug],
		queryFn: () => api.getWikiHistory(slug),
		enabled: showHistory,
	});

	const page = data?.page;

	if (isLoading) {
		return (
			<div className="flex flex-1 items-center justify-center">
				<div className="text-xs text-ink-dull">Loading…</div>
			</div>
		);
	}

	if (!page) {
		return (
			<div className="flex flex-1 flex-col items-center justify-center gap-2">
				<p className="text-sm text-ink-dull">Page not found</p>
				<button onClick={onBack} className="text-xs text-accent hover:underline">Go back</button>
			</div>
		);
	}

	return (
		<div className="flex flex-1 flex-col overflow-hidden">
			{/* Header */}
			<div className="flex shrink-0 items-center gap-2 border-b border-app-line/30 px-4 py-3">
				<button onClick={onBack} className="text-ink-dull hover:text-ink">
					<ArrowLeft className="size-4" />
				</button>
				<div className="flex-1 min-w-0">
					<h1 className="text-base font-semibold text-ink truncate">{page.title}</h1>
					<div className="flex items-center gap-2 text-xs text-ink-dull">
						<span className="rounded bg-app-selected/40 px-1.5 py-0.5 capitalize">{page.page_type}</span>
						<span>v{page.version}</span>
						<span>·</span>
						<span>{new Date(page.updated_at).toLocaleDateString()}</span>
					</div>
				</div>
				<button
					onClick={() => setShowHistory(!showHistory)}
					className={`text-ink-dull hover:text-ink ${showHistory ? "text-accent" : ""}`}
					title="Version history"
				>
					<ClockCounterClockwise className="size-4" />
				</button>
			</div>

			{showHistory && historyData ? (
				<div className="flex-1 overflow-y-auto p-4">
					<h3 className="mb-3 text-xs font-semibold uppercase tracking-wide text-ink-dull">Version History</h3>
					<div className="space-y-2">
						{historyData.versions.map((v) => (
							<div key={v.id} className="rounded-lg border border-app-line/30 p-3">
								<div className="flex items-center justify-between">
									<span className="text-sm font-medium text-ink">Version {v.version}</span>
									<span className="text-xs text-ink-dull">{new Date(v.created_at).toLocaleString()}</span>
								</div>
								{v.edit_summary && (
									<p className="mt-1 text-xs text-ink-dull">{v.edit_summary}</p>
								)}
								<p className="mt-1 text-xs text-ink-dull">by {v.author_id}</p>
							</div>
						))}
					</div>
				</div>
			) : (
				<div className="flex-1 overflow-y-auto p-4">
					<WikiMarkdown content={page.content} onNavigate={onNavigate} />

					{page.related.length > 0 && (
						<div className="mt-6 border-t border-app-line/30 pt-4">
							<h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-dull">Related</h3>
							<div className="flex flex-wrap gap-2">
								{page.related.map((slug) => (
									<button
										key={slug}
										onClick={() => onNavigate(slug)}
										className="rounded-full bg-app-selected/30 px-2.5 py-1 text-xs text-ink hover:bg-app-selected/50"
									>
										{slug}
									</button>
								))}
							</div>
						</div>
					)}
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Page list sidebar
// ---------------------------------------------------------------------------

function PageList({
	pages,
	selectedSlug,
	onSelect,
}: {
	pages: WikiPageSummary[];
	selectedSlug: string | null;
	onSelect: (slug: string) => void;
}) {
	const grouped = PAGE_TYPES.reduce<Record<string, WikiPageSummary[]>>((acc, type) => {
		acc[type] = pages.filter((p) => p.page_type === type);
		return acc;
	}, {} as Record<string, WikiPageSummary[]>);

	return (
		<div className="space-y-4">
			{PAGE_TYPES.map((type) => {
				const group = grouped[type];
				if (!group || group.length === 0) return null;
				return (
					<div key={type}>
						<div className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-[0.16em] text-ink-dull">
							{PAGE_TYPE_LABELS[type]}
						</div>
						<div className="space-y-0.5">
							{group.map((page) => (
								<button
									key={page.slug}
									onClick={() => onSelect(page.slug)}
									className={`w-full rounded-lg px-2 py-1.5 text-left text-sm transition-colors ${
										selectedSlug === page.slug
											? "bg-app-selected/40 text-ink"
											: "text-ink-dull hover:bg-app-selected/20 hover:text-ink"
									}`}
								>
									<span className="truncate block">{page.title}</span>
								</button>
							))}
						</div>
					</div>
				);
			})}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Main Wiki view
// ---------------------------------------------------------------------------

export function Wiki() {
	const [selectedSlug, setSelectedSlug] = useState<string | null>(null);
	const [creating, setCreating] = useState(false);
	const [search, setSearch] = useState("");

	const { data: listData, isLoading } = useQuery({
		queryKey: ["wiki", "list"],
		queryFn: () => api.listWikiPages(),
	});

	const { data: searchData } = useQuery({
		queryKey: ["wiki", "search", search],
		queryFn: () => api.searchWikiPages({ query: search }),
		enabled: search.length > 1,
	});

	const pages = search.length > 1
		? (searchData?.pages ?? [])
		: (listData?.pages ?? []);

	const totalPages = listData?.pages.length ?? 0;

	if (creating) {
		return (
			<div className="flex h-full flex-col">
				<CreatePageForm
					onCreated={(slug) => {
						setCreating(false);
						setSelectedSlug(slug);
					}}
					onCancel={() => setCreating(false)}
				/>
			</div>
		);
	}

	return (
		<div className="flex h-full overflow-hidden">
			{/* Sidebar */}
			<div className="flex w-56 shrink-0 flex-col border-r border-app-line/30">
				{/* Header */}
				<div className="flex items-center gap-2 border-b border-app-line/30 px-3 py-3">
					<BookBookmark className="size-4 text-ink-dull" weight="bold" />
					<span className="text-sm font-semibold text-ink">Wiki</span>
					{totalPages > 0 && (
						<span className="ml-auto text-xs text-ink-dull">{totalPages}</span>
					)}
					<button
						onClick={() => setCreating(true)}
						className="ml-1 text-ink-dull hover:text-ink"
						title="New page"
					>
						<Plus className="size-4" />
					</button>
				</div>

				{/* Search */}
				<div className="px-3 py-2">
					<div className="flex items-center gap-2 rounded-md border border-app-line bg-app-input px-2 py-1">
						<MagnifyingGlass className="size-3.5 shrink-0 text-ink-dull" />
						<input
							value={search}
							onChange={(e) => setSearch(e.target.value)}
							placeholder="Search…"
							className="min-w-0 flex-1 bg-transparent text-xs text-ink outline-none placeholder:text-ink-dull"
						/>
						{search && (
							<button onClick={() => setSearch("")} className="text-ink-dull hover:text-ink">
								<X className="size-3" />
							</button>
						)}
					</div>
				</div>

				{/* Page list */}
				<div className="flex-1 overflow-y-auto px-3 pb-4">
					{isLoading ? (
						<p className="py-4 text-center text-xs text-ink-dull">Loading…</p>
					) : pages.length === 0 ? (
						<div className="py-6 text-center">
							<p className="text-xs text-ink-dull">
								{search ? "No results" : "No pages yet"}
							</p>
							{!search && (
								<button
									onClick={() => setCreating(true)}
									className="mt-2 text-xs text-accent hover:underline"
								>
									Create the first page
								</button>
							)}
						</div>
					) : (
						<PageList
							pages={pages}
							selectedSlug={selectedSlug}
							onSelect={setSelectedSlug}
						/>
					)}
				</div>
			</div>

			{/* Content */}
			<div className="flex flex-1 overflow-hidden">
				{selectedSlug ? (
					<PageDetail
						slug={selectedSlug}
						onBack={() => setSelectedSlug(null)}
						onNavigate={setSelectedSlug}
					/>
				) : (
					<div className="flex flex-1 flex-col items-center justify-center gap-3">
						<BookBookmark className="size-10 text-ink-dull/30" weight="thin" />
						<p className="text-sm text-ink-dull">Select a page or create a new one</p>
						<button
							onClick={() => setCreating(true)}
							className="flex items-center gap-1.5 rounded-md bg-accent/10 px-3 py-1.5 text-sm font-medium text-accent hover:bg-accent/20"
						>
							<Plus className="size-3.5" />
							New Page
						</button>
					</div>
				)}
			</div>
		</div>
	);
}
