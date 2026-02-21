import * as React from "react";
import { cx } from "./utils";

export interface FilterButtonProps
	extends React.ButtonHTMLAttributes<HTMLButtonElement> {
	active?: boolean;
	colorClass?: string;
	children: React.ReactNode;
}

export const FilterButton = React.forwardRef<
	HTMLButtonElement,
	FilterButtonProps
>(({ active, colorClass, className, children, ...props }, ref) => (
	<button
		ref={ref}
		className={cx(
			"h-6 rounded-md px-2.5 text-tiny font-medium transition-all duration-200 ease-out",
			active
				? colorClass || "bg-app-selected text-ink shadow-sm"
				: "text-ink-faint hover:text-ink-dull hover:bg-app-hover/50 active:scale-[0.97]",
			className
		)}
		{...props}
	>
		{children}
	</button>
));

FilterButton.displayName = "FilterButton";
