import * as React from "react";
import { cx } from "./utils";

export interface ToggleGroupOption<T extends string> {
	value: T;
	label: React.ReactNode;
	title?: string;
}

export interface ToggleGroupProps<T extends string> {
	options: ToggleGroupOption<T>[];
	value: T;
	onChange: (value: T) => void;
	className?: string;
}

export function ToggleGroup<T extends string>({
	options,
	value,
	onChange,
	className,
}: ToggleGroupProps<T>) {
	return (
		<div
			className={cx(
				"flex overflow-hidden rounded-md border border-app-line bg-app-darkBox",
				className
			)}
		>
			{options.map((option) => {
				const isActive = option.value === value;

				return (
					<button
						key={option.value}
						onClick={() => onChange(option.value)}
						title={option.title}
						className={cx(
							"inline-flex h-8 w-8 items-center justify-center transition-colors",
							isActive
								? "bg-app-selected text-ink"
								: "text-ink-faint hover:bg-app-hover/40 hover:text-ink-dull"
						)}
					>
						{option.label}
					</button>
				);
			})}
		</div>
	);
}
