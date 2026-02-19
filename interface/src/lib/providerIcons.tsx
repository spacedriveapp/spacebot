import { useId } from "react";
import Anthropic from "@lobehub/icons/es/Anthropic";
import OpenAI from "@lobehub/icons/es/OpenAI";
import OpenRouter from "@lobehub/icons/es/OpenRouter";
import Groq from "@lobehub/icons/es/Groq";
import Mistral from "@lobehub/icons/es/Mistral";
import DeepSeek from "@lobehub/icons/es/DeepSeek";
import Fireworks from "@lobehub/icons/es/Fireworks";
import Together from "@lobehub/icons/es/Together";
import XAI from "@lobehub/icons/es/XAI";
import ZAI from "@lobehub/icons/es/ZAI";

interface IconProps {
	size?: number;
	className?: string;
}

interface ProviderIconProps {
	provider: string;
	className?: string;
	size?: number;
}

function NvidiaIcon({ size = 24, className }: IconProps) {
	return (
		<svg
			width={size}
			height={size}
			viewBox="0 0 64 64"
			fill="currentColor"
			xmlns="http://www.w3.org/2000/svg"
			className={className}
			aria-hidden="true"
			focusable="false"
		>
			<path d="M23.862 23.46v-3.816l1.13-.047c10.46-.33 17.313 8.998 17.313 8.998s-7.396 10.27-15.335 10.27a9.73 9.73 0 0 1-3.086-.495v-11.59c4.075.495 4.9 2.285 7.326 6.36l5.44-4.57s-3.98-5.206-10.67-5.206c-.707-.024-1.413.024-2.12.094m0-12.626v5.7l1.13-.07c14.534-.495 24.026 11.92 24.026 11.92S38.136 41.622 26.806 41.622c-.99 0-1.955-.094-2.92-.26v3.533c.8.094 1.625.165 2.426.165 10.553 0 18.185-5.394 25.58-11.754 1.225.99 6.242 3.368 7.28 4.405-7.02 5.89-23.39 10.623-32.67 10.623a23.24 23.24 0 0 1-2.591-.141v4.97H64v-42.33zm0 27.536v3.015C14.1 39.644 11.4 29.49 11.4 29.49s4.688-5.182 12.46-6.03v3.298h-.024c-4.075-.495-7.28 3.32-7.28 3.32s1.814 6.43 7.302 8.29M6.548 29.067s5.77-8.527 17.337-9.422v-3.11C11.07 17.572 0 28.408 0 28.408s6.266 18.138 23.862 19.787v-3.298c-12.908-1.602-17.313-15.83-17.313-15.83z" />
		</svg>
	);
}

function OpenCodeZenIcon({ size = 24, className }: IconProps) {
	const clipId = useId();
	const clipPathId = `opencode-zen-clip-${clipId}`;
	const width = (size * 32) / 40;

	return (
		<svg
			width={width}
			height={size}
			viewBox="0 0 32 40"
			fill="none"
			xmlns="http://www.w3.org/2000/svg"
			className={className}
			aria-hidden="true"
			focusable="false"
		>
			<g clipPath={`url(#${clipPathId})`}>
				<path d="M24 32H8V16H24V32Z" fill="currentColor" opacity="0.4" />
				<path d="M24 8H8V32H24V8ZM32 40H0V0H32V40Z" fill="currentColor" />
			</g>
			<defs>
				<clipPath id={clipPathId}>
					<rect width="32" height="40" fill="white" />
				</clipPath>
			</defs>
		</svg>
	);
}

function OllamaIcon({ size = 24, className }: IconProps) {
	return (
		<svg
			width={size}
			height={size}
			viewBox="0 0 24 24"
			fill="none"
			xmlns="http://www.w3.org/2000/svg"
			className={className}
			aria-hidden="true"
			focusable="false"
		>
			<rect x="3" y="7" width="18" height="14" rx="3" stroke="currentColor" strokeWidth="1.5" />
			<circle cx="9" cy="13" r="1.5" fill="currentColor" />
			<circle cx="15" cy="13" r="1.5" fill="currentColor" />
			<path d="M12 3V7" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
		</svg>
	);
}

export function ProviderIcon({ provider, className = "text-ink-faint", size = 24 }: ProviderIconProps) {
	const iconProps: Partial<IconProps> = {
		size,
		className,
	};

	const iconMap: Record<string, React.ComponentType<IconProps>> = {
		anthropic: Anthropic,
		openai: OpenAI,
		openrouter: OpenRouter,
		groq: Groq,
		mistral: Mistral,
		deepseek: DeepSeek,
		fireworks: Fireworks,
		together: Together,
		xai: XAI,
		zhipu: ZAI,
		ollama: OllamaIcon,
		"opencode-zen": OpenCodeZenIcon,
		nvidia: NvidiaIcon,
	};

	const IconComponent = iconMap[provider.toLowerCase()];

	if (!IconComponent) {
		return <OpenAI {...iconProps} />;
	}

	return <IconComponent {...iconProps} />;
}
