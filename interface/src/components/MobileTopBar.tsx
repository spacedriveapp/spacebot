import {List} from "@phosphor-icons/react";
import clsx from "clsx";
import type {ReactNode} from "react";

interface MobileTopBarProps {
	title?: string;
	onMenu: () => void;
	leading?: ReactNode;
	trailing?: ReactNode;
	className?: string;
}

export function MobileTopBar({
	title,
	onMenu,
	leading,
	trailing,
	className,
}: MobileTopBarProps) {
	return (
		<header
			className={clsx(
				"flex h-12 shrink-0 items-center gap-2 border-b border-app-line bg-sidebar px-2",
				className,
			)}
		>
			{leading ?? (
				<button
					type="button"
					aria-label="Open menu"
					onClick={onMenu}
					className="flex h-11 w-11 items-center justify-center rounded-lg text-ink hover:bg-app-box/60 active:bg-app-box/80"
				>
					<List size={20} weight="regular" />
				</button>
			)}
			<div className="min-w-0 flex-1 truncate text-sm font-medium text-ink">
				{title}
			</div>
			{trailing && <div className="flex items-center gap-1">{trailing}</div>}
		</header>
	);
}
