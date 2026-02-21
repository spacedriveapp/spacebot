"use client";

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cx } from "./utils";
import { Tick02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

const progressBarStyles = cva(
  "relative h-2 w-full overflow-hidden rounded-full bg-app-button",
  {
    variants: {
      size: {
        sm: "h-1",
        md: "h-2",
        lg: "h-3",
        xl: "h-4",
      },
    },
    defaultVariants: {
      size: "md",
    },
  }
);

const progressIndicatorStyles = cva(
  "h-full w-full flex-1 bg-accent transition-all",
  {
    variants: {
      variant: {
        default: "bg-accent",
        success: "bg-success",
        warning: "bg-warning",
        error: "bg-error",
        neutral: "bg-ink-dull",
      },
      animated: {
        true: "animate-pulse",
      },
    },
    defaultVariants: {
      variant: "default",
      animated: false,
    },
  }
);

export interface ProgressBarProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof progressBarStyles>,
    VariantProps<typeof progressIndicatorStyles> {
  value: number;
  max?: number;
  showLabel?: boolean;
  labelPosition?: "inside" | "outside";
}

export const ProgressBar = React.forwardRef<HTMLDivElement, ProgressBarProps>(
  (
    {
      className,
      value,
      max = 100,
      size,
      variant,
      animated,
      showLabel = false,
      labelPosition = "outside",
      ...props
    },
    ref
  ) => {
    const percentage = Math.min(Math.max((value / max) * 100, 0), 100);

    return (
      <div className={cx("w-full", className)} {...props}>
        <div
          ref={ref}
          className={cx(progressBarStyles({ size }))}
        >
          <div
            className={cx(progressIndicatorStyles({ variant, animated }))}
            style={{ width: `${percentage}%` }}
          />
          {showLabel && labelPosition === "inside" && size !== "sm" && (
            <span className="absolute inset-0 flex items-center justify-center text-xs font-medium text-white">
              {Math.round(percentage)}%
            </span>
          )}
        </div>
        {showLabel && labelPosition === "outside" && (
          <div className="mt-1 flex items-center justify-between text-xs text-ink-dull">
            <span>{Math.round(percentage)}%</span>
            <span>
              {value} / {max}
            </span>
          </div>
        )}
      </div>
    );
  }
);

ProgressBar.displayName = "ProgressBar";

export interface CircularProgressProps
  extends React.SVGProps<SVGSVGElement>,
    VariantProps<typeof progressIndicatorStyles> {
  value: number;
  max?: number;
  size?: number;
  strokeWidth?: number;
  showLabel?: boolean;
}

export const CircularProgress: React.FC<CircularProgressProps> = ({
  value,
  max = 100,
  size = 40,
  strokeWidth = 4,
  variant,
  showLabel = false,
  className,
  ...props
}) => {
  const percentage = Math.min(Math.max((value / max) * 100, 0), 100);
  const radius = (size - strokeWidth) / 2;
  const circumference = radius * 2 * Math.PI;
  const offset = circumference - (percentage / 100) * circumference;

  const variantColor = {
    default: "stroke-accent",
    success: "stroke-green-500",
    warning: "stroke-amber-500",
    error: "stroke-red-500",
    neutral: "stroke-ink-dull",
  }[variant || "default"];

  return (
    <div className={cx("relative inline-flex items-center justify-center", className)}>
      <svg width={size} height={size} {...props}>
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="currentColor"
          strokeWidth={strokeWidth}
          className="text-app-button"
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="currentColor"
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          className={cx(variantColor, "transition-all duration-300")}
          transform={`rotate(-90 ${size / 2} ${size / 2})`}
        />
      </svg>
      {showLabel && (
        <span className="absolute text-xs font-medium text-ink">
          {Math.round(percentage)}%
        </span>
      )}
    </div>
  );
};

export interface ProgressStep {
  label: string;
  description?: string;
  status?: "pending" | "current" | "completed" | "error";
}

export interface ProgressStepsProps extends React.HTMLAttributes<HTMLDivElement> {
  steps: ProgressStep[];
  currentStep: number;
  orientation?: "horizontal" | "vertical";
}

export const ProgressSteps: React.FC<ProgressStepsProps> = ({
  steps,
  currentStep,
  orientation = "horizontal",
  className,
  ...props
}) => {
  return (
    <div
      className={cx(
        orientation === "horizontal"
          ? "flex items-start gap-2"
          : "flex flex-col gap-4",
        className
      )}
      {...props}
    >
      {steps.map((step, index) => {
        const status =
          step.status ||
          (index < currentStep
            ? "completed"
            : index === currentStep
            ? "current"
            : "pending");

        return (
          <div
            key={index}
            className={cx(
              "flex",
              orientation === "horizontal"
                ? "flex-col items-center flex-1 text-center"
                : "items-start gap-3"
            )}
          >
            <div
              className={cx(
                "flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-sm font-medium",
                status === "completed" && "bg-accent text-white",
                status === "current" && "border-2 border-accent bg-app-box text-accent",
                status === "pending" && "border-2 border-app-line bg-app-box text-ink-dull",
                status === "error" && "bg-error text-white"
              )}
            >
              {status === "completed" ? (
                <HugeiconsIcon icon={Tick02Icon} className="h-4 w-4" />
              ) : (
                index + 1
              )}
            </div>
            <div className={cx(orientation === "horizontal" && "mt-2")}>
              <p
                className={cx(
                  "text-sm font-medium",
                  status === "pending" ? "text-ink-dull" : "text-ink"
                )}
              >
                {step.label}
              </p>
              {step.description && (
                <p className="text-xs text-ink-dull mt-0.5">{step.description}</p>
              )}
            </div>
            {orientation === "horizontal" && index < steps.length - 1 && (
              <div className="absolute left-0 right-0 top-4 -z-10 hidden h-0.5 w-full bg-app-line lg:block" />
            )}
          </div>
        );
      })}
    </div>
  );
};
