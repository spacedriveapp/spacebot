import * as React from "react";
import {
	Input as SpaceUIInput,
	PasswordInput as SpaceUIPasswordInput,
	SearchInput as SpaceUISearchInput,
	TextArea as SpaceUITextArea,
} from "@spaceui/primitives";
import {cx} from "./utils";

const SpaceUIInputCompat = SpaceUIInput as unknown as React.ComponentType<any>;
const SpaceUITextAreaCompat = SpaceUITextArea as unknown as React.ComponentType<any>;
const SpaceUISearchInputCompat = SpaceUISearchInput as unknown as React.ComponentType<any>;
const SpaceUIPasswordInputCompat = SpaceUIPasswordInput as unknown as React.ComponentType<any>;

export const inputSizes = {
	sm: "sm",
	md: "md",
	lg: "lg",
} as const;

type LocalSize = keyof typeof inputSizes;

export interface InputProps
	extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> {
	size?: LocalSize;
	error?: boolean;
	icon?: React.ReactNode;
	right?: React.ReactNode;
	variant?: "default" | "transparent";
	inputElementClassName?: string;
}

export const Input = React.forwardRef<HTMLInputElement, InputProps>(
	({className, size = "sm", variant = "default", ...props}, ref) => (
		<SpaceUIInputCompat
			ref={ref}
			size={inputSizes[size]}
			variant={variant}
			className={className}
			{...props}
		/>
	),
);

Input.displayName = "Input";

export interface TextAreaProps
	extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
	variant?: "default" | "transparent";
	error?: boolean;
}

export const TextArea = React.forwardRef<HTMLTextAreaElement, TextAreaProps>(
	({className, variant = "default", error, ...props}, ref) => (
		<SpaceUITextAreaCompat
			ref={ref}
			variant={variant}
			error={error}
			className={className}
			{...props}
		/>
	),
);

TextArea.displayName = "TextArea";

export const SearchInput = React.forwardRef<
	HTMLInputElement,
	Omit<InputProps, "icon">
>(({size = "sm", ...props}, ref) => (
	<SpaceUISearchInputCompat ref={ref} size={inputSizes[size]} {...props} />
));

SearchInput.displayName = "SearchInput";

export const Label = React.forwardRef<
	HTMLLabelElement,
	React.LabelHTMLAttributes<HTMLLabelElement>
>(({className, ...props}, ref) => (
	<label
		ref={ref}
		className={cx("mb-1.5 block text-xs font-medium text-ink-dull", className)}
		{...props}
	/>
));

Label.displayName = "Label";

export const PasswordInput = React.forwardRef<
	HTMLInputElement,
	Omit<InputProps, "type">
>(({size = "sm", ...props}, ref) => (
	<SpaceUIPasswordInputCompat ref={ref} size={inputSizes[size]} {...props} />
));

PasswordInput.displayName = "PasswordInput";
