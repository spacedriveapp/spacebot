import type {AgentConfigResponse, AgentConfigUpdateRequest} from "@/api/client";

export type SectionId =
	| "general"
	| "soul"
	| "identity"
	| "role"
	| "routing"
	| "tuning"
	| "compaction"
	| "cortex"
	| "coalesce"
	| "memory"
	| "browser"
	| "sandbox"
	| "projects"
	| "ingest";

export interface AgentConfigSection {
	id: SectionId;
	label: string;
	group: "general" | "identity" | "config" | "data";
	description: string;
	detail: string;
}

export interface SaveHandlerRef {
	save?: () => void;
	revert?: () => void;
}

export interface GeneralEditorProps {
	agentId: string;
	displayName: string;
	role: string;
	gradientStart: string;
	gradientEnd: string;
	detail: string;
	onDirtyChange: (dirty: boolean) => void;
	saveHandlerRef: React.MutableRefObject<SaveHandlerRef>;
	onSave: (update: {
		display_name?: string;
		role?: string;
		gradient_start?: string;
		gradient_end?: string;
	}) => void;
}

export interface IdentityEditorProps {
	label: string;
	description: string;
	content: string | null;
	onDirtyChange: (dirty: boolean) => void;
	saveHandlerRef: React.MutableRefObject<SaveHandlerRef>;
	onSave: (value: string) => void;
}

export interface ConfigSectionEditorProps {
	sectionId: SectionId;
	label: string;
	description: string;
	detail: string;
	config: AgentConfigResponse;
	onDirtyChange: (dirty: boolean) => void;
	saveHandlerRef: React.MutableRefObject<SaveHandlerRef>;
	onSave: (update: Partial<AgentConfigUpdateRequest>) => void;
}
