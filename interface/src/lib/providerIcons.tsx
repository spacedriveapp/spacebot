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
				<path d="M24 32H8V16H24V32Z" fill="#4B4646" />
				<path d="M24 8H8V32H24V8ZM32 40H0V0H32V40Z" fill="#F1ECEC" />
			</g>
			<defs>
				<clipPath id={clipPathId}>
					<rect width="32" height="40" fill="white" />
				</clipPath>
			</defs>
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
		ollama: OpenRouter,
		groq: Groq,
		mistral: Mistral,
		deepseek: DeepSeek,
		fireworks: Fireworks,
		together: Together,
		xai: XAI,
		zhipu: ZAI,
		"opencode-zen": OpenCodeZenIcon,
	};

	const IconComponent = iconMap[provider.toLowerCase()];

	if (!IconComponent) {
		return <OpenAI {...iconProps} />;
	}

	return <IconComponent {...iconProps} />;
}
