import {
	CircleButton,
	PopoverRoot,
	PopoverTrigger,
	PopoverContent,
	Button,
} from "@spacedrive/primitives";
import {GearSix, ClockCounterClockwise} from "@phosphor-icons/react";
import {useState} from "react";
import {WorkersPanelContent} from "@/components/WorkersPanel";
import {ConversationSettingsPanel} from "@/components/ConversationSettingsPanel";
import {PortalHistoryPopover} from "./PortalHistoryPopover";
import type {
	ConversationDefaultsResponse,
	ConversationSettings,
} from "@/api/client";
import type {PortalConversationSummary} from "@/api/types";
import type {ActiveWorker} from "@/hooks/useChannelLiveState";

interface PortalHeaderProps {
	title: string;
	modelLabel?: string;
	responseMode?: string;
	activeWorkers?: ActiveWorker[];

	// Settings popover
	showSettings: boolean;
	onToggleSettings: (open: boolean) => void;
	defaults: ConversationDefaultsResponse | undefined;
	defaultsLoading: boolean;
	defaultsError: Error | null;
	settings: ConversationSettings;
	onSettingsChange: (settings: ConversationSettings) => void;
	onSaveSettings: () => void;
	saving: boolean;

	// Conversation actions
	conversations: PortalConversationSummary[];
	activeConversationId: string;
	onNewConversation: () => void;
	onSelectConversation: (id: string) => void;
	onDeleteConversation: (id: string) => void;
	onArchiveConversation: (id: string, archived: boolean) => void;
	showHistory: boolean;
	onToggleHistory: (open: boolean) => void;
}

export function PortalHeader({
	title,
	modelLabel,
	responseMode,
	activeWorkers = [],
	showSettings,
	onToggleSettings,
	defaults,
	defaultsLoading,
	defaultsError,
	settings,
	onSettingsChange,
	onSaveSettings,
	saving,
	conversations,
	activeConversationId,
	onNewConversation,
	onSelectConversation,
	onDeleteConversation,
	onArchiveConversation,
	showHistory,
	onToggleHistory,
}: PortalHeaderProps) {
	const [showWorkers, setShowWorkers] = useState(false);

	return (
		<div className="flex items-center justify-between border-b border-app-line bg-app px-4 py-2">
			<div className="flex items-center gap-2">
				<h2 className="text-sm font-medium text-ink">{title}</h2>
				{modelLabel && (
					<span className="text-xs text-ink-faint">{modelLabel}</span>
				)}
				{responseMode === "observe" && (
					<span className="rounded-md bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-400">
						Observe
					</span>
				)}
				{responseMode === "mention_only" && (
					<span className="rounded-md bg-red-500/10 px-1.5 py-0.5 text-[10px] font-medium text-red-400">
						Mention Only
					</span>
				)}
				{activeWorkers.length > 0 && (
					<PopoverRoot open={showWorkers} onOpenChange={setShowWorkers}>
						<PopoverTrigger asChild>
							<Button variant="gray" size="md">
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
								{activeWorkers.length} worker
								{activeWorkers.length !== 1 ? "s" : ""}
							</Button>
						</PopoverTrigger>
						<PopoverContent
							align="start"
							sideOffset={8}
							collisionPadding={16}
							className="w-[420px] p-0"
						>
							<WorkersPanelContent />
						</PopoverContent>
					</PopoverRoot>
				)}
			</div>

			<div className="flex items-center gap-2">
				<Button variant="gray" size="md" onClick={onNewConversation}>
					New conversation
				</Button>

				<PopoverRoot open={showHistory} onOpenChange={onToggleHistory}>
					<PopoverTrigger asChild>
						<CircleButton icon={ClockCounterClockwise} title="History" />
					</PopoverTrigger>
					<PopoverContent
						align="end"
						sideOffset={4}
						collisionPadding={16}
						className="w-80 p-0"
					>
						<PortalHistoryPopover
							conversations={conversations}
							activeConversationId={activeConversationId}
							onSelect={(id) => {
								onSelectConversation(id);
								onToggleHistory(false);
							}}
							onDelete={onDeleteConversation}
							onArchive={onArchiveConversation}
						/>
					</PopoverContent>
				</PopoverRoot>

				<PopoverRoot open={showSettings} onOpenChange={onToggleSettings}>
					<PopoverTrigger asChild>
						<CircleButton icon={GearSix} title="Settings" />
					</PopoverTrigger>
					<PopoverContent
						align="end"
						sideOffset={4}
						collisionPadding={16}
						className="max-h-[80vh] w-96 overflow-y-auto p-3"
					>
						{defaultsLoading ? (
							<div className="py-4 text-center text-xs text-ink-faint">
								Loading...
							</div>
						) : defaults ? (
							<ConversationSettingsPanel
								defaults={defaults}
								currentSettings={settings}
								onChange={onSettingsChange}
								onSave={onSaveSettings}
								onCancel={() => onToggleSettings(false)}
								saving={saving}
							/>
						) : (
							<div className="py-4 text-center text-xs text-red-400">
								{defaultsError?.message ?? "Failed to load settings"}
							</div>
						)}
					</PopoverContent>
				</PopoverRoot>
			</div>
		</div>
	);
}
