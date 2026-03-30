import * as React from "react";
import {
	Card as SpaceUICard,
	CardContent as SpaceUICardContent,
	CardDescription as SpaceUICardDescription,
	CardFooter as SpaceUICardFooter,
	CardHeader as SpaceUICardHeader,
	CardTitle as SpaceUICardTitle,
} from "@spaceui/primitives";
import {cva, type VariantProps} from "class-variance-authority";
import {cx} from "./utils";

const SpaceUICardCompat = SpaceUICard as unknown as React.ComponentType<any>;
const SpaceUICardHeaderCompat = SpaceUICardHeader as unknown as React.ComponentType<any>;
const SpaceUICardTitleCompat = SpaceUICardTitle as unknown as React.ComponentType<any>;
const SpaceUICardDescriptionCompat = SpaceUICardDescription as unknown as React.ComponentType<any>;
const SpaceUICardContentCompat = SpaceUICardContent as unknown as React.ComponentType<any>;
const SpaceUICardFooterCompat = SpaceUICardFooter as unknown as React.ComponentType<any>;

export const cardStyles = cva("", {
	variants: {
		variant: {
			default: "bg-app-darkBox",
			darker: "bg-app-darkerBox",
			ghost: "border-transparent bg-transparent shadow-none",
		},
		padding: {
			none: "",
			sm: "p-3",
			md: "p-5",
			lg: "p-6",
		},
	},
	defaultVariants: {
		variant: "default",
		padding: "md",
	},
});

export type CardBaseProps = VariantProps<typeof cardStyles>;

export interface CardProps
	extends React.HTMLAttributes<HTMLDivElement>,
		CardBaseProps {}

export const Card = React.forwardRef<HTMLDivElement, CardProps>(
	({className, variant, padding, ...props}, ref) => {
		void ref;
		const paddingClass = padding === "none" ? "p-0" : undefined;

		return (
			<SpaceUICardCompat
				className={cx(cardStyles({variant}), paddingClass, className)}
				{...props}
			/>
		);
	},
);

Card.displayName = "Card";

export const CardHeader = React.forwardRef<
	HTMLDivElement,
	React.HTMLAttributes<HTMLDivElement>
>(({className, ...props}, ref) => {
	void ref;
	return <SpaceUICardHeaderCompat className={cx("p-0", className)} {...props} />;
});

CardHeader.displayName = "CardHeader";

export const CardTitle = React.forwardRef<
	HTMLHeadingElement,
	React.HTMLAttributes<HTMLHeadingElement>
>(({className, ...props}, ref) => {
	void ref;
	return (
		<SpaceUICardTitleCompat
			className={cx("font-plex text-base text-ink", className)}
			{...props}
		/>
	);
});

CardTitle.displayName = "CardTitle";

export const CardDescription = React.forwardRef<
	HTMLParagraphElement,
	React.HTMLAttributes<HTMLParagraphElement>
>(({className, ...props}, ref) => {
	void ref;
	return <SpaceUICardDescriptionCompat className={className} {...props} />;
});

CardDescription.displayName = "CardDescription";

export const CardContent = React.forwardRef<
	HTMLDivElement,
	React.HTMLAttributes<HTMLDivElement>
>(({className, ...props}, ref) => {
	void ref;
	return <SpaceUICardContentCompat className={cx("p-0", className)} {...props} />;
});

CardContent.displayName = "CardContent";

export const CardFooter = React.forwardRef<
	HTMLDivElement,
	React.HTMLAttributes<HTMLDivElement>
>(({className, ...props}, ref) => {
	void ref;
	return <SpaceUICardFooterCompat className={cx("p-0 pt-4", className)} {...props} />;
});

CardFooter.displayName = "CardFooter";
