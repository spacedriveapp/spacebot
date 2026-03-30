import * as React from "react";
import {FilterButton as SpaceUIFilterButton} from "@spaceui/primitives";

export interface FilterButtonProps
	extends React.ButtonHTMLAttributes<HTMLButtonElement> {
	active?: boolean;
	colorClass?: string;
	children: React.ReactNode;
}

export const FilterButton = React.forwardRef<
	HTMLButtonElement,
	FilterButtonProps
>(({active, colorClass, className, children, ...props}, ref) => (
	<SpaceUIFilterButton
		ref={ref}
		active={active}
		label={typeof children === "string" ? children : ""}
		className={active && colorClass ? `${colorClass} ${className ?? ""}` : className}
		{...props}
	>
		{typeof children === "string" ? undefined : children}
	</SpaceUIFilterButton>
));

FilterButton.displayName = "FilterButton";
