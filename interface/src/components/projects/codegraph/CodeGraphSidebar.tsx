// Left-side sidebar with two tabs:
//   - Explorer: a file tree built from File.source_file by splitting on /.
//   - Filters:  node-label toggles, edge-type toggles, focus-depth chips,
//               color legend, and the embedded GraphStatsView charts.
//
// Ported from reference/GitNexus/gitnexus-web/src/components/FileTreePanel.tsx
// adapted to spacebot's node shape and tailwind tokens.

import { useCallback, useEffect, useMemo, useState } from "react";
import { clsx } from "clsx";
import { HugeiconsIcon } from "@hugeicons/react";
import {
	ArrowDown01Icon,
	ArrowRight01Icon,
	Folder01Icon,
	CodeIcon,
	DocumentCodeIcon,
	Search01Icon,
	Cancel01Icon,
	Target02Icon,
	LeftToRightListBulletIcon,
	CubeIcon,
	HashtagIcon,
	TextFontIcon,
	ThirdBracketIcon,
	VariableIcon,
	AtIcon,
} from "@hugeicons/core-free-icons";
import type { IconSvgElement } from "@hugeicons/react";
import * as Popover from "@radix-ui/react-popover";
import { NodeColorPicker } from "./NodeColorPicker";
import {
	NODE_COLORS,
	FILTERABLE_LABELS,
	ALL_EDGE_TYPES,
	EDGE_INFO,
	type NodeLabel,
	type EdgeType,
} from "./constants";

// Icon per filter-facing node type. Mirrors GitNexus's lucide choices
// (Box/Hash/List/Type/Braces/Variable/AtSign/FileCode) using their
// closest Hugeicons counterparts.
const NODE_TYPE_ICONS: Record<string, IconSvgElement> = {
	Folder: Folder01Icon,
	File: DocumentCodeIcon,
	Class: CubeIcon,
	Interface: HashtagIcon,
	Enum: LeftToRightListBulletIcon,
	Type: TextFontIcon,
	Function: ThirdBracketIcon,
	Method: ThirdBracketIcon,
	Variable: VariableIcon,
	Decorator: AtIcon,
	Import: DocumentCodeIcon,
};

const getNodeTypeIcon = (label: NodeLabel): IconSvgElement =>
	NODE_TYPE_ICONS[label] ?? CodeIcon;
import { getNodeColor, getEdgeColor } from "./graphAdapter";
import type { BulkNode } from "./types";
import { GraphStatsView } from "../GraphStatsView";

// ---------------------------------------------------------------------------
// File-tree helpers — builds a folder-nested tree out of the flat list of
// File nodes using their `source_file` paths. Folder nodes aren't needed
// here: we derive structure from file paths, which is what GitNexus does.
// ---------------------------------------------------------------------------

interface TreeNode {
	id: string;
	name: string;
	type: "folder" | "file";
	path: string;
	children: TreeNode[];
	graphNode?: BulkNode;
}

const buildFileTree = (nodes: BulkNode[]): TreeNode[] => {
	const root: TreeNode[] = [];
	const pathMap = new Map<string, TreeNode>();
	const fileNodes = nodes
		.filter((n) => n.label === "File" && !!n.source_file)
		.sort((a, b) => (a.source_file ?? "").localeCompare(b.source_file ?? ""));

	for (const node of fileNodes) {
		const parts = (node.source_file ?? "").split("/").filter(Boolean);
		let currentPath = "";
		let currentLevel = root;
		parts.forEach((part, index) => {
			currentPath = currentPath ? `${currentPath}/${part}` : part;
			let existing = pathMap.get(currentPath);
			if (!existing) {
				const isLast = index === parts.length - 1;
				existing = {
					id: isLast ? String(node.id) : currentPath,
					name: part,
					type: isLast ? "file" : "folder",
					path: currentPath,
					children: [],
					graphNode: isLast ? node : undefined,
				};
				pathMap.set(currentPath, existing);
				currentLevel.push(existing);
			}
			currentLevel = existing.children;
		});
	}
	return root;
};

// ---------------------------------------------------------------------------
// Recursive tree item
// ---------------------------------------------------------------------------

interface TreeItemProps {
	node: TreeNode;
	depth: number;
	searchQuery: string;
	selectedPath: string | null;
	expandedPaths: Set<string>;
	toggleExpanded: (path: string) => void;
	onFileClick: (node: TreeNode) => void;
}

function TreeItem({
	node,
	depth,
	searchQuery,
	selectedPath,
	expandedPaths,
	toggleExpanded,
	onFileClick,
}: TreeItemProps) {
	const isExpanded = expandedPaths.has(node.path);
	const isSelected = selectedPath === node.path;
	const hasChildren = node.children.length > 0;
	const [isHovered, setIsHovered] = useState(false);
	const nodeColor =
		node.type === "folder" ? NODE_COLORS.Folder : NODE_COLORS.File;

	const filteredChildren = useMemo(() => {
		if (!searchQuery) return node.children;
		const q = searchQuery.toLowerCase();
		const matches = (n: TreeNode): boolean =>
			n.name.toLowerCase().includes(q) || n.children.some(matches);
		return node.children.filter(matches);
	}, [node.children, searchQuery]);

	const nameMatches =
		searchQuery && node.name.toLowerCase().includes(searchQuery.toLowerCase());

	const handleClick = () => {
		if (hasChildren) toggleExpanded(node.path);
		onFileClick(node);
	};

	return (
		<div>
			<button
				onClick={handleClick}
				onMouseEnter={() => setIsHovered(true)}
				onMouseLeave={() => setIsHovered(false)}
				className={clsx(
					"relative flex w-full items-center gap-1.5 rounded border-l-2 px-2 py-1 text-left text-sm transition-colors",
					isSelected
						? "border-accent bg-accent/15 text-accent"
						: "border-transparent text-ink-dull hover:text-ink",
					nameMatches && !isSelected && "bg-accent/10",
				)}
				style={{
					paddingLeft: `${depth * 12 + 8}px`,
					backgroundColor:
						isHovered && !isSelected
							? `${nodeColor}20`
							: undefined,
					borderLeftColor:
						isHovered && !isSelected ? nodeColor : undefined,
				}}
			>
				{hasChildren ? (
					isExpanded ? (
						<HugeiconsIcon
							icon={ArrowDown01Icon}
							className="h-3.5 w-3.5 shrink-0 text-ink-faint"
						/>
					) : (
						<HugeiconsIcon
							icon={ArrowRight01Icon}
							className="h-3.5 w-3.5 shrink-0 text-ink-faint"
						/>
					)
				) : (
					<span className="w-3.5" />
				)}

				<HugeiconsIcon
					icon={node.type === "folder" ? Folder01Icon : DocumentCodeIcon}
					className="h-4 w-4 shrink-0"
					style={{
						color: node.type === "folder" ? NODE_COLORS.Folder : NODE_COLORS.File,
					}}
				/>

				<span className="flex-1 truncate font-mono text-xs">{node.name}</span>

				{node.type === "folder" && hasChildren && (
					<span className="shrink-0 rounded bg-app px-1.5 py-0.5 font-mono text-[10px] tabular-nums text-ink-faint">
						{node.children.length}
					</span>
				)}
			</button>

			{isExpanded && filteredChildren.length > 0 && (
				<div>
					{filteredChildren.map((child) => (
						<TreeItem
							key={child.id}
							node={child}
							depth={depth + 1}
							searchQuery={searchQuery}
							selectedPath={selectedPath}
							expandedPaths={expandedPaths}
							toggleExpanded={toggleExpanded}
							onFileClick={onFileClick}
						/>
					))}
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Sidebar props
// ---------------------------------------------------------------------------

interface Props {
	projectId: string;
	nodes: BulkNode[];
	selectedNode: BulkNode | null;
	onSelectNode: (node: BulkNode) => void;
	onFocusNode: (node: BulkNode) => void;
	visibleLabels: NodeLabel[];
	onToggleLabel: (label: NodeLabel) => void;
	visibleEdgeTypes: EdgeType[];
	onToggleEdge: (edge: EdgeType) => void;
	depthFilter: number | null;
	onChangeDepthFilter: (depth: number | null) => void;
	colorOverrides: Record<string, string>;
	onColorChange: (label: NodeLabel, color: string | null) => void;
	edgeColorOverrides: Record<string, string>;
	onEdgeColorChange: (edge: EdgeType, color: string | null) => void;
	/** Whether the Filters tab is applicable to the current view. The mermaid
	 *  view ignores label/edge/depth filters, so the tab is hidden there. */
	showFilters?: boolean;
}

export function CodeGraphSidebar({
	projectId,
	nodes,
	selectedNode,
	onSelectNode,
	onFocusNode,
	visibleLabels,
	onToggleLabel,
	visibleEdgeTypes,
	onToggleEdge,
	depthFilter,
	onChangeDepthFilter,
	colorOverrides,
	onColorChange,
	edgeColorOverrides,
	onEdgeColorChange,
	showFilters = true,
}: Props) {
	const [isCollapsed, setIsCollapsed] = useState(false);
	const [activeTab, setActiveTab] = useState<"files" | "filters">("files");

	// If the Filters tab becomes inapplicable while it's open (user switched
	// to mermaid with filters showing), fall back to the file explorer so
	// the panel doesn't render an empty-shell tab.
	useEffect(() => {
		if (!showFilters && activeTab === "filters") {
			setActiveTab("files");
		}
	}, [showFilters, activeTab]);
	const [searchQuery, setSearchQuery] = useState("");
	const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());

	const fileTree = useMemo(() => buildFileTree(nodes), [nodes]);

	// Auto-expand first level once, when the tree first loads.
	useEffect(() => {
		if (fileTree.length > 0 && expandedPaths.size === 0) {
			setExpandedPaths(new Set(fileTree.map((n) => n.path)));
		}
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [fileTree.length]);

	// Auto-expand parent folders when a selection comes from outside.
	useEffect(() => {
		const path = selectedNode?.source_file;
		if (!path) return;
		const parts = path.split("/").filter(Boolean);
		const toExpand: string[] = [];
		let curr = "";
		for (let i = 0; i < parts.length - 1; i++) {
			curr = curr ? `${curr}/${parts[i]}` : parts[i];
			toExpand.push(curr);
		}
		if (toExpand.length > 0) {
			setExpandedPaths((prev) => {
				const next = new Set(prev);
				toExpand.forEach((p) => next.add(p));
				return next;
			});
		}
	}, [selectedNode?.id]);

	const toggleExpanded = useCallback((path: string) => {
		setExpandedPaths((prev) => {
			const next = new Set(prev);
			if (next.has(path)) next.delete(path);
			else next.add(path);
			return next;
		});
	}, []);

	const handleFileClick = useCallback(
		(treeNode: TreeNode) => {
			if (treeNode.graphNode) {
				const isSame = selectedNode?.qualified_name === treeNode.graphNode.qualified_name;
				onSelectNode(treeNode.graphNode);
				if (!isSame) onFocusNode(treeNode.graphNode);
			}
		},
		[onSelectNode, onFocusNode, selectedNode],
	);

	const selectedPath = selectedNode?.source_file ?? null;

	// Collapsed rail
	if (isCollapsed) {
		return (
			<div className="flex h-full w-12 flex-col items-center gap-2 border-r border-app-line bg-app-darkBox py-3">
				<button
					onClick={() => setIsCollapsed(false)}
					className="rounded p-2 text-ink-dull transition-colors hover:bg-app-hover hover:text-ink"
					title="Expand Panel"
				>
					<HugeiconsIcon icon={ArrowRight01Icon} className="h-5 w-5" />
				</button>
				<div className="my-1 h-px w-6 bg-app-line" />
				<button
					onClick={() => {
						setIsCollapsed(false);
						setActiveTab("files");
					}}
					className={clsx(
						"rounded p-2 transition-colors",
						activeTab === "files"
							? "bg-accent/15 text-accent"
							: "text-ink-dull hover:bg-app-hover hover:text-ink",
					)}
					title="Explorer"
				>
					<HugeiconsIcon icon={Folder01Icon} className="h-5 w-5" />
				</button>
				{showFilters && (
					<button
						onClick={() => {
							setIsCollapsed(false);
							setActiveTab("filters");
						}}
						className={clsx(
							"rounded p-2 transition-colors",
							activeTab === "filters"
								? "bg-accent/15 text-accent"
								: "text-ink-dull hover:bg-app-hover hover:text-ink",
						)}
						title="Filters"
					>
						<HugeiconsIcon icon={LeftToRightListBulletIcon} className="h-5 w-5" />
					</button>
				)}
			</div>
		);
	}

	return (
		<div className="flex h-full w-72 flex-col border-r border-app-line bg-app-darkBox">
			{/* Tab header */}
			<div className="flex items-center justify-between border-b border-app-line px-3 py-2">
				<div className="flex items-center gap-1">
					<button
						onClick={() => setActiveTab("files")}
						className={clsx(
							"rounded px-2 py-1 text-xs transition-colors",
							activeTab === "files"
								? "bg-accent/20 text-accent"
								: "text-ink-dull hover:bg-app-hover hover:text-ink",
						)}
					>
						Explorer
					</button>
					{showFilters && (
						<button
							onClick={() => setActiveTab("filters")}
							className={clsx(
								"rounded px-2 py-1 text-xs transition-colors",
								activeTab === "filters"
									? "bg-accent/20 text-accent"
									: "text-ink-dull hover:bg-app-hover hover:text-ink",
							)}
						>
							Filters
						</button>
					)}
				</div>
				<button
					onClick={() => setIsCollapsed(true)}
					className="rounded p-1 text-ink-faint transition-colors hover:bg-app-hover hover:text-ink"
					title="Collapse Panel"
				>
					<HugeiconsIcon icon={Cancel01Icon} className="h-4 w-4" />
				</button>
			</div>

			{activeTab === "files" && (
				<>
					<div className="border-b border-app-line px-3 py-2">
						<div className="relative">
							<HugeiconsIcon
								icon={Search01Icon}
								className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-ink-faint"
							/>
							<input
								type="text"
								placeholder="Search files..."
								value={searchQuery}
								onChange={(e) => setSearchQuery(e.target.value)}
								className="w-full rounded border border-app-line bg-app py-1.5 pl-8 pr-3 text-xs text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
							/>
						</div>
					</div>

					<div className="flex-1 overflow-y-auto py-2">
						{fileTree.length === 0 ? (
							<div className="px-3 py-4 text-center text-xs text-ink-faint">
								No files in graph
							</div>
						) : (
							fileTree.map((n) => (
								<TreeItem
									key={n.id}
									node={n}
									depth={0}
									searchQuery={searchQuery}
									selectedPath={selectedPath}
									expandedPaths={expandedPaths}
									toggleExpanded={toggleExpanded}
									onFileClick={handleFileClick}
								/>
							))
						)}
					</div>
				</>
			)}

			{activeTab === "filters" && (
				<div className="flex-1 overflow-y-auto p-3">
					{/* Node type toggles */}
					<Section title="Node Types" subtitle="Click color to change, click name to toggle">
						<div className="flex flex-col gap-1">
							{FILTERABLE_LABELS.map((label) => {
								const isVisible = visibleLabels.includes(label);
								const color = getNodeColor(label, colorOverrides);
								const icon = getNodeTypeIcon(label);
								return (
									<div
										key={label}
										className={clsx(
											"group flex items-center gap-2.5 rounded px-2 py-1.5 transition-colors",
											isVisible
												? "bg-app text-ink"
												: "text-ink-faint hover:bg-app-hover hover:text-ink-dull",
										)}
									>
										{/* Icon swatch — color tint + type icon */}
										<div
											className={clsx(
												"flex h-5 w-5 shrink-0 items-center justify-center rounded",
												!isVisible && "opacity-40",
											)}
											style={{ backgroundColor: `${color}20` }}
										>
											<HugeiconsIcon
												icon={icon}
												className="h-3 w-3"
												style={{ color }}
											/>
										</div>

										{/* Label name — click to toggle visibility */}
										<button
											className="flex min-w-0 flex-1 cursor-pointer items-center gap-2"
											onClick={() => onToggleLabel(label)}
										>
											<span className="flex-1 text-left text-xs">{label}</span>
										</button>

										{/* Color picker icon — visible on hover */}
										<Popover.Root>
											<Popover.Trigger asChild>
												<button
													className="shrink-0 rounded p-0.5 text-ink-faint/0 transition-all group-hover:text-ink-faint hover:!text-ink hover:!bg-app-hover"
													title="Change node color"
												>
													<svg viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
														<path d="M13.4 1.6a2.1 2.1 0 0 0-3 0L3.3 8.7a1 1 0 0 0-.2.4l-1 3.5a.5.5 0 0 0 .6.6l3.5-1a1 1 0 0 0 .4-.2l7.1-7.1a2.1 2.1 0 0 0 0-3ZM11 3.2l1.8 1.8-5.7 5.7-2.3.6.6-2.3Z"/>
													</svg>
												</button>
											</Popover.Trigger>
											<Popover.Portal>
												<Popover.Content
													side="right"
													sideOffset={8}
													className="z-50 rounded-lg border border-app-line bg-app-darkBox p-2 shadow-xl"
												>
													<NodeColorPicker
														currentColor={color}
														defaultColor={NODE_COLORS[label]}
														onSelect={(c) => onColorChange(label, c)}
														onReset={() => onColorChange(label, null)}
													/>
												</Popover.Content>
											</Popover.Portal>
										</Popover.Root>

										{/* Visibility toggle dot */}
										<div
											className={clsx(
												"h-2 w-2 shrink-0 rounded-full transition-colors",
												isVisible ? "bg-accent" : "bg-app-line",
											)}
										/>
									</div>
								);
							})}
						</div>
					</Section>

					{/* Edge type toggles */}
					<Section title="Edge Types" subtitle="Click color to change, click name to toggle">
						<div className="flex flex-col gap-1">
							{ALL_EDGE_TYPES.map((edge) => {
								const info = EDGE_INFO[edge];
								const isVisible = visibleEdgeTypes.includes(edge);
								const color = getEdgeColor(edge, edgeColorOverrides);
								return (
									<div
										key={edge}
										className={clsx(
											"group flex items-center gap-2.5 rounded px-2 py-1.5 transition-colors",
											isVisible
												? "bg-app text-ink"
												: "text-ink-faint hover:bg-app-hover hover:text-ink-dull",
										)}
									>
										<div
											className={clsx(
												"h-1.5 w-6 rounded-full shrink-0",
												!isVisible && "opacity-40",
											)}
											style={{ backgroundColor: color }}
										/>
										<button
											className="flex min-w-0 flex-1 cursor-pointer items-center gap-2"
											onClick={() => onToggleEdge(edge)}
										>
											<span className="flex-1 text-left text-xs">{info.label}</span>
										</button>

										<Popover.Root>
											<Popover.Trigger asChild>
												<button
													className="shrink-0 rounded p-0.5 text-ink-faint/0 transition-all group-hover:text-ink-faint hover:!text-ink hover:!bg-app-hover"
													title="Change edge color"
												>
													<svg viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
														<path d="M13.4 1.6a2.1 2.1 0 0 0-3 0L3.3 8.7a1 1 0 0 0-.2.4l-1 3.5a.5.5 0 0 0 .6.6l3.5-1a1 1 0 0 0 .4-.2l7.1-7.1a2.1 2.1 0 0 0 0-3ZM11 3.2l1.8 1.8-5.7 5.7-2.3.6.6-2.3Z"/>
													</svg>
												</button>
											</Popover.Trigger>
											<Popover.Portal>
												<Popover.Content
													side="right"
													sideOffset={8}
													className="z-50 rounded-lg border border-app-line bg-app-darkBox p-2 shadow-xl"
												>
													<NodeColorPicker
														currentColor={color}
														defaultColor={info.color}
														onSelect={(c) => onEdgeColorChange(edge, c)}
														onReset={() => onEdgeColorChange(edge, null)}
													/>
												</Popover.Content>
											</Popover.Portal>
										</Popover.Root>

										<div
											className={clsx(
												"h-2 w-2 shrink-0 rounded-full transition-colors",
												isVisible ? "bg-accent" : "bg-app-line",
											)}
										/>
									</div>
								);
							})}
						</div>
					</Section>

					{/* Depth filter */}
					<Section
						title="Focus Depth"
						subtitle="Show nodes within N hops of the selection"
						icon={Target02Icon}
					>
						<div className="flex flex-wrap gap-1.5">
							{[
								{ value: null, label: "All" },
								{ value: 1, label: "1 hop" },
								{ value: 2, label: "2 hops" },
								{ value: 3, label: "3 hops" },
								{ value: 5, label: "5 hops" },
							].map(({ value, label }) => (
								<button
									key={label}
									onClick={() => onChangeDepthFilter(value)}
									className={clsx(
										"rounded px-2 py-1 text-xs transition-colors",
										depthFilter === value
											? "bg-accent text-white"
											: "bg-app text-ink-dull hover:bg-app-hover hover:text-ink",
									)}
								>
									{label}
								</button>
							))}
						</div>
						{depthFilter !== null && !selectedNode && (
							<p className="mt-2 text-[10px] text-amber-400">
								Select a node to apply the depth filter
							</p>
						)}
					</Section>

					{/* Embedded stats charts */}
					<Section title="Graph Stats" subtitle="Node and edge distributions">
						<div className="overflow-hidden">
							<GraphStatsView projectId={projectId} compact />
						</div>
					</Section>
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Small section wrapper used in the Filters tab to give each block a
// consistent heading + spacing.
// ---------------------------------------------------------------------------

function Section({
	title,
	subtitle,
	children,
	icon,
}: {
	title: string;
	subtitle?: string;
	children: React.ReactNode;
	icon?: React.ComponentProps<typeof HugeiconsIcon>["icon"];
}) {
	return (
		<div className="mb-6 border-t border-app-line pt-4 first:border-t-0 first:pt-0">
			<h3 className="mb-1 flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-ink-dull">
				{icon && <HugeiconsIcon icon={icon} className="h-3 w-3" />}
				{title}
			</h3>
			{subtitle && <p className="mb-3 text-[11px] text-ink-faint">{subtitle}</p>}
			{children}
		</div>
	);
}
