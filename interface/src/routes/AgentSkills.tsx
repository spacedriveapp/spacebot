import {useState, useRef, useCallback} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {
	SkillsSidebar,
	SkillInspector,
	SkillsDirectory,
	BundledSkills,
	CreateSkill,
	type SkillView,
	type SelectedSkill,
} from "@/components/skills";
import type {SkillInfo, RegistrySkill} from "@/api/client";
import {useIsMobile} from "@/hooks/useMediaQuery";
import {Drawer} from "@/ui/Drawer";
import {ListBullets} from "@phosphor-icons/react";

interface AgentSkillsProps {
	agentId: string;
}

export function AgentSkills({agentId}: AgentSkillsProps) {
	const queryClient = useQueryClient();
	const fileInputRef = useRef<HTMLInputElement>(null);

	const [activeView, setActiveView] = useState<SkillView>("directory");
	const [selectedSkill, setSelectedSkill] = useState<SelectedSkill | null>(null);

	const {data: skillsData, isLoading: isSkillsLoading} = useQuery({
		queryKey: ["skills", agentId],
		queryFn: () => api.listSkills(agentId),
		refetchInterval: 10_000,
	});

	const installedSkills = skillsData?.skills ?? [];

	const installedKeys = new Map(
		installedSkills.map((s) => [
			s.source_repo
				? `${s.source_repo}/${s.name}`.toLowerCase()
				: s.name.toLowerCase(),
			s.name,
		]),
	);

	const installedNamesSet = new Set(installedKeys.keys());

	const installMutation = useMutation({
		mutationFn: (spec: string) =>
			api.installSkill({agent_id: agentId, spec, instance: false}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["skills", agentId]});
		},
	});

	const removeMutation = useMutation({
		mutationFn: (name: string) => api.removeSkill({agent_id: agentId, name}),
		onSuccess: (_, removedName) => {
			queryClient.invalidateQueries({queryKey: ["skills", agentId]});
			setSelectedSkill((prev) =>
				prev?.type === "installed" && prev.skill.name === removedName ? null : prev,
			);
		},
	});

	const uploadMutation = useMutation({
		mutationFn: (files: File[]) => api.uploadSkillFiles(agentId, files),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["skills", agentId]});
		},
		onSettled: () => {
			if (fileInputRef.current) fileInputRef.current.value = "";
		},
	});

	const handleSelectInstalledSkill = useCallback((skill: SkillInfo) => {
		setSelectedSkill({type: "installed", skill});
	}, []);

	const handleSelectRegistrySkill = useCallback((skill: RegistrySkill) => {
		setSelectedSkill({type: "registry", skill});
	}, []);

	const isMobile = useIsMobile();
	const [sidebarOpen, setSidebarOpen] = useState(false);

	const handleViewChange = (view: SkillView) => {
		setActiveView(view);
		setSidebarOpen(false);
	};

	const handleMobileSelectInstalled = useCallback(
		(skill: SkillInfo) => {
			handleSelectInstalledSkill(skill);
			setSidebarOpen(false);
		},
		[handleSelectInstalledSkill],
	);

	const skillsSidebar = (onSelect: (s: SkillInfo) => void, onView: (v: SkillView) => void) => (
		<SkillsSidebar
			activeView={activeView}
			onViewChange={onView}
			installedSkills={installedSkills}
			selectedSkill={selectedSkill}
			onSelectInstalledSkill={onSelect}
			isLoading={isSkillsLoading}
		/>
	);

	return (
		<div className="relative flex h-full">
			<input
				ref={fileInputRef}
				type="file"
				accept=".zip,.skill"
				multiple
				className="hidden"
				onChange={(e) => {
					const files = e.target.files;
					if (files && files.length > 0) uploadMutation.mutate(Array.from(files));
				}}
			/>

			{!isMobile && skillsSidebar(handleSelectInstalledSkill, setActiveView)}

			<div className="flex flex-1 flex-col overflow-hidden">
				{activeView === "directory" && (
					<SkillsDirectory
						agentId={agentId}
						installedKeys={installedKeys}
						selectedSkill={selectedSkill}
						onSelectSkill={handleSelectRegistrySkill}
						installMutation={{
							mutate: installMutation.mutate,
							isPending: installMutation.isPending,
							variables: installMutation.variables,
						}}
						removeMutation={{
							mutate: removeMutation.mutate,
							isPending: removeMutation.isPending,
							variables: removeMutation.variables,
						}}
					/>
				)}
				{activeView === "bundled" && (
					<BundledSkills
						installedSkills={installedSkills}
						selectedSkill={selectedSkill}
						onSelectSkill={handleSelectInstalledSkill}
					/>
				)}
				{activeView === "create" && <CreateSkill />}
			</div>

			{selectedSkill && !isMobile && (
				<SkillInspector
					agentId={agentId}
					selected={selectedSkill}
					onClose={() => setSelectedSkill(null)}
					onInstall={(spec) => installMutation.mutate(spec)}
					onRemove={(name) => removeMutation.mutate(name)}
					installedNames={installedNamesSet}
					isInstalling={installMutation.isPending}
					isRemoving={removeMutation.isPending}
					removingName={removeMutation.variables ?? null}
				/>
			)}

			{isMobile && (
				<>
					<button
						type="button"
						aria-label="Open skills sidebar"
						onClick={() => setSidebarOpen(true)}
						className="fixed bottom-4 right-4 z-40 flex h-12 w-12 items-center justify-center rounded-full border border-app-line bg-app-box text-ink shadow-lg active:bg-app-selected"
					>
						<ListBullets size={20} weight="bold" />
					</button>
					<Drawer
						open={sidebarOpen}
						onOpenChange={setSidebarOpen}
						side="left"
						ariaLabel="Skills"
					>
						{skillsSidebar(handleMobileSelectInstalled, handleViewChange)}
					</Drawer>
					<Drawer
						open={!!selectedSkill}
						onOpenChange={(open) => {
							if (!open) setSelectedSkill(null);
						}}
						side="right"
						ariaLabel="Skill details"
					>
						{selectedSkill && (
							<SkillInspector
								agentId={agentId}
								selected={selectedSkill}
								onClose={() => setSelectedSkill(null)}
								onInstall={(spec) => installMutation.mutate(spec)}
								onRemove={(name) => removeMutation.mutate(name)}
								installedNames={installedNamesSet}
								isInstalling={installMutation.isPending}
								isRemoving={removeMutation.isPending}
								removingName={removeMutation.variables ?? null}
							/>
						)}
					</Drawer>
				</>
			)}
		</div>
	);
}
