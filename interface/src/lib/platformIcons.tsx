import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faDiscord, faSlack, faTelegram, faTwitch, faWhatsapp } from "@fortawesome/free-brands-svg-icons";
import { faLink, faEnvelope, faComments, faComment, faServer } from "@fortawesome/free-solid-svg-icons";

interface PlatformIconProps {
	platform: string;
	className?: string;
	size?: "sm" | "lg" | "1x" | "2x";
}

export function PlatformIcon({ platform, className = "text-ink-faint", size = "1x" }: PlatformIconProps) {
	const iconMap: Record<string, any> = {
		discord: faDiscord,
		slack: faSlack,
		telegram: faTelegram,
		twitch: faTwitch,
		webhook: faLink,
		email: faEnvelope,
		mattermost: faServer,
		whatsapp: faWhatsapp,
		signal: faComment,
		matrix: faComments,
		imessage: faComment,
		irc: faComments,
		lark: faComment,
		dingtalk: faComment,
	};

	const icon = iconMap[platform.toLowerCase()] || faLink;

	return <FontAwesomeIcon icon={icon} size={size} className={className} />;
}
