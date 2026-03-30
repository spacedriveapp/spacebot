import * as React from "react";
import {Banner as SpaceUIBanner} from "@spaceui/primitives";
import {cx} from "./utils";

export type BannerVariant = "info" | "warning" | "error" | "success" | "cyan";

interface BannerProps extends React.HTMLAttributes<HTMLDivElement> {
	variant?: BannerVariant;
	dot?: "pulse" | "static" | "none";
	children: React.ReactNode;
}

export function Banner({
	variant = "info",
	dot = "static",
	className,
	children,
	...props
}: BannerProps) {
	const mappedVariant = variant === "cyan" ? "info" : variant;

	return (
		<SpaceUIBanner
			variant={mappedVariant}
			showDot={dot !== "none"}
			className={cx(dot === "pulse" && "[&>span:first-child]:animate-pulse", className)}
			{...props}
		>
			{children}
		</SpaceUIBanner>
	);
}

export function BannerActions({
	children,
	className,
}: {
	children: React.ReactNode;
	className?: string;
}) {
	return <div className={cx("ml-auto flex items-center gap-2", className)}>{children}</div>;
}
