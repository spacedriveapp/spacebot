import * as React from "react";
import * as SwitchPrimitives from "@radix-ui/react-switch";
import {cva, type VariantProps} from "class-variance-authority";
import {cx} from "./utils";

const switchStyles = cva(
	[
		"peer inline-flex shrink-0 cursor-pointer items-center rounded-full transition-all duration-200",
		"focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 focus-visible:ring-offset-2 focus-visible:ring-offset-app",
		"disabled:cursor-not-allowed disabled:opacity-50",
		"data-[state=checked]:bg-gradient-accent data-[state=checked]:shadow-glow-accent data-[state=unchecked]:bg-app-darkBox",
	],
	{
		variants: {
			size: {
				sm: "h-4 w-7 p-0.5",
				md: "h-5 w-9 p-0.5",
				lg: "h-6 w-11 p-0.5",
			},
		},
		defaultVariants: {
			size: "md",
		},
	},
);

const thumbStyles = cva(
	[
		"pointer-events-none block rounded-full bg-white shadow-elevation-1 ring-0 transition-transform duration-200",
	],
	{
		variants: {
			size: {
				sm: "h-3 w-3 data-[state=checked]:translate-x-3 data-[state=unchecked]:translate-x-0",
				md: "h-4 w-4 data-[state=checked]:translate-x-4 data-[state=unchecked]:translate-x-0",
				lg: "h-5 w-5 data-[state=checked]:translate-x-5 data-[state=unchecked]:translate-x-0",
			},
		},
		defaultVariants: {
			size: "md",
		},
	},
);

export interface ToggleProps
	extends
		React.ComponentPropsWithoutRef<typeof SwitchPrimitives.Root>,
		VariantProps<typeof switchStyles> {}

export const Toggle = React.forwardRef<
	React.ElementRef<typeof SwitchPrimitives.Root>,
	ToggleProps
>(({className, size, ...props}, ref) => (
	<SwitchPrimitives.Root
		className={cx(switchStyles({size}), className)}
		{...props}
		ref={ref}
	>
		<SwitchPrimitives.Thumb className={cx(thumbStyles({size}))} />
	</SwitchPrimitives.Root>
));

Toggle.displayName = SwitchPrimitives.Root.displayName;
