"use client";

import { useFormContext, FormProvider, useFormState } from "react-hook-form";
import { cx } from "../utils";

export { FormProvider, useFormContext, useFormState };

export const Form = FormProvider;

export interface ErrorMessageProps {
  error?: string;
  className?: string;
}

export const ErrorMessage = ({ error, className }: ErrorMessageProps) => {
  if (!error) return null;
  return (
    <p className={cx("text-xs text-error mt-1", className)}>{error}</p>
  );
};
