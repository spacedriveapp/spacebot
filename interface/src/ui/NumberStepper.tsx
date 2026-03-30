import * as React from "react";
import {NumberStepper as SpaceUINumberStepper} from "@spaceui/primitives";
import {cx} from "./utils";

const SpaceUINumberStepperCompat = SpaceUINumberStepper as unknown as React.ComponentType<any>;

export interface NumberStepperProps {
	label?: string;
	description?: string;
	value: number;
	onChange: (value: number) => void;
	min?: number;
	max?: number;
	step?: number;
	suffix?: string;
	type?: "integer" | "float";
	showProgress?: boolean;
	variant?: "default" | "compact";
}

export const NumberStepper = React.forwardRef<HTMLDivElement, NumberStepperProps>(
	(
		{
			label,
			description,
			value,
			onChange,
			min,
			max,
			step = 1,
			suffix,
			type = "integer",
			showProgress = false,
			variant = "default",
		},
		ref,
	) => (
		<div ref={ref} className="flex flex-col gap-1.5">
			{label && <label className="text-sm font-medium text-ink">{label}</label>}
			{description && <p className="text-tiny text-ink-faint">{description}</p>}
			<div className={cx("flex items-center gap-2.5", (label || description) && "mt-1")}>
				<SpaceUINumberStepperCompat
					value={value ?? 0}
					onChange={onChange}
					min={min}
					max={max}
					step={step}
					allowFloat={type === "float"}
					showProgress={showProgress}
					className={cx(variant === "compact" && "[&>div>span]:min-w-[2.5rem] [&>div>button]:h-7 [&>div>button]:w-7")}
				/>
				{suffix && <span className="text-tiny text-ink-faint">{suffix}</span>}
			</div>
		</div>
	),
);

NumberStepper.displayName = "NumberStepper";
