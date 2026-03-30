"use client";

import * as React from "react";
import {
	RadioGroupItem as SpaceUIRadioGroupItem,
	RadioGroupRoot as SpaceUIRadioGroupRoot,
} from "@spaceui/primitives";
import {cva, type VariantProps} from "class-variance-authority";
import {cx} from "./utils";

const RadioGroupRootCompat = SpaceUIRadioGroupRoot as unknown as React.ComponentType<any>;
const RadioGroupItemCompat = SpaceUIRadioGroupItem as unknown as React.ComponentType<any>;

const radioGroupStyles = cva("grid gap-3");
const radioGroupItemStyles = cva("");

export interface RadioGroupProps
	extends React.ComponentPropsWithoutRef<typeof RadioGroupRootCompat>,
		VariantProps<typeof radioGroupStyles> {}

export const RadioGroup = React.forwardRef<HTMLDivElement, RadioGroupProps>(
	({className, ...props}, ref) => (
		<RadioGroupRootCompat className={cx(radioGroupStyles(), className)} {...props} ref={ref} />
	),
);

RadioGroup.displayName = "RadioGroup";

export interface RadioGroupItemProps
	extends React.ComponentPropsWithoutRef<typeof RadioGroupItemCompat>,
		VariantProps<typeof radioGroupItemStyles> {}

export const RadioGroupItem = React.forwardRef<HTMLButtonElement, RadioGroupItemProps>(
	({className, ...props}, ref) => (
		<RadioGroupItemCompat ref={ref} className={className} {...props} />
	),
);

RadioGroupItem.displayName = "RadioGroupItem";

export interface RadioLabelProps {
	children: React.ReactNode;
	disabled?: boolean;
	className?: string;
}

export const RadioLabel: React.FC<RadioLabelProps> = ({
	children,
	disabled,
	className,
}) => (
	<span
		className={cx("text-sm font-medium text-ink", disabled && "opacity-50", className)}
	>
		{children}
	</span>
);

export interface RadioGroupFieldProps {
	value: string;
	label: React.ReactNode;
	description?: string;
	disabled?: boolean;
	className?: string;
}

export const RadioGroupField: React.FC<RadioGroupFieldProps> = ({
	value,
	label,
	description,
	disabled,
	className,
}) => (
	<label
		className={cx(
			"flex cursor-pointer items-start gap-3",
			disabled && "cursor-not-allowed",
			className,
		)}
	>
		<div className="mt-0.5">
			<RadioGroupItem value={value} disabled={disabled} />
		</div>
		<div className="space-y-1">
			<RadioLabel disabled={disabled}>{label}</RadioLabel>
			{description && (
				<p className={cx("text-xs text-ink-dull", disabled && "opacity-50")}>
					{description}
				</p>
			)}
		</div>
	</label>
);
