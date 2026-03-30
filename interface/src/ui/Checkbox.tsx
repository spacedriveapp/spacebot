"use client";

import * as React from "react";
import {Check, Minus} from "@phosphor-icons/react";
import {CheckboxIndicator, CheckboxRoot} from "@spaceui/primitives";
import {cva, type VariantProps} from "class-variance-authority";
import {cx} from "./utils";

const CheckboxRootCompat = CheckboxRoot as unknown as React.ComponentType<any>;
const CheckboxIndicatorCompat = CheckboxIndicator as unknown as React.ComponentType<any>;

const checkboxStyles = cva([
	"peer h-4 w-4 shrink-0 rounded border border-app-line",
	"ring-offset-app-box focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2",
	"disabled:cursor-not-allowed disabled:opacity-50",
	"data-[state=checked]:bg-accent data-[state=checked]:border-accent",
	"data-[state=indeterminate]:bg-accent data-[state=indeterminate]:border-accent",
]);

const indicatorStyles = cva("flex items-center justify-center text-current");

export interface CheckboxProps
	extends React.ComponentPropsWithoutRef<typeof CheckboxRootCompat>,
		VariantProps<typeof checkboxStyles> {
	indeterminate?: boolean;
}

export const Checkbox = React.forwardRef<HTMLButtonElement, CheckboxProps>(
	({className, indeterminate, checked, ...props}, ref) => (
		<CheckboxRootCompat
			ref={ref}
			className={cx(checkboxStyles(), className)}
			checked={indeterminate ? "indeterminate" : checked}
			{...props}
		>
			<CheckboxIndicatorCompat className={cx(indicatorStyles())}>
				{indeterminate ? (
					<Minus className="h-3 w-3 text-white" />
				) : (
					<Check className="h-3 w-3 text-white" weight="bold" />
				)}
			</CheckboxIndicatorCompat>
		</CheckboxRootCompat>
	),
);

Checkbox.displayName = "Checkbox";

export interface CheckboxLabelProps {
	children: React.ReactNode;
	disabled?: boolean;
	className?: string;
}

export const CheckboxLabel: React.FC<CheckboxLabelProps> = ({
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

export interface CheckboxFieldProps {
	label: React.ReactNode;
	description?: string;
	disabled?: boolean;
	className?: string;
	checkboxProps?: Omit<CheckboxProps, "disabled">;
}

export const CheckboxField: React.FC<CheckboxFieldProps> = ({
	label,
	description,
	disabled,
	className,
	checkboxProps,
}) => (
	<label
		className={cx(
			"flex cursor-pointer items-start gap-3",
			disabled && "cursor-not-allowed",
			className,
		)}
	>
		<div className="mt-0.5">
			<Checkbox disabled={disabled} {...checkboxProps} />
		</div>
		<div className="space-y-1">
			<CheckboxLabel disabled={disabled}>{label}</CheckboxLabel>
			{description && (
				<p className={cx("text-xs text-ink-dull", disabled && "opacity-50")}>
					{description}
				</p>
			)}
		</div>
	</label>
);
