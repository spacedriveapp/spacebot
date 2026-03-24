import { useState, useEffect, useRef, useCallback } from "react";
import { useQuery, useMutation, useQueryClient, useInfiniteQuery } from "@tanstack/react-query";
import { api, type SkillInfo, type RegistrySkill, type RegistryView } from "@/api/client";
import { Button, Badge, Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/ui";
import { clsx } from "clsx";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import {
	faDownload,
	faTrash,
	faSearch,
	faSpinner,
	faCheckCircle,
	faExternalLinkAlt,
	faFire,
	faTrophy,
	faBolt,
	faUpload,
} from "@fortawesome/free-solid-svg-icons";

interface AgentSkillsProps {
	agentId: string;
}

function formatInstalls(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
	return String(n);
}

/**
 * Derive the install spec from a registry skill.
 *
 * The spec is always "owner/repo/skill-name" (3-part format required by backend).
 * Even for single-skill repos, the skillId is included.
 */
function installSpec(skill: RegistrySkill): string {
	return `${skill.source}/${skill.skillId}`;
}

/** Modal for viewing an installed skill's full SKILL.md content. */
function InstalledSkillDetailModal({
	agentId,
	skill,
	open,
	onOpenChange,
}: {
	agentId: string;
	skill: SkillInfo | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}) {
	const { data, isLoading } = useQuery({
		queryKey: ["skill-content", agentId, skill?.name],
		queryFn: () => api.getSkillContent(agentId, skill!.name),
		enabled: open && !!skill,
	});

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
				<DialogHeader>
					<div className="flex items-center gap-2">
						<DialogTitle>{skill?.name ?? "Skill"}</DialogTitle>
						{skill && (
							<Badge variant={skill.source === "instance" ? "accent" : "green"} size="sm">
								{skill.source}
							</Badge>
						)}
					</div>
					<DialogDescription>
						{skill?.description || "No description provided"}
					</DialogDescription>
				</DialogHeader>

				{isLoading && (
					<div className="flex items-center justify-center py-8">
						<FontAwesomeIcon icon={faSpinner} className="animate-spin text-ink-faint" />
						<span className="ml-2 text-sm text-ink-faint">Loading skill content...</span>
					</div>
				)}

				{data && (
					<div className="space-y-4">
						{data.source_repo && (
							<div className="flex items-center gap-2 text-xs text-ink-dull">
								<span className="font-medium text-ink-faint">Source:</span>
								<a
									href={`https://github.com/${data.source_repo}`}
									target="_blank"
									rel="noopener noreferrer"
									className="font-mono transition-colors hover:text-accent"
								>
									{data.source_repo}
									<FontAwesomeIcon icon={faExternalLinkAlt} className="ml-1 text-[10px]" />
								</a>
							</div>
						)}
						<div className="flex items-center gap-2 text-xs text-ink-dull">
							<span className="font-medium text-ink-faint">Path:</span>
							<span className="font-mono break-all">{data.base_dir}</span>
						</div>
						<SkillContentBlock content={data.content} />
					</div>
				)}
			</DialogContent>
		</Dialog>
	);
}

/** Modal for viewing a registry (non-installed) skill's full SKILL.md content. */
function RegistrySkillDetailModal({
	skill,
	open,
	onOpenChange,
	onInstall,
	isInstalled,
	isInstalling,
}: {
	skill: RegistrySkill | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onInstall: () => void;
	isInstalled: boolean;
	isInstalling: boolean;
}) {
	const { data, isLoading } = useQuery({
		queryKey: ["registry-skill-content", skill?.source, skill?.skillId],
		queryFn: () => api.registrySkillContent(skill!.source, skill!.skillId),
		enabled: open && !!skill,
	});

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
				<DialogHeader>
					<div className="flex items-center gap-2">
						<DialogTitle>{skill?.name ?? "Skill"}</DialogTitle>
						{skill && !isInstalled && (
							<Button
								variant="default"
								size="sm"
								onClick={(e) => {
									e.stopPropagation();
									onInstall();
								}}
								disabled={isInstalling}
								className="ml-auto"
							>
								{isInstalling ? (
									<>
										<FontAwesomeIcon icon={faSpinner} className="animate-spin" />
										Installing...
									</>
								) : (
									<>
										<FontAwesomeIcon icon={faDownload} />
										Install
									</>
								)}
							</Button>
						)}
						{isInstalled && (
							<Badge variant="green" size="sm" className="ml-auto">
								Installed
							</Badge>
						)}
					</div>
					<DialogDescription>
						{skill?.description || "No description provided"}
					</DialogDescription>
				</DialogHeader>

				{skill && (
					<div className="flex items-center gap-2 text-xs text-ink-dull">
						<span className="font-medium text-ink-faint">Source:</span>
						<a
							href={`https://github.com/${skill.source}`}
							target="_blank"
							rel="noopener noreferrer"
							className="font-mono transition-colors hover:text-accent"
						>
							{skill.source}
							<FontAwesomeIcon icon={faExternalLinkAlt} className="ml-1 text-[10px]" />
						</a>
						{skill.installs > 0 && (
							<>
								<span className="text-ink-dull/40">|</span>
								<span>{formatInstalls(skill.installs)} installs</span>
							</>
						)}
					</div>
				)}

				{isLoading && (
					<div className="flex items-center justify-center py-8">
						<FontAwesomeIcon icon={faSpinner} className="animate-spin text-ink-faint" />
						<span className="ml-2 text-sm text-ink-faint">Loading skill content...</span>
					</div>
				)}

				{data && !data.content && !isLoading && (
					<div className="rounded-md border border-app-line bg-app-darkBox p-6 text-center">
						<p className="text-sm text-ink-faint">
							Could not fetch SKILL.md from GitHub.
						</p>
					</div>
				)}

				{data?.content && <SkillContentBlock content={data.content} />}
			</DialogContent>
		</Dialog>
	);
}

/** Shared component that renders a SKILL.md content block. */
function SkillContentBlock({ content }: { content: string }) {
	return (
		<div className="rounded-md border border-app-line bg-app-darkBox">
			<div className="border-b border-app-line px-4 py-2">
				<span className="font-mono text-xs font-medium text-ink-faint">SKILL.md</span>
			</div>
			<pre className="overflow-x-auto whitespace-pre-wrap p-4 font-mono text-xs leading-relaxed text-ink-dull">
				{content}
			</pre>
		</div>
	);
}

function InstalledSkill({
	skill,
	onClick,
	onRemove,
	isRemoving,
}: {
	skill: SkillInfo;
	onClick: () => void;
	onRemove: () => void;
	isRemoving: boolean;
}) {
	return (
		<div
			className="flex cursor-pointer flex-col rounded-lg border border-app-line bg-app-box p-4 transition-colors hover:border-app-line-hover"
			onClick={onClick}
		>
			<div className="flex items-center justify-between gap-3">
				<div className="min-w-0 flex-1">
					<div className="flex min-w-0 items-center gap-2">
						<h3 className="truncate font-plex text-sm font-medium text-ink">
							{skill.name}
						</h3>
						<Badge variant={skill.source === "instance" ? "accent" : "green"} size="sm">
							{skill.source}
						</Badge>
					</div>
				</div>
				<Button
					variant="outline"
					size="icon"
					onClick={(e) => {
						e.stopPropagation();
						onRemove();
					}}
					disabled={isRemoving}
					className="h-6 w-6 flex-shrink-0"
				>
					<FontAwesomeIcon
						icon={isRemoving ? faSpinner : faTrash}
						className={clsx("text-[11px]", isRemoving && "animate-spin")}
					/>
				</Button>
			</div>
			<p className="mt-2 text-xs text-ink-faint">
				{skill.description || "No description provided"}
			</p>
			<p className="mt-3 font-mono text-xs text-ink-dull break-all">
				{skill.base_dir}
			</p>
		</div>
	);
}

function RegistrySkillCard({
	skill,
	isInstalled,
	onClick,
	onInstall,
	onRemove,
	isInstalling,
	isRemoving,
}: {
	skill: RegistrySkill;
	isInstalled: boolean;
	onClick: () => void;
	onInstall: () => void;
	onRemove: () => void;
	isInstalling: boolean;
	isRemoving: boolean;
}) {
	return (
		<div
			className="flex cursor-pointer flex-col rounded-lg border border-app-line bg-app-box p-4 transition-colors hover:border-app-line-hover"
			onClick={onClick}
		>
			<div className="flex items-center gap-2">
				<h3 className="truncate font-plex text-sm font-medium text-ink">
					{skill.name}
				</h3>
			</div>
			<p className="mt-1 font-mono text-[11px] text-ink-dull/60">{skill.source}</p>
			<p className="mt-2 flex-1 text-xs text-ink-faint">
				{skill.description || "No description provided"}
			</p>
			<div className="mt-auto flex items-center justify-between gap-2 pt-3">
				<span className="text-xs text-ink-faint">
					{formatInstalls(skill.installs)} installs
				</span>
				<Button
					variant="outline"
					size="icon"
					onClick={(e) => {
						e.stopPropagation();
						if (isInstalled) {
							onRemove();
						} else {
							onInstall();
						}
					}}
					disabled={isInstalling || isRemoving}
					className={clsx(
						"group h-6 w-6 p-0",
					)}
					title={isInstalled ? "Remove installed skill" : "Install skill"}
				>
					{isInstalling || isRemoving ? (
						<FontAwesomeIcon icon={faSpinner} className="animate-spin text-xs" />
					) : isInstalled ? (
						<span className="relative flex h-3.5 w-3.5 items-center justify-center text-xs">
							<FontAwesomeIcon
								icon={faCheckCircle}
								className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 transition-all duration-150 ease-out group-hover:scale-75 group-hover:opacity-0"
							/>
							<FontAwesomeIcon
								icon={faTrash}
								className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 scale-75 opacity-0 transition-all duration-150 ease-out group-hover:scale-100 group-hover:opacity-100"
							/>
						</span>
					) : (
						<FontAwesomeIcon icon={faDownload} className="text-xs" />
					)}
				</Button>
			</div>
		</div>
	);
}

const VIEWS: { key: RegistryView; label: string; icon: typeof faFire }[] = [
	{ key: "all-time", label: "All Time", icon: faTrophy },
	{ key: "trending", label: "Trending", icon: faBolt },
	{ key: "hot", label: "Hot", icon: faFire },
];

export function AgentSkills({ agentId }: AgentSkillsProps) {
	const queryClient = useQueryClient();
	const [searchQuery, setSearchQuery] = useState("");
	const [debouncedSearch, setDebouncedSearch] = useState("");
	const [activeTab, setActiveTab] = useState<"installed" | "browse">("browse");
	const [registryView, setRegistryView] = useState<RegistryView>("all-time");
	const scrollRef = useRef<HTMLDivElement>(null);
	const fileInputRef = useRef<HTMLInputElement>(null);

	// Installed skill detail modal state
	const [selectedSkill, setSelectedSkill] = useState<SkillInfo | null>(null);
	const [detailModalOpen, setDetailModalOpen] = useState(false);

	// Registry skill detail modal state
	const [selectedRegistrySkill, setSelectedRegistrySkill] = useState<RegistrySkill | null>(null);
	const [registryDetailOpen, setRegistryDetailOpen] = useState(false);

	// Debounce search input
	useEffect(() => {
		if (searchQuery.length === 0) {
			setDebouncedSearch("");
			return;
		}
		if (searchQuery.length < 2) return;
		const timer = setTimeout(
			() => setDebouncedSearch(searchQuery),
			Math.max(150, 350 - 50 * searchQuery.length),
		);
		return () => clearTimeout(timer);
	}, [searchQuery]);

	// Installed skills
	const { data: skillsData, isLoading } = useQuery({
		queryKey: ["skills", agentId],
		queryFn: () => api.listSkills(agentId),
		refetchInterval: 10_000,
	});

	// Registry browse with infinite scroll
	const {
		data: browseData,
		fetchNextPage,
		hasNextPage,
		isFetchingNextPage,
		isLoading: isBrowseLoading,
	} = useInfiniteQuery({
		queryKey: ["registry-browse", registryView],
		queryFn: ({ pageParam }) => api.registryBrowse(registryView, pageParam),
		initialPageParam: 0,
		getNextPageParam: (lastPage, _allPages, lastPageParam) =>
			lastPage.has_more ? lastPageParam + 1 : undefined,
		enabled: activeTab === "browse" && !debouncedSearch,
	});

	// Registry search
	const { data: searchData, isLoading: isSearching } = useQuery({
		queryKey: ["registry-search", debouncedSearch],
		queryFn: () => api.registrySearch(debouncedSearch),
		enabled: activeTab === "browse" && debouncedSearch.length >= 2,
	});

	// Infinite scroll handler
	const handleScroll = useCallback(() => {
		const el = scrollRef.current;
		if (!el || !hasNextPage || isFetchingNextPage || debouncedSearch) return;
		const { scrollTop, scrollHeight, clientHeight } = el;
		if (scrollHeight - scrollTop - clientHeight < 400) {
			fetchNextPage();
		}
	}, [hasNextPage, isFetchingNextPage, fetchNextPage, debouncedSearch]);

	useEffect(() => {
		const el = scrollRef.current;
		if (!el) return;
		el.addEventListener("scroll", handleScroll);
		return () => el.removeEventListener("scroll", handleScroll);
	}, [handleScroll]);

	const installMutation = useMutation({
		mutationFn: (spec: string) =>
			api.installSkill({
				agent_id: agentId,
				spec,
				instance: false,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["skills", agentId] });
		},
	});

	const githubInstallMutation = useMutation({
		mutationFn: (spec: string) =>
			api.installSkill({
				agent_id: agentId,
				spec,
				instance: false,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["skills", agentId] });
		},
	});

	const removeMutation = useMutation({
		mutationFn: (name: string) =>
			api.removeSkill({
				agent_id: agentId,
				name,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["skills", agentId] });
		},
	});

	const uploadMutation = useMutation({
		mutationFn: (files: File[]) => api.uploadSkillFiles(agentId, files),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["skills", agentId] });
		},
		onSettled: () => {
			if (fileInputRef.current) {
				fileInputRef.current.value = "";
			}
		},
	});

	const handleFileUpload = useCallback(
		(files: FileList | null) => {
			if (!files || files.length === 0) return;
			uploadMutation.mutate(Array.from(files));
		},
		[uploadMutation],
	);

	const installedSkills = skillsData?.skills ?? [];
	const installedKeys = new Map(
		installedSkills.map((s) => [
			s.source_repo
				? `${s.source_repo}/${s.name}`.toLowerCase()
				: s.name.toLowerCase(),
			s.name,
		]),
	);

	// Flatten browse pages or use search results
	const registrySkills: RegistrySkill[] = debouncedSearch
		? (searchData?.skills ?? [])
		: (browseData?.pages.flatMap((p) => p.skills) ?? []);

	const isRegistryLoading = debouncedSearch ? isSearching : isBrowseLoading;
	const totalRegistrySkills = browseData?.pages[0]?.total;

	return (
		<div className="flex h-full flex-col">
			{/* Installed skill detail modal */}
			<InstalledSkillDetailModal
				agentId={agentId}
				skill={selectedSkill}
				open={detailModalOpen}
				onOpenChange={(open) => {
					setDetailModalOpen(open);
					if (!open) setSelectedSkill(null);
				}}
			/>

			{/* Registry skill detail modal */}
			<RegistrySkillDetailModal
				skill={selectedRegistrySkill}
				open={registryDetailOpen}
				onOpenChange={(open) => {
					setRegistryDetailOpen(open);
					if (!open) setSelectedRegistrySkill(null);
				}}
				onInstall={() => {
					if (selectedRegistrySkill) {
						installMutation.mutate(installSpec(selectedRegistrySkill));
					}
				}}
				isInstalled={
					selectedRegistrySkill
						? Boolean(
								installedKeys.get(
									`${selectedRegistrySkill.source}/${selectedRegistrySkill.name}`.toLowerCase(),
								) ?? installedKeys.get(selectedRegistrySkill.name.toLowerCase()),
							)
						: false
				}
				isInstalling={
					selectedRegistrySkill
						? installMutation.isPending &&
							installMutation.variables === installSpec(selectedRegistrySkill)
						: false
				}
			/>

			{/* Hidden file input for upload */}
			<input
				ref={fileInputRef}
				type="file"
				accept=".zip,.skill"
				multiple
				className="hidden"
				onChange={(e) => handleFileUpload(e.target.files)}
			/>

			{/* Header with tabs */}
			<div className="border-b border-app-line">
				<div className="flex items-center gap-1 px-6 py-3">
					<button
						onClick={() => setActiveTab("browse")}
						className={clsx(
							"rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
							activeTab === "browse"
								? "bg-app-line text-ink"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						Browse Registry
					</button>
					<button
						onClick={() => setActiveTab("installed")}
						className={clsx(
							"rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
							activeTab === "installed"
								? "bg-app-line text-ink"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						Installed ({installedSkills.length})
					</button>
					<div className="flex-1" />
					<a
						href="https://skills.sh"
						target="_blank"
						rel="noopener noreferrer"
						className="flex items-center gap-2 text-xs text-ink-faint transition-colors hover:text-accent"
					>
						<span>skills.sh</span>
						<FontAwesomeIcon icon={faExternalLinkAlt} className="text-xs" />
					</a>
				</div>

				{activeTab === "browse" && (
					<div className="border-t border-app-line px-6 py-3">
						<div className="flex items-center gap-3">
							<div className="relative flex-1">
								<FontAwesomeIcon
									icon={faSearch}
									className="absolute left-3 top-1/2 -translate-y-1/2 text-ink-faint"
								/>
								<input
									type="text"
									value={searchQuery}
									onChange={(e) => setSearchQuery(e.target.value)}
									placeholder="Search skills..."
									className="w-full rounded-md border border-app-line bg-app-darkBox py-2 pl-10 pr-3 text-sm text-ink placeholder-ink-faint focus:border-accent focus:outline-none"
								/>
							</div>
							{!debouncedSearch && (
								<div className="flex items-center gap-1">
									{VIEWS.map((v) => (
										<button
											key={v.key}
											onClick={() => setRegistryView(v.key)}
											className={clsx(
												"flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors",
												registryView === v.key
													? "bg-app-line text-ink"
													: "text-ink-faint hover:text-ink-dull",
											)}
										>
											<FontAwesomeIcon icon={v.icon} className="text-[10px]" />
											{v.label}
										</button>
									))}
								</div>
							)}
						</div>
					</div>
				)}
			</div>

			{/* Content */}
			<div ref={scrollRef} className="flex-1 overflow-y-auto">
				<div className="p-6">
					{activeTab === "browse" && (
						<div className="space-y-4">
							<div className="rounded-lg border border-app-line bg-app-box p-6">
								<h3 className="text-sm font-medium text-ink">
									Install from GitHub
								</h3>
								<p className="mt-1 text-xs text-ink-faint">
									Install any skill from a GitHub repository
								</p>
								<form
									onSubmit={(e) => {
										e.preventDefault();
										const formData = new FormData(e.currentTarget);
										const spec = formData.get("spec") as string;
										if (spec) {
											githubInstallMutation.mutate(spec);
											e.currentTarget.reset();
										}
									}}
									className="mt-3 flex gap-2"
								>
									<input
										type="text"
										name="spec"
										placeholder="owner/repo or owner/repo/skill-name"
										className="flex-1 rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder-ink-faint focus:border-accent focus:outline-none"
									/>
									<Button
										type="submit"
										variant="default"
										size="default"
										disabled={githubInstallMutation.isPending}
									>
										{githubInstallMutation.isPending ? (
											<>
												<FontAwesomeIcon
													icon={faSpinner}
													className="animate-spin"
												/>
												Installing...
											</>
										) : (
											<>
												<FontAwesomeIcon icon={faDownload} />
												Install
											</>
										)}
									</Button>
								</form>
								{githubInstallMutation.isError && (
									<p className="mt-2 text-xs text-red-400">
										Failed to install skill. Check the repository format.
									</p>
								)}
								{githubInstallMutation.isSuccess && (
									<p className="mt-2 text-xs text-green-400">
										Installed {githubInstallMutation.data.installed.length} skill(s):{" "}
										{githubInstallMutation.data.installed.join(", ")}
									</p>
								)}
							</div>

							<div className="flex items-center justify-between">
								<h2 className="text-sm font-medium text-ink-dull">
									{debouncedSearch
										? `Results for "${debouncedSearch}"`
										: `${VIEWS.find((v) => v.key === registryView)?.label ?? ""} Skills`}
								</h2>
							<span className="text-xs text-ink-faint">
								{debouncedSearch && searchData
									? `${searchData.count} results`
									: totalRegistrySkills != null
										? `${totalRegistrySkills} skills`
										: registrySkills.length > 0
											? `${registrySkills.length} skills`
											: ""}
							</span>
							</div>

							{isRegistryLoading && registrySkills.length === 0 && (
								<div className="rounded-lg border border-app-line bg-app-box p-8 text-center">
									<FontAwesomeIcon
										icon={faSpinner}
										className="animate-spin text-ink-faint"
									/>
									<p className="mt-2 text-sm text-ink-faint">
										Loading skills from registry...
									</p>
								</div>
							)}

							{!isRegistryLoading && registrySkills.length === 0 && debouncedSearch && (
								<div className="rounded-lg border border-app-line bg-app-box p-8 text-center">
									<p className="text-sm text-ink-faint">
										No skills found matching "{debouncedSearch}"
									</p>
								</div>
							)}

							<div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
								{registrySkills.map((skill) => {
									const spec = installSpec(skill);
									const compositeKey = `${skill.source}/${skill.name}`.toLowerCase();
									const installedName =
										installedKeys.get(compositeKey) ??
										installedKeys.get(skill.name.toLowerCase());
									const isInstalled = Boolean(installedName);
									return (
										<RegistrySkillCard
											key={`${skill.source}/${skill.skillId}`}
											skill={skill}
											isInstalled={isInstalled}
											onClick={() => {
												setSelectedRegistrySkill(skill);
												setRegistryDetailOpen(true);
											}}
											onInstall={() =>
												installMutation.mutate(spec)
											}
											onRemove={() => {
												if (installedName) {
													removeMutation.mutate(installedName);
												}
											}}
											isInstalling={
												installMutation.isPending &&
												installMutation.variables === spec
											}
											isRemoving={
												removeMutation.isPending &&
												removeMutation.variables === installedName
											}
										/>
									);
								})}
							</div>

							{isFetchingNextPage && (
								<div className="py-4 text-center">
									<FontAwesomeIcon
										icon={faSpinner}
										className="animate-spin text-ink-faint"
									/>
									<span className="ml-2 text-xs text-ink-faint">
										Loading more...
									</span>
								</div>
							)}

						</div>
					)}

					{activeTab === "installed" && (
						<div className="space-y-4">
							<div className="flex items-center justify-between">
								<h2 className="text-sm font-medium text-ink-dull">
									Installed Skills
								</h2>
								<div className="flex items-center gap-3">
									<span className="text-xs text-ink-faint">
										{installedSkills.length} skills
									</span>
									<Button
										variant="outline"
										size="default"
										onClick={() => fileInputRef.current?.click()}
										disabled={uploadMutation.isPending}
									>
										{uploadMutation.isPending ? (
											<>
												<FontAwesomeIcon icon={faSpinner} className="animate-spin" />
												Uploading...
											</>
										) : (
											<>
												<FontAwesomeIcon icon={faUpload} />
												Upload Skills
											</>
										)}
									</Button>
								</div>
							</div>

							{uploadMutation.isError && (
								<p className="text-xs text-red-400">
									Failed to upload skill. Make sure the file is a valid .zip or .skill archive.
								</p>
							)}
							{uploadMutation.isSuccess && (
								<p className="text-xs text-green-400">
									Uploaded {uploadMutation.data.installed.length} skill(s):{" "}
									{uploadMutation.data.installed.join(", ")}
								</p>
							)}

							{isLoading && (
								<div className="rounded-lg border border-app-line bg-app-box p-8 text-center">
									<FontAwesomeIcon
										icon={faSpinner}
										className="animate-spin text-ink-faint"
									/>
									<p className="mt-2 text-sm text-ink-faint">
										Loading skills...
									</p>
								</div>
							)}

							{!isLoading && installedSkills.length === 0 && (
								<div className="rounded-lg border border-app-line bg-app-box p-8 text-center">
									<p className="text-sm text-ink-faint">
										No skills installed yet
									</p>
									<Button
										variant="default"
										size="default"
										onClick={() => setActiveTab("browse")}
										className="mt-4"
									>
										<FontAwesomeIcon icon={faSearch} />
										Browse Skills
									</Button>
								</div>
							)}

							<div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
								{installedSkills.map((skill) => (
									<InstalledSkill
										key={skill.name}
										skill={skill}
										onClick={() => {
											setSelectedSkill(skill);
											setDetailModalOpen(true);
										}}
										onRemove={() => removeMutation.mutate(skill.name)}
										isRemoving={
											removeMutation.isPending &&
											removeMutation.variables === skill.name
										}
									/>
								))}
							</div>
						</div>
					)}
				</div>
			</div>
		</div>
	);
}
