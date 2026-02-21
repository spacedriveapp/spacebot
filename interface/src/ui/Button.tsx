import * as React from "react";
import {cva, type VariantProps} from "class-variance-authority";
import {cx} from "./utils";
import {Loader} from "./Loader";

export const buttonStyles = cva(
	[
		"inline-flex items-center justify-center rounded-lg font-medium",
		"transition-all duration-150 active:scale-[0.97]",
		"disabled:pointer-events-none disabled:opacity-50",
		"focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 focus-visible:ring-offset-2 focus-visible:ring-offset-app",
	],
	{
		variants: {
			size: {
				default: "h-9 px-4 py-2 text-sm",
				sm: "h-8 rounded-md px-3 text-xs",
				lg: "h-10 rounded-md px-8 text-sm",
				icon: "h-8 w-8 rounded-md p-0",
			},
			variant: {
				default: [
					"bg-gradient-accent text-white shadow-elevation-1",
					"hover:bg-gradient-accent-hover hover:shadow-glow-accent",
				],
				destructive: [
					"bg-error text-white shadow-elevation-1",
					"hover:bg-error/90 hover:shadow-elevation-2",
				],
				outline: [
					"border border-app-line bg-transparent",
					"hover:bg-app-hover/40 hover:text-ink hover:border-app-hover",
				],
				"accent-outline": [
					"border border-accent/30 bg-accent/10 text-accent",
					"hover:bg-accent/20 hover:border-accent/50 hover:shadow-glow-accent",
				],
				secondary: [
					"bg-app-darkBox text-ink-dull shadow-elevation-1",
					"hover:bg-app-lightBox hover:text-ink hover:shadow-elevation-2",
				],
				ghost: ["hover:bg-app-darkBox hover:text-ink-dull", "text-ink-faint"],
				link: ["text-accent underline-offset-4", "hover:underline"],
			},
		},
		defaultVariants: {
			variant: "default",
			size: "default",
		},
	},
);

export type ButtonBaseProps = VariantProps<typeof buttonStyles>;

export interface ButtonProps
	extends React.ButtonHTMLAttributes<HTMLButtonElement>, ButtonBaseProps {
	loading?: boolean;
	leftIcon?: React.ReactNode;
	rightIcon?: React.ReactNode;
	children?: React.ReactNode;
}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
	(
		{
			className,
			variant,
			size,
			loading,
			leftIcon,
			rightIcon,
			children,
			disabled,
			...props
		},
		ref,
	) => {
		const isDisabled = disabled || loading;

		return (
			<button
				className={cx(buttonStyles({variant, size}), className)}
				ref={ref}
				disabled={isDisabled}
				{...props}
			>
				{loading && <Loader className="mr-2 h-4 w-4 animate-spin" />}
				{!loading && leftIcon && (
					<span className="mr-2 inline-flex items-center">{leftIcon}</span>
				)}
				{children}
				{!loading && rightIcon && (
					<span className="ml-2 inline-flex items-center">{rightIcon}</span>
				)}
			</button>
		);
	},
);

Button.displayName = "Button";
