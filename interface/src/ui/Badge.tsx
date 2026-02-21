import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cx } from "./utils";

export const badgeStyles = cva(
  [
    "inline-flex items-center rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset whitespace-nowrap gap-1.5",
  ],
  {
    variants: {
      variant: {
        default: "bg-app-lightBox text-ink-dull ring-app-line",
        secondary: "bg-app-darkBox text-ink-dull ring-app-line",
        outline: "text-ink-dull ring-app-line",
        accent: "bg-accent/15 text-accent ring-accent/25",
        violet: "bg-violet-500/15 text-violet-400 ring-violet-500/25",
        success: "bg-success/15 text-success ring-success/25",
        warning: "bg-warning/15 text-warning ring-warning/25",
        error: "bg-error/15 text-error ring-error/25",
        info: "bg-blue-500/15 text-blue-400 ring-blue-500/25",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
);

export type BadgeBaseProps = VariantProps<typeof badgeStyles>;

export interface BadgeProps
  extends React.HTMLAttributes<HTMLDivElement>,
    BadgeBaseProps {
  children?: React.ReactNode;
  dot?: boolean;
  pulse?: boolean;
}

const dotColorMap: Record<string, string> = {
  success: "bg-success",
  warning: "bg-warning",
  error: "bg-error",
  accent: "bg-accent",
  violet: "bg-violet-400",
  info: "bg-blue-400",
  default: "bg-ink-faint",
  secondary: "bg-ink-faint",
  outline: "bg-ink-faint",
};

export const Badge = React.forwardRef<HTMLDivElement, BadgeProps>(
  ({ className, variant, size, children, dot, pulse, ...props }, ref) => {
    return (
      <div
        ref={ref}
        className={cx(badgeStyles({ variant, size }), className)}
        {...props}
      >
        {dot && (
          <span
            className={cx(
              "h-1.5 w-1.5 shrink-0 rounded-full",
              dotColorMap[variant ?? "default"],
              pulse && "animate-pulse",
            )}
          />
        )}
        {children}
      </div>
    );
  }
);

Badge.displayName = "Badge";
