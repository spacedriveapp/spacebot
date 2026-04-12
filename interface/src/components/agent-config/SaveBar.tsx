import {motion, AnimatePresence} from "framer-motion";
import {Button} from "@spacedrive/primitives";

interface SaveBarProps {
	dirty: boolean;
	saving: boolean;
	onSave: () => void;
	onRevert: () => void;
}

export function SaveBar({dirty, saving, onSave, onRevert}: SaveBarProps) {
	return (
		<AnimatePresence>
			{dirty && (
				<motion.div
					initial={{y: 100}}
					animate={{y: 0}}
					exit={{y: 100}}
					transition={{type: "spring", damping: 25, stiffness: 300}}
					className="absolute bottom-4 right-4 flex items-center gap-4 rounded-lg border border-app-line/50 bg-app-dark-box px-4 py-3 shadow-lg"
				>
					<span className="text-sm text-ink-dull">
						You have unsaved changes
					</span>
					<div className="flex items-center gap-2">
						<Button onClick={onRevert} variant="gray" size="sm">
							Revert
						</Button>
						<Button
							onClick={onSave}
							variant="accent"
							size="sm"
							disabled={saving}
						>
							Save Changes
						</Button>
					</div>
				</motion.div>
			)}
		</AnimatePresence>
	);
}
