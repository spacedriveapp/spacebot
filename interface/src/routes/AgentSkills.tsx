import { useState, useEffect, useRef, useCallback } from "react";
import { useQuery, useMutation, useQueryClient, useInfiniteQuery } from "@tanstack/react-query";
import { api, type SkillInfo, type RegistrySkill, type RegistryView } from "@/api/client";
import { Button, Badge } from "@/ui";
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
 * For multi-skill repos (e.g. anthropics/skills with skill "frontend-design"),
 * the spec is "owner/repo/skill-name". For single-skill repos where the repo
 * name matches the skillId (e.g. vercel-labs/agent-browser), use "owner/repo"
 * so the installer scans the whole repo for SKILL.md files.
 */
function installSpec(skill: RegistrySkill): string {
	const repoName = skill.source.split("/").pop();
	if (repoName === skill.skillId) {
		return skill.source;
	}
	return `${skill.source}/${skill.skillId}`;
}

function InstalledSkill({
	skill,
	onRemove,
	isRemoving,
}: {
	skill: SkillInfo;
	onRemove: () => void;
	isRemoving: boolean;
}) {
	return (
		<div className="flex flex-col rounded-lg border border-app-line bg-app-box p-4 transition-colors hover:border-app-line-hover">
			<div className="flex items-start justify-between gap-3">
				<div className="flex-1">
					<div className="flex items-center gap-2">
						<h3 className="font-plex text-sm font-medium text-ink">
							{skill.name}
						</h3>
						<Badge variant={skill.source === "instance" ? "accent" : "green"} size="sm">
							{skill.source}
						</Badge>
					</div>
					<p className="mt-2 text-xs text-ink-faint">
						{skill.description || "No description provided"}
					</p>
				</div>
				<Button
					variant="ghost"
					size="sm"
					onClick={onRemove}
					disabled={isRemoving}
					className="text-red-400 hover:text-red-300"
				>
					<FontAwesomeIcon
						icon={isRemoving ? faSpinner : faTrash}
						className={isRemoving ? "animate-spin" : ""}
					/>
				</Button>
			</div>
			<p className="mt-3 font-mono text-xs text-ink-dull">
				{skill.base_dir}
			</p>
		</div>
	);
}

function RegistrySkillCard({
	skill,
	isInstalled,
	onInstall,
	isInstalling,
}: {
	skill: RegistrySkill;
	isInstalled: boolean;
	onInstall: () => void;
	isInstalling: boolean;
}) {
	return (
		<div className="flex flex-col rounded-lg border border-app-line bg-app-box p-4 transition-colors hover:border-app-line-hover">
			<div className="flex items-center gap-2">
				<h3 className="truncate font-plex text-sm font-medium text-ink">
					{skill.name}
				</h3>
				{isInstalled && (
					<FontAwesomeIcon
						icon={faCheckCircle}
						className="shrink-0 text-xs text-green-400"
					/>
				)}
			</div>
			<p className="mt-1 font-mono text-[11px] text-ink-dull/60">{skill.source}</p>
			<div className="mt-auto flex items-center justify-between gap-2 pt-3">
				<span className="text-xs text-ink-faint">
					{formatInstalls(skill.installs)} installs
				</span>
				<Button
					variant={isInstalled ? "ghost" : "outline"}
					size="icon"
					onClick={onInstall}
					disabled={isInstalling || isInstalled}
				>
					<FontAwesomeIcon
						icon={isInstalling ? faSpinner : faDownload}
						className={clsx("text-xs", isInstalling && "animate-spin")}
					/>
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

	const installedSkills = skillsData?.skills ?? [];
	const installedNames = new Set(installedSkills.map((s) => s.name.toLowerCase()));

	// Flatten browse pages or use search results
	const registrySkills: RegistrySkill[] = debouncedSearch
		? (searchData?.skills ?? [])
		: (browseData?.pages.flatMap((p) => p.skills) ?? []);

	const isRegistryLoading = debouncedSearch ? isSearching : isBrowseLoading;

	return (
		<div className="flex h-full flex-col">
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
							<div className="flex items-center justify-between">
								<h2 className="text-sm font-medium text-ink-dull">
									{debouncedSearch
										? `Results for "${debouncedSearch}"`
										: `${VIEWS.find((v) => v.key === registryView)?.label ?? ""} Skills`}
								</h2>
								<span className="text-xs text-ink-faint">
									{debouncedSearch && searchData
										? `${searchData.count} results`
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
									const isInstalled = installedNames.has(
										skill.name.toLowerCase(),
									);
									return (
										<RegistrySkillCard
											key={`${skill.source}/${skill.skillId}`}
											skill={skill}
											isInstalled={isInstalled}
											onInstall={() =>
												installMutation.mutate(spec)
											}
											isInstalling={
												installMutation.isPending &&
												installMutation.variables === spec
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

							<div className="mt-8 rounded-lg border border-app-line bg-app-box p-6">
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
											installMutation.mutate(spec);
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
										disabled={installMutation.isPending}
									>
										{installMutation.isPending ? (
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
								{installMutation.isError && (
									<p className="mt-2 text-xs text-red-400">
										Failed to install skill. Check the repository format.
									</p>
								)}
								{installMutation.isSuccess && (
									<p className="mt-2 text-xs text-green-400">
										Installed {installMutation.data.installed.length} skill(s):{" "}
										{installMutation.data.installed.join(", ")}
									</p>
								)}
							</div>
						</div>
					)}

					{activeTab === "installed" && (
						<div className="space-y-4">
							<div className="flex items-center justify-between">
								<h2 className="text-sm font-medium text-ink-dull">
									Installed Skills
								</h2>
								<span className="text-xs text-ink-faint">
									{installedSkills.length} skills
								</span>
							</div>

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
