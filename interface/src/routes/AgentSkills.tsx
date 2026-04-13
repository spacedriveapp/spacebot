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

	return (
		<div className="flex h-full">
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

			<SkillsSidebar
				activeView={activeView}
				onViewChange={setActiveView}
				installedSkills={installedSkills}
				selectedSkill={selectedSkill}
				onSelectInstalledSkill={handleSelectInstalledSkill}
				isLoading={isSkillsLoading}
			/>

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
		</div>
	);
}
