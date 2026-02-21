import * as React from "react";
import { cx } from "./utils";

export interface SettingSidebarButtonProps
	extends React.ButtonHTMLAttributes<HTMLButtonElement> {
	active?: boolean;
	children: React.ReactNode;
}

export const SettingSidebarButton = React.forwardRef<
	HTMLButtonElement,
	SettingSidebarButtonProps
>(({ active, className, children, ...props }, ref) => (
	<button
		ref={ref}
		className={cx(
			"flex w-full items-center gap-2 rounded-md py-1.5 text-left text-[13px] transition-colors duration-150 relative",
			active
				? "bg-accent/10 text-accent font-medium pl-[9px] border-l-2 border-accent"
				: "text-ink-dull hover:bg-white/[0.04] hover:text-ink pl-[11px]",
			className
		)}
		{...props}
	>
		{children}
	</button>
));

SettingSidebarButton.displayName = "SettingSidebarButton";
