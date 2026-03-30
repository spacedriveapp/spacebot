import * as React from "react";
import {Switch, type SwitchProps} from "@spaceui/primitives";

export interface ToggleProps extends SwitchProps {}

export const Toggle = React.forwardRef<HTMLButtonElement, ToggleProps>(
	(props, ref) => <Switch ref={ref} {...props} />,
);

Toggle.displayName = "Toggle";
