import * as React from "react";
import {Badge as SpaceUIBadge} from "@spaceui/primitives";
import {cva, type VariantProps} from "class-variance-authority";
import {cx} from "./utils";

export const badgeStyles = cva("inline-flex items-center rounded-full border font-medium transition-colors", {
	variants: {
		variant: {
			default: "border-transparent bg-app-button text-ink-dull hover:bg-app-hover hover:text-ink",
			accent: "border-transparent bg-accent/15 text-accent hover:bg-accent/25",
			amber: "border-transparent bg-amber-500/15 text-amber-500 hover:bg-amber-500/25",
			violet: "border-transparent bg-violet-500/15 text-violet-500 hover:bg-violet-500/25",
			blue: "border-transparent bg-blue-500/15 text-blue-500 hover:bg-blue-500/25",
			red: "border-transparent bg-red-500/15 text-red-500 hover:bg-red-500/25",
			green: "border-transparent bg-emerald-500/15 text-emerald-500 hover:bg-emerald-500/25",
			outline: "border-app-line text-ink-dull hover:border-ink-faint hover:text-ink",
		},
		size: {
			sm: "h-5 px-2 text-tiny gap-1",
			md: "h-6 px-2.5 text-xs gap-1.5",
		},
	},
	defaultVariants: {
		variant: "default",
		size: "sm",
	},
});

export type BadgeBaseProps = VariantProps<typeof badgeStyles>;

export interface BadgeProps
	extends React.HTMLAttributes<HTMLDivElement>,
		BadgeBaseProps {
	children?: React.ReactNode;
}

export const Badge = React.forwardRef<HTMLDivElement, BadgeProps>(
	({className, variant, size, children, ...props}, ref) => {
		void ref;
		const spaceUiVariant =
			variant === "outline"
				? "outline"
				: variant === "red"
					? "destructive"
					: variant === "green"
						? "success"
						: variant === "amber"
							? "warning"
							: "secondary";

		return (
			<SpaceUIBadgeCompat
				variant={spaceUiVariant}
				size="sm"
				className={cx(badgeStyles({variant, size}), className)}
				{...props}
			>
				{children}
			</SpaceUIBadgeCompat>
		);
	},
);

Badge.displayName = "Badge";
const SpaceUIBadgeCompat = SpaceUIBadge as unknown as React.ComponentType<any>;
