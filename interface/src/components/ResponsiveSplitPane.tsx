import type { ReactNode } from "react";
import { useViewport } from "@/hooks/useViewport";
import { Button } from "@/ui";

interface ResponsiveSplitPaneProps {
	primary: ReactNode;
	secondary: ReactNode;
	showSecondary: boolean;
	onCloseSecondary?: () => void;
	secondaryTitle?: string;
	secondaryWidthClassName?: string;
}

export function ResponsiveSplitPane({
	primary,
	secondary,
	showSecondary,
	onCloseSecondary,
	secondaryTitle = "Details",
	secondaryWidthClassName = "w-[min(400px,40%)]",
}: ResponsiveSplitPaneProps) {
	const { isMobile, isTablet } = useViewport();
	const isSinglePane = isMobile || isTablet;

	if (!isSinglePane) {
		return (
			<div className="flex h-full">
				<div className="min-w-0 flex-1">{primary}</div>
				{showSecondary && (
					<div className={`shrink-0 overflow-hidden border-l border-app-line/50 ${secondaryWidthClassName}`}>
						{secondary}
					</div>
				)}
			</div>
		);
	}

	if (showSecondary) {
		return (
			<div className="flex h-full flex-col">
				<div className="flex h-11 items-center gap-2 border-b border-app-line/50 bg-app-darkBox/30 px-3">
					{onCloseSecondary && (
						<Button
							type="button"
							variant="ghost"
							size="sm"
							onClick={onCloseSecondary}
						>
							Back
						</Button>
					)}
					<span className="truncate text-sm text-ink-dull">{secondaryTitle}</span>
				</div>
				<div className="min-h-0 flex-1">{secondary}</div>
			</div>
		);
	}

	return <div className="flex h-full min-h-0 flex-col">{primary}</div>;
}
