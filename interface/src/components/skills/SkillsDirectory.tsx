import {useState, useEffect, useRef, useCallback} from "react";
import {useQuery, useInfiniteQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {useVirtualizer} from "@tanstack/react-virtual";
import {MagnifyingGlass, ArrowSquareOut, DownloadSimple} from "@phosphor-icons/react";
import {Button} from "@spacedrive/primitives";
import {api, type RegistrySkill, type RegistryView} from "@/api/client";
import {RegistrySkillRow} from "./RegistrySkillRow";
import {REGISTRY_VIEWS} from "./constants";
import type {SelectedSkill} from "./types";

interface SkillsDirectoryProps {
	agentId: string;
	installedKeys: Map<string, string>;
	selectedSkill: SelectedSkill | null;
	onSelectSkill: (skill: RegistrySkill) => void;
	installMutation: {
		mutate: (spec: string) => void;
		isPending: boolean;
		variables: string | undefined;
	};
	removeMutation: {
		mutate: (name: string) => void;
		isPending: boolean;
		variables: string | undefined;
	};
}

export function SkillsDirectory({
	agentId,
	installedKeys,
	selectedSkill,
	onSelectSkill,
	installMutation,
	removeMutation,
}: SkillsDirectoryProps) {
	const queryClient = useQueryClient();
	const [searchQuery, setSearchQuery] = useState("");
	const [debouncedSearch, setDebouncedSearch] = useState("");
	const [registryView, setRegistryView] = useState<RegistryView>("all-time");
	const scrollRef = useRef<HTMLDivElement>(null);

	// Debounce search
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

	const {
		data: browseData,
		fetchNextPage,
		hasNextPage,
		isFetchingNextPage,
		isLoading: isBrowseLoading,
	} = useInfiniteQuery({
		queryKey: ["registry-browse", registryView],
		queryFn: ({pageParam}) => api.registryBrowse(registryView, pageParam),
		initialPageParam: 0,
		getNextPageParam: (lastPage, _allPages, lastPageParam) =>
			lastPage.has_more ? lastPageParam + 1 : undefined,
		enabled: !debouncedSearch,
	});

	const {data: searchData, isLoading: isSearching} = useQuery({
		queryKey: ["registry-search", debouncedSearch],
		queryFn: () => api.registrySearch(debouncedSearch),
		enabled: debouncedSearch.length >= 2,
	});

	const githubInstallMutation = useMutation({
		mutationFn: (spec: string) =>
			api.installSkill({agent_id: agentId, spec, instance: false}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["skills", agentId]});
		},
	});

	const handleScroll = useCallback(() => {
		const el = scrollRef.current;
		if (!el || !hasNextPage || isFetchingNextPage || debouncedSearch) return;
		const {scrollTop, scrollHeight, clientHeight} = el;
		if (scrollHeight - scrollTop - clientHeight < 400) fetchNextPage();
	}, [hasNextPage, isFetchingNextPage, fetchNextPage, debouncedSearch]);

	useEffect(() => {
		const el = scrollRef.current;
		if (!el) return;
		el.addEventListener("scroll", handleScroll);
		return () => el.removeEventListener("scroll", handleScroll);
	}, [handleScroll]);

	const registrySkills: RegistrySkill[] = debouncedSearch
		? (searchData?.skills ?? [])
		: (browseData?.pages.flatMap((p) => p.skills) ?? []);

	const isLoading = debouncedSearch ? isSearching : isBrowseLoading;
	const totalCount = browseData?.pages[0]?.total;

	const virtualizer = useVirtualizer({
		count: registrySkills.length,
		getScrollElement: () => scrollRef.current,
		estimateSize: () => 56,
		overscan: 5,
	});

	const selectedRegistryId =
		selectedSkill?.type === "registry"
			? `${selectedSkill.skill.source}/${selectedSkill.skill.skillId}`
			: null;

	return (
		<div className="flex h-full flex-col">
			{/* Toolbar */}
			<div className="flex items-center gap-3 border-b border-app-line/50 px-5 py-3">
				<div className="relative flex-1">
					<MagnifyingGlass className="absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-ink-faint" weight="bold" />
					<input
						type="text"
						value={searchQuery}
						onChange={(e) => setSearchQuery(e.target.value)}
						placeholder="Search skills..."
						className="w-full rounded-md border border-app-line bg-app-dark-box py-1.5 pl-9 pr-3 text-sm text-ink placeholder-ink-faint focus:border-accent focus:outline-none"
					/>
				</div>
				{!debouncedSearch && (
					<div className="flex items-center gap-1">
						{REGISTRY_VIEWS.map((v) => (
							<button
								key={v.key}
								onClick={() => setRegistryView(v.key)}
								className={`rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors ${
									registryView === v.key
										? "bg-app-line text-ink"
										: "text-ink-faint hover:text-ink-dull"
								}`}
							>
								{v.label}
							</button>
						))}
					</div>
				)}
				<a
					href="https://skills.sh"
					target="_blank"
					rel="noopener noreferrer"
					className="flex items-center gap-1.5 text-xs text-ink-faint transition-colors hover:text-accent"
				>
					skills.sh
					<ArrowSquareOut className="size-3" weight="bold" />
				</a>
			</div>

			{/* GitHub install form */}
			<div className="border-b border-app-line/50 px-5 py-3">
				<form
					onSubmit={(e) => {
						e.preventDefault();
						const data = new FormData(e.currentTarget);
						const spec = data.get("spec") as string;
						if (spec) {
							githubInstallMutation.mutate(spec);
							e.currentTarget.reset();
						}
					}}
					className="flex gap-2"
				>
					<input
						type="text"
						name="spec"
						placeholder="Install from GitHub: owner/repo or owner/repo/skill-name"
						className="flex-1 rounded-md border border-app-line bg-app-dark-box px-3 py-1.5 text-sm text-ink placeholder-ink-faint focus:border-accent focus:outline-none"
					/>
					<Button
						type="submit"
						variant="outline"
						size="sm"
						disabled={githubInstallMutation.isPending}
					>
						<DownloadSimple className="size-3.5" weight="bold" />
						{githubInstallMutation.isPending ? "Installing..." : "Install"}
					</Button>
				</form>
				{githubInstallMutation.isError && (
					<p className="mt-1.5 text-xs text-red-400">
						Failed to install. Check the repository format.
					</p>
				)}
				{githubInstallMutation.isSuccess && (
					<p className="mt-1.5 text-xs text-green-400">
						Installed: {githubInstallMutation.data.installed.join(", ")}
					</p>
				)}
			</div>

			{/* Count bar */}
			<div className="flex items-center justify-between px-5 py-2">
				<span className="text-xs text-ink-faint">
					{debouncedSearch
						? `Results for "${debouncedSearch}"`
						: `${REGISTRY_VIEWS.find((v) => v.key === registryView)?.label} Skills`}
				</span>
				<span className="text-xs text-ink-dull">
					{debouncedSearch && searchData
						? `${searchData.count} results`
						: totalCount != null
							? `${totalCount} skills`
							: registrySkills.length > 0
								? `${registrySkills.length} skills`
								: ""}
				</span>
			</div>

			{/* Virtualized list */}
			<div ref={scrollRef} className="flex-1 overflow-y-auto px-3 pb-4">
				{isLoading && registrySkills.length === 0 && (
					<div className="flex items-center gap-2 px-2 py-6 text-xs text-ink-faint">
						<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
						Loading from registry...
					</div>
				)}
				{!isLoading && registrySkills.length === 0 && debouncedSearch && (
					<p className="px-2 py-6 text-xs text-ink-faint">
						No skills found for "{debouncedSearch}"
					</p>
				)}
				{registrySkills.length > 0 && (
					<div
						style={{
							height: `${virtualizer.getTotalSize()}px`,
							width: "100%",
							position: "relative",
						}}
					>
						{virtualizer.getVirtualItems().map((virtualItem) => {
							const skill = registrySkills[virtualItem.index]!;
							const spec = `${skill.source}/${skill.skillId}`;
							const compositeKey = `${skill.source}/${skill.name}`.toLowerCase();
							const installedName =
								installedKeys.get(compositeKey) ??
								installedKeys.get(skill.name.toLowerCase());
							const isInstalled = Boolean(installedName);
							return (
								<div
									key={virtualItem.key}
									style={{
										position: "absolute",
										top: 0,
										left: 0,
										width: "100%",
										height: `${virtualItem.size}px`,
										transform: `translateY(${virtualItem.start}px)`,
									}}
								>
									<RegistrySkillRow
										skill={skill}
										isInstalled={isInstalled}
										isSelected={selectedRegistryId === spec}
										isInstalling={
											installMutation.isPending && installMutation.variables === spec
										}
										isRemoving={
											removeMutation.isPending &&
											removeMutation.variables === installedName
										}
										onClick={() => onSelectSkill(skill)}
										onInstall={() => installMutation.mutate(spec)}
										onRemove={() => {
											if (installedName) removeMutation.mutate(installedName);
										}}
									/>
								</div>
							);
						})}
					</div>
				)}
				{isFetchingNextPage && (
					<div className="flex items-center gap-2 py-4 text-xs text-ink-faint">
						<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
						Loading more...
					</div>
				)}
			</div>
		</div>
	);
}
