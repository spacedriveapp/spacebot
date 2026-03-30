export type {ComponentPropsWithoutRef} from "react";
export {
	SelectRoot as Select,
	SelectGroup,
	SelectValue,
	SelectTrigger,
	SelectContent,
	SelectLabel,
	SelectItem,
	SelectSeparator,
	SelectScrollUpButton,
	SelectScrollDownButton,
} from "@spaceui/primitives";

export type SelectProps = React.ComponentPropsWithoutRef<
	typeof import("@spaceui/primitives").SelectRoot
>;
