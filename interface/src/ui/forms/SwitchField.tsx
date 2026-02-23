"use client";

import * as React from "react";
import { useFormContext, Controller } from "react-hook-form";
import { Toggle, type ToggleProps } from "../Toggle";
import { ErrorMessage } from "./Form";
import { cx } from "../utils";

export interface SwitchFieldProps extends Omit<ToggleProps, "name" | "checked" | "onCheckedChange"> {
  name: string;
  label?: string;
  description?: string;
  className?: string;
}

export const SwitchField: React.FC<SwitchFieldProps> = ({
  name,
  label,
  description,
  className,
  ...props
}) => {
  const { control, formState } = useFormContext();
  const error = formState.errors[name]?.message as string | undefined;

  return (
    <Controller
      name={name}
      control={control}
      render={({ field }) => (
        <div className={cx("space-y-1.5", className)}>
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              {label && (
                <span className="text-sm font-medium text-ink">{label}</span>
              )}
              {description && (
                <p className="text-xs text-ink-dull">{description}</p>
              )}
            </div>
            <Toggle
              {...props}
              checked={field.value ?? false}
              onCheckedChange={field.onChange}
            />
          </label>
          {error && <ErrorMessage error={error} />}
        </div>
      )}
    />
  );
};

SwitchField.displayName = "SwitchField";
