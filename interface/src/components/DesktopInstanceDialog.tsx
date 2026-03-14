import { useState, type FormEvent } from "react";
import { LinkSquare02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useDesktopInstance } from "@/hooks/useDesktopInstance";
import {
	Button,
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
	Dialog,
	DialogContent,
	DialogDescription,
	DialogHeader,
	DialogTitle,
	Input,
} from "@/ui";

function ConnectForm({ onCancel }: { onCancel?: () => void }) {
	const { currentUrl, connectToInstance } = useDesktopInstance();
	const [value, setValue] = useState(currentUrl);
	const [error, setError] = useState<string | null>(null);
	const [isSaving, setIsSaving] = useState(false);

	const handleSubmit = (event: FormEvent) => {
		event.preventDefault();
		setIsSaving(true);
		setError(null);
		try {
			connectToInstance(value);
		} catch (submitError) {
			setError(submitError instanceof Error ? submitError.message : "Invalid URL.");
			setIsSaving(false);
		}
	};

	return (
		<form className="space-y-5" onSubmit={handleSubmit}>
			<div className="space-y-2">
				<label htmlFor="desktop-instance-url" className="sr-only">
					Spacebot server URL
				</label>
				<Input
					id="desktop-instance-url"
					type="url"
					value={value}
					onChange={(event) => setValue(event.target.value)}
					placeholder="http://127.0.0.1:19898"
					icon={<HugeiconsIcon icon={LinkSquare02Icon} size={14} />}
					size="md"
					autoFocus
					aria-invalid={!!error}
					aria-describedby={error ? "desktop-instance-url-error" : undefined}
				/>
				{error ? (
					<p id="desktop-instance-url-error" className="text-xs text-red-400">
						{error}
					</p>
				) : null}
			</div>

			<div className="flex items-center justify-end gap-2">
				{onCancel ? (
					<Button type="button" variant="ghost" onClick={onCancel}>
						Cancel
					</Button>
				) : null}
				<Button type="submit" loading={isSaving}>
					Connect
				</Button>
			</div>
		</form>
	);
}

export function DesktopInstanceDialog() {
	const { dialogOpen, closeDialog } = useDesktopInstance();

	return (
		<Dialog open={dialogOpen} onOpenChange={(open) => !open && closeDialog()}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle>Change Server</DialogTitle>
					<DialogDescription>
						Point this app at a different Spacebot server.
					</DialogDescription>
				</DialogHeader>
				<ConnectForm onCancel={closeDialog} />
			</DialogContent>
		</Dialog>
	);
}

export function DesktopInstanceGate() {
	const { isDesktop } = useDesktopInstance();

	if (!isDesktop) return null;

	return (
		<div className="flex h-full items-center justify-center bg-app">
			<Card className="w-full max-w-sm">
				<CardHeader className="pb-4">
					<CardTitle>Connect to Spacebot</CardTitle>
					<CardDescription>Enter the URL of a running Spacebot server.</CardDescription>
				</CardHeader>
				<CardContent>
					<ConnectForm />
				</CardContent>
			</Card>
		</div>
	);
}
