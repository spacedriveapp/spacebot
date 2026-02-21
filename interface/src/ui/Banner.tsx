import * as React from "react";
import { cx } from "./utils";

export type BannerVariant = "info" | "warning" | "error" | "success" | "accent";

interface BannerProps extends React.HTMLAttributes<HTMLDivElement> {
  variant?: BannerVariant;
  dot?: "pulse" | "static" | "none";
  children: React.ReactNode;
}

const variantStyles: Record<BannerVariant, string> = {
  info: "bg-blue-500/10 text-blue-400 border-blue-500/20",
  warning: "bg-warning/10 text-warning border-warning/20",
  error: "bg-error/10 text-error border-error/20",
  success: "bg-success/10 text-success border-success/20",
  accent: "bg-accent/10 text-accent border-accent/20",
};

export function Banner({
  variant = "info",
  dot = "static",
  className,
  children,
  ...props
}: BannerProps) {
  return (
    <div
      className={cx(
        "border-b px-4 py-2 text-sm",
        variantStyles[variant],
        className
      )}
      {...props}
    >
      <div className="flex items-center gap-2">
        {dot !== "none" && (
          <div
            className={cx(
              "h-1.5 w-1.5 rounded-full bg-current",
              dot === "pulse" && "animate-pulse"
            )}
          />
        )}
        {children}
      </div>
    </div>
  );
}

export function BannerActions({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cx("ml-auto flex items-center gap-2", className)}>
      {children}
    </div>
  );
}
