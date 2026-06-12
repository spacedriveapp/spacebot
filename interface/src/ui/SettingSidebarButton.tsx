import * as React from "react";
import {cx} from "class-variance-authority";

export interface SettingSidebarButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
	active?: boolean;
	children: React.ReactNode;
}

export const SettingSidebarButton = React.forwardRef<
	HTMLButtonElement,
	SettingSidebarButtonProps
>(({active, className, children, ...props}, ref) => (
	<button
		ref={ref}
		className={cx(
			"flex items-center gap-2 rounded-md px-2.5 py-2 text-left text-sm transition-colors",
			active
				? "bg-app-dark-box text-ink"
				: "text-ink-dull hover:bg-app-dark-box/50 hover:text-ink",
			className,
		)}
		{...props}
	>
		{children}
	</button>
));

SettingSidebarButton.displayName = "SettingSidebarButton";
