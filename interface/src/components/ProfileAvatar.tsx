/** Deterministic gradient avatar with initials, custom gradient, or uploaded image. */

import { useEffect, useState } from "react";

export function seedGradient(seed: string): [string, string] {
	let hash = 0;
	for (let i = 0; i < seed.length; i++) {
		hash = seed.charCodeAt(i) + ((hash << 5) - hash);
		hash |= 0;
	}
	const hue1 = (hash >>> 0) % 360;
	const hue2 = (hue1 + 40 + ((hash >>> 8) % 60)) % 360;
	return [`hsl(${hue1}, 70%, 55%)`, `hsl(${hue2}, 60%, 45%)`];
}

interface ProfileAvatarProps {
	/** Seed for the gradient (typically the node/agent ID). */
	seed: string;
	/** Display name to derive initials from. Falls back to seed. */
	name: string;
	/** Pixel size of the avatar. Default 48. */
	size?: number;
	/** Extra CSS classes on the outer element. */
	className?: string;
	/** Custom gradient start color (overrides seed gradient). */
	gradientStart?: string;
	/** Custom gradient end color (overrides seed gradient). */
	gradientEnd?: string;
	/** URL to a custom avatar image (takes precedence over gradient). */
	avatarUrl?: string;
}

export function ProfileAvatar({
	seed,
	name,
	size = 48,
	className,
	gradientStart,
	gradientEnd,
	avatarUrl,
}: ProfileAvatarProps) {
	const [imgFailed, setImgFailed] = useState(false);

	// Reset failure state when the URL changes (e.g. after uploading a new avatar).
	useEffect(() => setImgFailed(false), [avatarUrl]);

	// If an uploaded avatar exists and hasn't failed to load, render an img.
	if (avatarUrl && !imgFailed) {
		return (
			<img
				src={avatarUrl}
				alt={name}
				width={size}
				height={size}
				className={className}
				style={{ objectFit: "cover" }}
				draggable={false}
				onError={() => setImgFailed(true)}
			/>
		);
	}

	const [defaultC1, defaultC2] = seedGradient(seed);
	const c1 = gradientStart || defaultC1;
	const c2 = gradientEnd || defaultC2;
	const gradientId = `av-${seed}`;
	const initials = name.slice(0, 2).toUpperCase();
	const fontSize = 22;

	return (
		<svg
			width={size}
			height={size}
			viewBox="0 0 64 64"
			className={className}
		>
			<defs>
				<linearGradient id={gradientId} x1="0%" y1="0%" x2="100%" y2="100%">
					<stop offset="0%" stopColor={c1} />
					<stop offset="100%" stopColor={c2} />
				</linearGradient>
			</defs>
			<circle cx="32" cy="32" r="32" fill={`url(#${gradientId})`} />
			<text
				x="32"
				y="32"
				textAnchor="middle"
				dominantBaseline="central"
				fill="white"
				fontSize={fontSize}
				fontWeight="600"
				fontFamily="IBM Plex Sans, sans-serif"
				opacity="0.9"
			>
				{initials}
			</text>
		</svg>
	);
}
