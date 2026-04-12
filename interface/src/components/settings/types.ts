import type {GlobalSettingsResponse} from "@/api/client";

export type SectionId =
	| "instance"
	| "appearance"
	| "providers"
	| "channels"
	| "api-keys"
	| "secrets"
	| "server"
	| "integrations"
	| "worker-logs"
	| "updates"
	| "config-file"
	| "changelog";

export type Platform =
	| "discord"
	| "slack"
	| "telegram"
	| "twitch"
	| "email"
	| "webhook"
	| "mattermost"
	| "signal";

export interface GlobalSettingsSectionProps {
	settings: GlobalSettingsResponse | undefined;
	isLoading: boolean;
}

export interface ChangelogRelease {
	version: string;
	body: string;
}

export interface ProviderCardProps {
	provider: string;
	name: string;
	description: string;
	configured: boolean;
	defaultModel: string;
	onEdit: () => void;
	onRemove: () => void;
	removing: boolean;
	actionLabel?: string;
	showRemove?: boolean;
}

export interface ChatGptOAuthDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	isRequesting: boolean;
	isPolling: boolean;
	message: {text: string; type: "success" | "error"} | null;
	deviceCodeInfo: {userCode: string; verificationUrl: string} | null;
	deviceCodeCopied: boolean;
	onCopyDeviceCode: () => void;
	onOpenDeviceLogin: () => void;
	onRestart: () => void;
}
