import * as React from "react";
import {ToggleGroup as SpaceUIToggleGroup} from "@spaceui/primitives";
import {cx} from "./utils";

const SpaceUIToggleGroupCompat = SpaceUIToggleGroup as unknown as React.ComponentType<any>;

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
		<SpaceUIToggleGroupCompat
			options={options}
			value={value}
			onChange={onChange}
			className={cx("bg-app-darkBox", className)}
			itemClassName="h-8 w-8 justify-center px-0"
		/>
	);
}
