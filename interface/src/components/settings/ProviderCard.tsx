import {Button} from "@spacedrive/primitives";
import {ProviderIcon} from "@/lib/providerIcons";
import type {ProviderCardProps} from "./types";

export function ProviderCard({
	provider,
	name,
	description,
	configured,
	defaultModel,
	onEdit,
	onRemove,
	removing,
	actionLabel,
	showRemove,
}: ProviderCardProps) {
	const primaryLabel = actionLabel ?? (configured ? "Update" : "Add key");
	const shouldShowRemove = showRemove ?? configured;
	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4">
			<div className="flex items-center gap-3">
				<ProviderIcon provider={provider} size={32} />
				<div className="flex-1">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">{name}</span>
						{configured && (
							<span className="inline-flex items-center">
								<span
									className="h-2 w-2 rounded-full bg-green-400"
									aria-hidden="true"
								/>
								<span className="sr-only">Configured</span>
							</span>
						)}
					</div>
					<p className="mt-0.5 text-sm text-ink-dull">{description}</p>
					<p className="mt-1 text-tiny text-ink-faint">
						Default model: <span className="text-ink-dull">{defaultModel}</span>
					</p>
				</div>
				<div className="flex gap-2">
					<Button onClick={onEdit} variant="outline" size="md">
						{primaryLabel}
					</Button>
					{shouldShowRemove && (
						<Button
							onClick={onRemove}
							variant="outline"
							size="md"
							loading={removing}
						>
							Remove
						</Button>
					)}
				</div>
			</div>
		</div>
	);
}
