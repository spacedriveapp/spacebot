import type {GlobalSettingsResponse} from "@/api/client";

export type SectionId =
	| "instance"
	| "appearance"
	| "providers"
	| "channels"
	| "api-keys"
	| "secrets"
	| "server"
	| "opencode"
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
	/** Provider id — the prefix in `provider/model` routing strings. */
	provider: string;
	/** `"anthropic"` or `"openai_compatible"`. */
	apiType: string;
	baseUrl: string;
	displayName?: string | null;
	hasKey: boolean;
	onEdit: () => void;
	onRemove: () => void;
	removing: boolean;
}
