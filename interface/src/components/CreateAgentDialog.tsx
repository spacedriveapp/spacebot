import {useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {api, type PresetMeta} from "@/api/client";
import {DialogRoot, DialogContent} from "@spacedrive/primitives";
import {CortexChatPanel} from "@/components/CortexChatPanel";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faRobot,
	faCode,
	faMagnifyingGlass,
	faHeadset,
	faPen,
	faHandshake,
	faBriefcase,
	faUsers,
	faTableColumns,
	faPlus,
} from "@fortawesome/free-solid-svg-icons";
import type {IconDefinition} from "@fortawesome/fontawesome-svg-core";

interface CreateAgentDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	/** The agent ID whose cortex will handle the factory flow. */
	agentId: string;
}

const PRESET_ICONS: Record<string, IconDefinition> = {
	bot: faRobot,
	code: faCode,
	search: faMagnifyingGlass,
	headset: faHeadset,
	pen: faPen,
	handshake: faHandshake,
	briefcase: faBriefcase,
	users: faUsers,
	kanban: faTableColumns,
};

function PresetCard({
	preset,
	onClick,
}: {
	preset: PresetMeta;
	onClick: () => void;
}) {
	const icon = PRESET_ICONS[preset.icon] ?? faRobot;

	return (
		<button
			type="button"
			onClick={onClick}
			className="group flex items-start gap-3 rounded-lg border border-app-line/40 bg-app-dark-box/30 p-3 text-left transition-all hover:border-accent/50 hover:bg-accent/5"
		>
			<div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-app-box/60 text-ink-faint transition-colors group-hover:bg-accent/15 group-hover:text-accent">
				<FontAwesomeIcon icon={icon} className="h-3.5 w-3.5" />
			</div>
			<div className="min-w-0">
				<p className="text-sm font-medium text-ink">{preset.name}</p>
				<p className="mt-0.5 line-clamp-2 text-tiny leading-snug text-ink-faint">
					{preset.description}
				</p>
			</div>
		</button>
	);
}

function ScratchCard({onClick}: {onClick: () => void}) {
	return (
		<button
			type="button"
			onClick={onClick}
			className="group flex items-start gap-3 rounded-lg border border-dashed border-app-line/40 bg-transparent p-3 text-left transition-all hover:border-accent/50 hover:bg-accent/5"
		>
			<div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-app-box/40 text-ink-faint transition-colors group-hover:bg-accent/15 group-hover:text-accent">
				<FontAwesomeIcon icon={faPlus} className="h-3.5 w-3.5" />
			</div>
			<div className="min-w-0">
				<p className="text-sm font-medium text-ink">Start from scratch</p>
				<p className="mt-0.5 line-clamp-2 text-tiny leading-snug text-ink-faint">
					Describe what you need and the cortex will build it.
				</p>
			</div>
		</button>
	);
}

export function CreateAgentDialog({
	open,
	onOpenChange,
	agentId,
}: CreateAgentDialogProps) {
	const [selectedPreset, setSelectedPreset] = useState<string | null>(null);
	const [chatPrompt, setChatPrompt] = useState<string | null>(null);

	const {data} = useQuery({
		queryKey: ["factory-presets"],
		queryFn: api.factoryPresets,
		enabled: open,
	});

	const presets = data?.presets ?? [];

	function handlePresetClick(preset: PresetMeta) {
		setSelectedPreset(preset.id);
		setChatPrompt(
			`I would like to create a new agent using the "${preset.name}" preset (${preset.id}). ` +
				`Load the preset, walk me through customizing it for my needs, and create the agent when ready.`,
		);
	}

	function handleScratchClick() {
		setSelectedPreset("scratch");
		setChatPrompt(
			"I would like to create a new agent from scratch. " +
				"Ask me what I need, recommend a preset if one fits, and walk me through the creation process.",
		);
	}

	function handleClose() {
		setSelectedPreset(null);
		setChatPrompt(null);
	}

	return (
		<DialogRoot
			open={open}
			onOpenChange={(v) => {
				if (!v) handleClose();
				onOpenChange(v);
			}}
		>
			<DialogContent className="!flex h-[80vh] max-h-[800px] max-w-3xl !flex-col !gap-0 overflow-hidden !p-0">
				{!chatPrompt ? (
					/* ---- Preset picker ---- */
					<div className="flex h-full flex-col">
						<div className="border-b border-app-line/50 px-5 py-4">
							<h2 className="font-plex text-base font-semibold text-ink">
								Create Agent
							</h2>
							<p className="mt-0.5 text-sm text-ink-dull">
								Choose a preset to get started, or start from scratch.
							</p>
						</div>
						<div className="flex-1 overflow-y-auto px-5 py-4">
							<div className="grid grid-cols-2 gap-2.5">
								{presets.map((preset) => (
									<PresetCard
										key={preset.id}
										preset={preset}
										onClick={() => handlePresetClick(preset)}
									/>
								))}
								<ScratchCard onClick={handleScratchClick} />
							</div>
						</div>
					</div>
				) : (
					/* ---- Cortex chat with factory context ---- */
					<div className="flex min-h-0 flex-1 flex-col">
						<div className="flex items-center justify-between border-b border-app-line/50 px-6 py-3">
							<div className="flex items-center gap-3">
								<button
									type="button"
									onClick={() => {
										setSelectedPreset(null);
										setChatPrompt(null);
									}}
									className="text-sm text-ink-faint transition-colors hover:text-ink"
								>
									&larr; Back
								</button>
								<span className="text-sm font-medium text-ink">
									{selectedPreset === "scratch"
										? "New Agent"
										: (presets.find((p) => p.id === selectedPreset)?.name ??
											"New Agent")}
								</span>
							</div>
						</div>
						<div className="min-h-0 flex-1">
							<CortexChatPanel
								agentId={agentId}
								initialPrompt={chatPrompt ?? undefined}
								hideHeader
							/>
						</div>
					</div>
				)}
			</DialogContent>
		</DialogRoot>
	);
}
