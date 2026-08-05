import {Button} from "@spacedrive/primitives";
import {ProviderIcon} from "@/lib/providerIcons";
import type {ProviderCardProps} from "./types";

const API_TYPE_LABELS: Record<string, string> = {
	anthropic: "Anthropic Messages API",
	openai_compatible: "OpenAI-compatible",
};

export function ProviderCard({
	provider,
	apiType,
	baseUrl,
	displayName,
	hasKey,
	onEdit,
	onRemove,
	removing,
}: ProviderCardProps) {
	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4">
			<div className="flex items-center gap-3">
				<ProviderIcon provider={provider} size={32} />
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">
							{displayName || provider}
						</span>
						<span className="rounded bg-app-dark-box px-1.5 py-0.5 text-tiny text-ink-faint">
							{API_TYPE_LABELS[apiType] ?? apiType}
						</span>
						{hasKey && (
							<span className="inline-flex items-center">
								<span
									className="h-2 w-2 rounded-full bg-status-success"
									aria-hidden="true"
								/>
								<span className="sr-only">Credential resolved</span>
							</span>
						)}
					</div>
					<p className="mt-0.5 truncate text-sm text-ink-dull">{baseUrl}</p>
					<p className="mt-1 text-tiny text-ink-faint">
						Models route as{" "}
						<span className="text-ink-dull">{provider}/&lt;model&gt;</span>
						{!hasKey && (
							<span className="text-status-error">
								{" "}
								&middot; no API key resolved
							</span>
						)}
					</p>
				</div>
				<div className="flex gap-2">
					<Button onClick={onEdit} variant="outline" size="md">
						Edit
					</Button>
					<Button
						onClick={onRemove}
						variant="outline"
						size="md"
						loading={removing}
					>
						Remove
					</Button>
				</div>
			</div>
		</div>
	);
}
