import {useState} from "react";
import {
	Button,
	Popover,
	SelectPill,
	OptionList,
	OptionListItem,
} from "@spacedrive/primitives";
import type {
	ConversationSettings,
	ConversationDefaultsResponse,
} from "@/api/types";

interface ConversationSettingsPanelProps {
	defaults: ConversationDefaultsResponse;
	currentSettings: ConversationSettings;
	onChange: (settings: ConversationSettings) => void;
	onSave: () => void;
	onCancel?: () => void;
	saving?: boolean;
}

const PRESETS: Array<{
	id: string;
	name: string;
	description: string;
	settings: ConversationSettings;
}> = [
	{
		id: "chat",
		name: "Chat",
		description: "Full memory, delegates heavy work to workers",
		settings: {
			memory: "full",
			delegation: "standard",
			worker_context: {history: "none", memory: "none"},
		},
	},
	{
		id: "focus",
		name: "Focus",
		description: "Agent can see memories but won't create new ones",
		settings: {
			memory: "ambient",
			delegation: "standard",
			worker_context: {history: "none", memory: "none"},
		},
	},
	{
		id: "hands-on",
		name: "Hands-on",
		description:
			"Agent has direct tool access, workers get conversation context",
		settings: {
			memory: "off",
			delegation: "direct",
			worker_context: {history: "recent", memory: "tools"},
		},
	},
	{
		id: "quick",
		name: "Quick",
		description: "No memory, no frills — fast stateless responses",
		settings: {
			memory: "off",
			delegation: "standard",
			worker_context: {history: "none", memory: "none"},
		},
	},
];

const MEMORY_OPTIONS = [
	{value: "full", label: "On"},
	{value: "ambient", label: "Context Only"},
	{value: "off", label: "Off"},
] as const;

const MEMORY_DESCRIPTIONS: Record<string, string> = {
	full: "Agent reads and writes memories. Conversations are remembered long-term.",
	ambient:
		"Agent can see its memories but won't save new ones. Good for sensitive or throwaway chats.",
	off: "No memory at all. The agent only knows its base personality.",
};

const DELEGATION_OPTIONS = [
	{value: "standard", label: "Standard"},
	{value: "direct", label: "Direct"},
] as const;

const DELEGATION_DESCRIPTIONS: Record<string, string> = {
	standard:
		"Agent delegates tasks to background workers. Stays responsive while work happens in parallel.",
	direct:
		"Agent has direct access to shell, files, browser, and memory tools. Power-user mode.",
};

const RESPONSE_MODE_OPTIONS = [
	{value: "active", label: "Active"},
	{value: "observe", label: "Observe"},
	{value: "mention_only", label: "Mention Only"},
] as const;

const RESPONSE_MODE_DESCRIPTIONS: Record<string, string> = {
	active: "Responds to all messages normally.",
	observe:
		"Learns from the conversation passively — never responds, even when mentioned. Use this for channels the agent should monitor without participating.",
	mention_only:
		"Messages are still visible to the agent for context, but it only responds when explicitly @mentioned, replied to, or given a command. To block messages entirely, use the binding-level require mention setting instead.",
};

const WORKER_HISTORY_OPTIONS = [
	{value: "none", label: "None"},
	{value: "summary", label: "Summary"},
	{value: "recent", label: "Recent (20)"},
	{value: "full", label: "Full"},
] as const;

const WORKER_MEMORY_OPTIONS = [
	{value: "none", label: "None"},
	{value: "ambient", label: "Read-only"},
	{value: "tools", label: "Can search"},
	{value: "full", label: "Full access"},
] as const;

/** Group models by provider for the select dropdown. */
function groupModelsByProvider(
	models: ConversationDefaultsResponse["available_models"],
) {
	const groups: Record<string, typeof models> = {};
	for (const model of models) {
		const key = model.provider;
		if (!groups[key]) groups[key] = [];
		groups[key].push(model);
	}
	return groups;
}

function SettingField({
	label,
	description,
	children,
}: {
	label: string;
	description?: string;
	children: React.ReactNode;
}) {
	return (
		<div className="flex flex-col gap-1">
			<div className="flex items-center justify-between gap-3">
				<label className="shrink-0 text-xs text-ink-dull">{label}</label>
				<div className="flex w-40 justify-end">{children}</div>
			</div>
			{description && (
				<p className="text-[11px] leading-snug text-ink-faint">{description}</p>
			)}
		</div>
	);
}

function SettingRow({
	label,
	children,
}: {
	label: string;
	children: React.ReactNode;
}) {
	return (
		<div className="flex items-center justify-between gap-3">
			<label className="shrink-0 text-xs text-ink-dull">{label}</label>
			<div className="flex w-40 justify-end">{children}</div>
		</div>
	);
}

/** Popover + SelectPill + OptionList wrapper for simple option lists. */
function SettingSelect<T extends string>({
	value,
	options,
	onChange,
}: {
	value: T;
	options: ReadonlyArray<{value: T; label: string}>;
	onChange: (value: T) => void;
}) {
	const [open, setOpen] = useState(false);
	const selectedLabel = options.find((o) => o.value === value)?.label ?? value;

	return (
		<Popover.Root open={open} onOpenChange={setOpen}>
			<Popover.Trigger asChild>
				<SelectPill size="sm">{selectedLabel}</SelectPill>
			</Popover.Trigger>
			<Popover.Content
				align="end"
				sideOffset={4}
				className="min-w-[160px] p-1.5"
			>
				<OptionList>
					{options.map((opt) => (
						<OptionListItem
							key={opt.value}
							selected={opt.value === value}
							size="sm"
							onClick={() => {
								onChange(opt.value);
								setOpen(false);
							}}
						>
							{opt.label}
						</OptionListItem>
					))}
				</OptionList>
			</Popover.Content>
		</Popover.Root>
	);
}

/** Model picker with provider groups. */
function ModelSelect({
	value,
	modelGroups,
	onChange,
	inheritOption,
}: {
	value: string;
	modelGroups: Record<string, ConversationDefaultsResponse["available_models"]>;
	onChange: (value: string) => void;
	inheritOption?: boolean;
}) {
	const [open, setOpen] = useState(false);

	const allModels = Object.values(modelGroups).flat();
	const selectedLabel =
		value === "__inherit__"
			? "Inherit"
			: (allModels.find((m) => m.id === value)?.name ?? value);

	return (
		<Popover.Root open={open} onOpenChange={setOpen}>
			<Popover.Trigger asChild>
				<SelectPill size="sm">{selectedLabel}</SelectPill>
			</Popover.Trigger>
			<Popover.Content
				align="end"
				sideOffset={4}
				className="max-h-60 min-w-[200px] overflow-y-auto p-1.5"
			>
				<OptionList>
					{inheritOption && (
						<OptionListItem
							selected={value === "__inherit__"}
							size="sm"
							className="italic"
							onClick={() => {
								onChange("__inherit__");
								setOpen(false);
							}}
						>
							Inherit
						</OptionListItem>
					)}
					{Object.entries(modelGroups).map(([provider, models]) => (
						<div key={provider}>
							<div className="px-2.5 py-1 text-[10px] font-semibold uppercase tracking-wider text-ink-faint">
								{provider}
							</div>
							{models.map((m) => (
								<OptionListItem
									key={m.id}
									selected={m.id === value}
									size="sm"
									onClick={() => {
										onChange(m.id);
										setOpen(false);
									}}
								>
									{m.name}
								</OptionListItem>
							))}
						</div>
					))}
				</OptionList>
			</Popover.Content>
		</Popover.Root>
	);
}

export function ConversationSettingsPanel({
	defaults,
	currentSettings,
	onChange,
	onSave,
	onCancel,
	saving,
}: ConversationSettingsPanelProps) {
	const [showAdvanced, setShowAdvanced] = useState(false);
	const modelGroups = groupModelsByProvider(defaults.available_models);

	const currentMemory = currentSettings.memory || defaults.memory;
	const currentDelegation = currentSettings.delegation || defaults.delegation;
	const currentResponseMode = currentSettings.response_mode || "active";

	return (
		<div className="flex flex-col gap-3">
			{/* Presets */}
			<div className="flex gap-1.5">
				{PRESETS.map((preset) => (
					<button
						key={preset.id}
						onClick={() => onChange({...currentSettings, ...preset.settings})}
						title={preset.description}
						className="rounded-md border border-app-line bg-app-dark-box px-2.5 py-1 text-xs text-ink-dull transition-colors hover:border-accent/40 hover:text-ink"
					>
						{preset.name}
					</button>
				))}
			</div>

			{/* Core settings */}
			<div className="flex flex-col gap-3">
				<SettingField
					label="Model"
					description="Which LLM powers this conversation. Inherited by workers and branches unless overridden below."
				>
					<ModelSelect
						value={currentSettings.model || defaults.model}
						modelGroups={modelGroups}
						onChange={(value) => onChange({...currentSettings, model: value})}
					/>
				</SettingField>

				<SettingField
					label="Memory"
					description={MEMORY_DESCRIPTIONS[currentMemory]}
				>
					<SettingSelect
						value={currentMemory}
						options={MEMORY_OPTIONS}
						onChange={(value) =>
							onChange({
								...currentSettings,
								memory: value as ConversationSettings["memory"],
							})
						}
					/>
				</SettingField>

				<SettingField
					label="Mode"
					description={DELEGATION_DESCRIPTIONS[currentDelegation]}
				>
					<SettingSelect
						value={currentDelegation}
						options={DELEGATION_OPTIONS}
						onChange={(value) =>
							onChange({
								...currentSettings,
								delegation: value as ConversationSettings["delegation"],
							})
						}
					/>
				</SettingField>

				<SettingField
					label="Response"
					description={RESPONSE_MODE_DESCRIPTIONS[currentResponseMode]}
				>
					<SettingSelect
						value={currentResponseMode}
						options={RESPONSE_MODE_OPTIONS}
						onChange={(value) =>
							onChange({
								...currentSettings,
								response_mode: value as ConversationSettings["response_mode"],
							})
						}
					/>
				</SettingField>
			</div>

			{/* Advanced toggle */}
			<button
				onClick={() => setShowAdvanced(!showAdvanced)}
				className="self-start text-[11px] text-ink-faint transition-colors hover:text-ink-dull"
			>
				{showAdvanced ? "Hide" : "Show"} advanced
			</button>

			{showAdvanced && (
				<div className="flex flex-col gap-3 border-t border-app-line pt-2.5">
					{/* Per-process model overrides */}
					<SettingField
						label="Model overrides"
						description="Use a different model for a specific process. 'Inherit' uses the model set above."
					>
						<div />
					</SettingField>
					{(["channel", "branch", "worker"] as const).map((process) => (
						<SettingRow
							key={process}
							label={process.charAt(0).toUpperCase() + process.slice(1)}
						>
							<ModelSelect
								value={
									currentSettings.model_overrides?.[process] || "__inherit__"
								}
								modelGroups={modelGroups}
								inheritOption
								onChange={(value) => {
									const overrides = {
										...currentSettings.model_overrides,
									};
									if (value === "__inherit__") {
										delete overrides[process];
									} else {
										overrides[process] = value;
									}
									onChange({
										...currentSettings,
										model_overrides: overrides,
									});
								}}
							/>
						</SettingRow>
					))}

					<SettingField
						label="Worker context"
						description="What information spawned workers receive. More context = smarter workers but slower startup."
					>
						<div />
					</SettingField>
					<SettingRow label="History">
						<SettingSelect
							value={
								currentSettings.worker_context?.history ||
								defaults.worker_context.history
							}
							options={WORKER_HISTORY_OPTIONS}
							onChange={(value) =>
								onChange({
									...currentSettings,
									worker_context: {
										...currentSettings.worker_context,
										history: value as any,
									},
								})
							}
						/>
					</SettingRow>

					<SettingRow label="Memory">
						<SettingSelect
							value={
								currentSettings.worker_context?.memory ||
								defaults.worker_context.memory
							}
							options={WORKER_MEMORY_OPTIONS}
							onChange={(value) =>
								onChange({
									...currentSettings,
									worker_context: {
										...currentSettings.worker_context,
										memory: value as any,
									},
								})
							}
						/>
					</SettingRow>
				</div>
			)}

			{/* Actions */}
			<div className="flex items-center justify-end gap-2 pt-1">
				{onCancel && (
					<Button variant="gray" onClick={onCancel}>
						Cancel
					</Button>
				)}
				<Button variant="accent" onClick={onSave} disabled={saving}>
					{saving ? "Saving..." : "Apply"}
				</Button>
			</div>
		</div>
	);
}
