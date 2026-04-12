interface PortalEmptyProps {
	agentName: string;
}

export function PortalEmpty({ agentName }: PortalEmptyProps) {
	return (
		<div className="mb-6 text-left">
			<h1 className="text-ink text-[2.65rem] font-semibold tracking-tight">
				Let&apos;s get to work
			</h1>
			<p className="text-ink-dull mt-2 text-sm">
				Start a conversation with {agentName}.
			</p>
		</div>
	);
}
