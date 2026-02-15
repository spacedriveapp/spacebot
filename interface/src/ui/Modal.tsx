import { useEffect, useRef } from "react";
import { AnimatePresence, motion } from "framer-motion";

export function Modal({
	isOpen,
	onClose,
	title,
	children,
}: {
	isOpen: boolean;
	onClose: () => void;
	title: string;
	children: React.ReactNode;
}) {
	const ref = useRef<HTMLDivElement>(null);

	useEffect(() => {
		if (!isOpen) return;
		function handleKey(e: KeyboardEvent) {
			if (e.key === "Escape") onClose();
		}
		document.addEventListener("keydown", handleKey);
		return () => document.removeEventListener("keydown", handleKey);
	}, [isOpen, onClose]);

	if (!isOpen) return null;

	return (
		<AnimatePresence>
			<motion.div
				initial={{ opacity: 0 }}
				animate={{ opacity: 1 }}
				exit={{ opacity: 0 }}
				className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
				onClick={(e) => {
					if (e.target === e.currentTarget) onClose();
				}}
			>
				<motion.div
					ref={ref}
					initial={{ opacity: 0, scale: 0.95 }}
					animate={{ opacity: 1, scale: 1 }}
					exit={{ opacity: 0, scale: 0.95 }}
					transition={{ duration: 0.15 }}
					className="w-full max-w-lg rounded-xl border border-app-line bg-app-box p-6 shadow-2xl"
				>
					<h2 className="mb-4 font-plex text-base font-medium text-ink">
						{title}
					</h2>
					{children}
				</motion.div>
			</motion.div>
		</AnimatePresence>
	);
}
