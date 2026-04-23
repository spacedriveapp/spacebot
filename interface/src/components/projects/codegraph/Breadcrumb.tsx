// In-canvas navigation bar for the mermaid view. Shows "Overview" when
// at the folder-cluster level, and "Overview / {LayerName}" with a back
// chevron when drilled into a layer's detail.

interface Props {
	activeLayerName: string | null;
	onBackToOverview: () => void;
}

export function Breadcrumb({ activeLayerName, onBackToOverview }: Props) {
	if (activeLayerName == null) {
		return (
			<span className="text-[11px] font-medium uppercase tracking-wider text-ink-faint">
				Overview
			</span>
		);
	}
	return (
		<div className="flex items-center gap-1.5 text-[11px]">
			<button
				type="button"
				onClick={onBackToOverview}
				className="flex items-center gap-1 rounded px-1.5 py-0.5 text-ink-dull transition-colors hover:bg-app-hover hover:text-ink"
				title="Back to project overview"
			>
				<svg viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3">
					<path d="M10.3 3.3a1 1 0 0 1 0 1.4L7 8l3.3 3.3a1 1 0 1 1-1.4 1.4l-4-4a1 1 0 0 1 0-1.4l4-4a1 1 0 0 1 1.4 0z" />
				</svg>
				<span className="uppercase tracking-wider">Overview</span>
			</button>
			<span className="text-ink-faint">/</span>
			<span className="font-medium text-ink" title={activeLayerName}>
				{activeLayerName}
			</span>
		</div>
	);
}
