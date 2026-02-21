import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { motion, type HTMLMotionProps } from "framer-motion";
import { cx } from "./utils";
import { slideUp, spring } from "@/lib/motion";

export const cardStyles = cva(
  [
    "rounded-xl border transition-[border-color,box-shadow,background-color,transform] duration-150",
    "shadow-elevation-1",
  ],
  {
    variants: {
      variant: {
        default: [
          "border-app-line/80 bg-app-darkBox",
          "hover:border-app-hover hover:shadow-elevation-2",
        ],
        darker: [
          "border-app-line/80 bg-app-darkerBox",
          "hover:border-app-hover hover:shadow-elevation-2",
        ],
        ghost: [
          "border-transparent bg-transparent shadow-none",
        ],
        interactive: [
          "border-app-line/80 bg-app-darkBox cursor-pointer",
          "hover:border-accent/30 hover:shadow-elevation-3 hover:shadow-glow-accent hover:bg-app-box",
          "active:scale-[0.985]",
        ],
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
  }
);

export type CardBaseProps = VariantProps<typeof cardStyles>;

export interface CardProps
  extends React.HTMLAttributes<HTMLDivElement>,
    CardBaseProps {}

export const Card = React.forwardRef<HTMLDivElement, CardProps>(
  ({ className, variant, padding, ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cx(cardStyles({ variant, padding }), className)}
        {...props}
      />
    );
  }
);

Card.displayName = "Card";

export interface AnimatedCardProps
  extends Omit<HTMLMotionProps<"div">, "variant">,
    CardBaseProps {
  animate?: boolean;
}

export const AnimatedCard = React.forwardRef<HTMLDivElement, AnimatedCardProps>(
  ({ className, variant, padding, animate = true, ...props }, ref) => {
    return (
      <motion.div
        ref={ref}
        className={cx(cardStyles({ variant, padding }), className)}
        variants={animate ? slideUp : undefined}
        initial={animate ? "initial" : undefined}
        animate={animate ? "animate" : undefined}
        transition={spring}
        {...props}
      />
    );
  }
);

AnimatedCard.displayName = "AnimatedCard";

export const CardHeader = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    className={cx("flex flex-col space-y-1.5", className)}
    {...props}
  />
));
CardHeader.displayName = "CardHeader";

export const CardTitle = React.forwardRef<
  HTMLHeadingElement,
  React.HTMLAttributes<HTMLHeadingElement>
>(({ className, ...props }, ref) => (
  <h3
    ref={ref}
    className={cx("text-base font-semibold tracking-tight text-ink", className)}
    {...props}
  />
));
CardTitle.displayName = "CardTitle";

export const CardDescription = React.forwardRef<
  HTMLParagraphElement,
  React.HTMLAttributes<HTMLParagraphElement>
>(({ className, ...props }, ref) => (
  <p
    ref={ref}
    className={cx("text-sm text-ink-dull", className)}
    {...props}
  />
));
CardDescription.displayName = "CardDescription";

export const CardContent = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div ref={ref} className={cx("", className)} {...props} />
));
CardContent.displayName = "CardContent";

export const CardFooter = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    className={cx("flex items-center pt-4", className)}
    {...props}
  />
));
CardFooter.displayName = "CardFooter";
