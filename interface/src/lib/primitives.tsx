import * as React from "react";
import {cx} from "class-variance-authority";
import {
	Banner as PrimitiveBanner,
	Button as PrimitiveButton,
	FilterButton as PrimitiveFilterButton,
	NumberStepper as PrimitiveNumberStepper,
} from "@spacedrive/primitives";

export * from "@spacedrive/primitives";

type LegacyButtonVariant = "ghost" | "secondary" | "destructive";
type PrimitiveButtonVariant = NonNullable<
	React.ComponentProps<typeof PrimitiveButton>["variant"]
>;
type PrimitiveButtonSize = React.ComponentProps<typeof PrimitiveButton>["size"];
type PrimitiveButtonRounding =
	React.ComponentProps<typeof PrimitiveButton>["rounding"];
type ButtonVariant = PrimitiveButtonVariant | LegacyButtonVariant;
type CompatButtonBaseProps = {
	children?: React.ReactNode;
	className?: string;
	loading?: boolean;
	rounding?: PrimitiveButtonRounding;
	size?: PrimitiveButtonSize;
	variant?: ButtonVariant;
};
type CompatActionButtonProps = CompatButtonBaseProps &
	Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "children"> & {
		href?: undefined;
	};
type CompatLinkButtonProps = CompatButtonBaseProps &
	Omit<React.AnchorHTMLAttributes<HTMLAnchorElement>, "children"> & {
		href: string;
	};
type CompatButtonProps = CompatActionButtonProps | CompatLinkButtonProps;

function hasHref(
	props: CompatButtonProps,
): props is CompatLinkButtonProps {
	return "href" in props && props.href !== undefined;
}

function mapButtonVariant(
	variant: ButtonVariant | undefined,
): PrimitiveButtonVariant | undefined {
	switch (variant) {
		case "ghost":
			return "subtle";
		case "secondary":
			return "gray";
		case "destructive":
			return "outline";
		default:
			return variant;
	}
}

function legacyButtonClassName(variant: ButtonVariant | undefined) {
	if (variant === "destructive") {
		return "border-red-500/40 text-red-300 hover:border-red-500/60 hover:bg-red-500/10";
	}

	return undefined;
}

function LoadingSpinner() {
	return (
		<span
			aria-hidden="true"
			className="inline-block size-3 shrink-0 animate-spin rounded-full border border-current border-t-transparent"
		/>
	);
}

export const Button = React.forwardRef<
	React.ElementRef<typeof PrimitiveButton>,
	CompatButtonProps
>(({variant, loading = false, className, children, ...props}, ref) => {
	const buttonClassName = cx(legacyButtonClassName(variant), className);
	const buttonVariant = mapButtonVariant(variant);
	const content = (
		<>
			{loading ? <LoadingSpinner /> : null}
			{children}
		</>
	);

	if (hasHref(props)) {
		const linkProps = props;
		const linkDisabled =
			loading ||
			linkProps["aria-disabled"] === true ||
			linkProps["aria-disabled"] === "true";
		const handleLinkClick: React.MouseEventHandler<HTMLAnchorElement> = (event) => {
			if (linkDisabled) {
				event.preventDefault();
				event.stopPropagation();
				return;
			}
			linkProps.onClick?.(event);
		};
		const handleLinkKeyDown: React.KeyboardEventHandler<HTMLAnchorElement> = (event) => {
			if (linkDisabled && (event.key === "Enter" || event.key === " ")) {
				event.preventDefault();
				event.stopPropagation();
				return;
			}
			linkProps.onKeyDown?.(event);
		};

			return (
				<PrimitiveButton
					{...linkProps}
					ref={ref}
					loading={loading}
					aria-busy={loading || undefined}
					aria-disabled={linkDisabled || undefined}
					tabIndex={linkDisabled ? -1 : linkProps.tabIndex}
					onClick={handleLinkClick}
				onKeyDown={handleLinkKeyDown}
				variant={buttonVariant}
				className={buttonClassName}
			>
				{content}
			</PrimitiveButton>
		);
	}

	const actionProps = props as CompatActionButtonProps;

	return (
		<PrimitiveButton
			{...actionProps}
			ref={ref}
			disabled={loading || actionProps.disabled}
			aria-busy={loading || undefined}
			variant={buttonVariant}
			className={buttonClassName}
		>
			{content}
		</PrimitiveButton>
	);
});

Button.displayName = "Button";

type BannerDotMode = "pulse" | "static";
type CompatBannerProps = React.ComponentProps<typeof PrimitiveBanner> & {
	dot?: BannerDotMode;
};

export const Banner = React.forwardRef<
	React.ElementRef<typeof PrimitiveBanner>,
	CompatBannerProps
>(({dot, showDot, className, ...props}, ref) => (
	<PrimitiveBanner
		{...props}
		ref={ref}
		showDot={showDot ?? dot !== undefined}
		className={cx(
			dot === "pulse" && "[&>span:first-child]:animate-pulse",
			className,
		)}
	/>
));

Banner.displayName = "Banner";

type CompatFilterButtonProps = React.ComponentProps<
	typeof PrimitiveFilterButton
> & {
	colorClass?: string;
};

export const FilterButton = React.forwardRef<
	React.ElementRef<typeof PrimitiveFilterButton>,
	CompatFilterButtonProps
>(({colorClass, active, className, ...props}, ref) => (
	<PrimitiveFilterButton
		{...props}
		ref={ref}
		active={active}
		className={cx(active && colorClass, className)}
	/>
));

FilterButton.displayName = "FilterButton";

type CompatNumberStepperProps = React.ComponentProps<
	typeof PrimitiveNumberStepper
> & {
	type?: "float" | "int";
	variant?: string;
};

export const NumberStepper = React.forwardRef<
	React.ElementRef<typeof PrimitiveNumberStepper>,
	CompatNumberStepperProps
>(({type, variant: _variant, allowFloat, ...props}, ref) => {
	// Only pass allowFloat when explicitly provided or when type === "float"
	// so PrimitiveNumberStepper can use its own default when neither is set.
	const resolvedAllowFloat = allowFloat !== undefined
		? allowFloat
		: (type === "float" ? true : undefined);
	return (
		<PrimitiveNumberStepper
			{...props}
			ref={ref}
			allowFloat={resolvedAllowFloat}
		/>
	);
});

NumberStepper.displayName = "NumberStepper";
