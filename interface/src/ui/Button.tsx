import * as React from "react";
import {cva, type VariantProps} from "class-variance-authority";
import {Button as SpaceUIButton, Loader} from "@spaceui/primitives";
import {cx} from "./utils";

export const buttonStyles = cva("", {
	variants: {
		size: {
			default: "",
			sm: "",
			lg: "",
			icon: "",
		},
		variant: {
			default: "",
			destructive: "",
			outline: "",
			secondary: "",
			ghost: "",
			link: "",
		},
	},
	defaultVariants: {
		variant: "default",
		size: "default",
	},
});

export type ButtonBaseProps = VariantProps<typeof buttonStyles>;

export interface ButtonProps
	extends React.ButtonHTMLAttributes<HTMLButtonElement>, ButtonBaseProps {
	loading?: boolean;
	leftIcon?: React.ReactNode;
	rightIcon?: React.ReactNode;
	children?: React.ReactNode;
}

const sizeMap: Record<NonNullable<ButtonProps["size"]>, "icon" | "sm" | "md" | "lg"> = {
	default: "md",
	sm: "sm",
	lg: "lg",
	icon: "icon",
};

const variantMap: Record<NonNullable<ButtonProps["variant"]>, "accent" | "gray" | "default" | "bare"> = {
	default: "accent",
	destructive: "gray",
	outline: "default",
	secondary: "gray",
	ghost: "bare",
	link: "bare",
};

const variantClassMap: Record<NonNullable<ButtonProps["variant"]>, string> = {
	default: "",
	destructive:
		"text-ink-dull hover:border-red-600 hover:bg-red-600 hover:text-white",
	outline: "",
	secondary: "",
	ghost: "text-ink-faint hover:bg-app-darkBox hover:text-ink-dull",
	link: "h-auto border-none p-0 text-accent underline-offset-4 hover:underline",
};

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
	(
		{
			className,
			variant: variantProp = "default",
			size: sizeProp = "default",
			loading,
			leftIcon,
			rightIcon,
			children,
			disabled,
			type = "button",
			...props
		},
		ref,
	) => {
		const variant = variantProp ?? "default";
		const size = sizeProp ?? "default";
		const isDisabled = disabled || loading;

		return (
			<SpaceUIButton
				ref={ref}
				type={type}
				disabled={isDisabled}
				variant={variantMap[variant]}
				size={sizeMap[size]}
				className={cx(
					"inline-flex items-center justify-center",
					variantClassMap[variant],
					className,
				)}
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
			</SpaceUIButton>
		);
	},
);

Button.displayName = "Button";
