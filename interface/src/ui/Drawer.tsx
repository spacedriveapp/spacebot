import * as RDialog from "@radix-ui/react-dialog";
import clsx from "clsx";
import {motion, AnimatePresence} from "framer-motion";
import type {ReactNode} from "react";

export type DrawerSide = "left" | "right" | "bottom";

interface DrawerProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	side?: DrawerSide;
	children: ReactNode;
	className?: string;
	ariaLabel?: string;
}

const sideAnim: Record<
	DrawerSide,
	{
		initial: object;
		animate: object;
		exit: object;
		positionClass: string;
	}
> = {
	left: {
		initial: {x: "-100%"},
		animate: {x: 0},
		exit: {x: "-100%"},
		positionClass: "inset-y-0 left-0 h-full w-[85vw] max-w-[320px]",
	},
	right: {
		initial: {x: "100%"},
		animate: {x: 0},
		exit: {x: "100%"},
		positionClass: "inset-y-0 right-0 h-full w-[85vw] max-w-[320px]",
	},
	bottom: {
		initial: {y: "100%"},
		animate: {y: 0},
		exit: {y: "100%"},
		positionClass: "inset-x-0 bottom-0 max-h-[85vh] w-full",
	},
};

export function Drawer({
	open,
	onOpenChange,
	side = "left",
	children,
	className,
	ariaLabel,
}: DrawerProps) {
	const cfg = sideAnim[side];

	return (
		<RDialog.Root open={open} onOpenChange={onOpenChange}>
			<AnimatePresence>
				{open && (
					<RDialog.Portal forceMount>
						<RDialog.Overlay asChild forceMount>
							<motion.div
								initial={{opacity: 0}}
								animate={{opacity: 1}}
								exit={{opacity: 0}}
								transition={{duration: 0.18}}
								className="fixed inset-0 z-50 bg-black/50 backdrop-blur-sm"
							/>
						</RDialog.Overlay>
						<RDialog.Content asChild forceMount aria-label={ariaLabel}>
							<motion.div
								initial={cfg.initial}
								animate={cfg.animate}
								exit={cfg.exit}
								transition={{type: "tween", duration: 0.22, ease: "easeOut"}}
								className={clsx(
									"fixed z-50 flex flex-col overflow-hidden border-app-line bg-sidebar shadow-2xl",
									cfg.positionClass,
									side === "left" && "border-r",
									side === "right" && "border-l",
									side === "bottom" && "rounded-t-2xl border-t",
									className,
								)}
							>
								<RDialog.Title className="sr-only">
									{ariaLabel || "Drawer"}
								</RDialog.Title>
								{children}
							</motion.div>
						</RDialog.Content>
					</RDialog.Portal>
				)}
			</AnimatePresence>
		</RDialog.Root>
	);
}
