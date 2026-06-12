import {
	Button,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogDescription,
	DialogFooter,
} from "@spacedrive/primitives";
import {ProviderIcon} from "@/lib/providerIcons";
import type {ChatGptOAuthDialogProps} from "./types";

export function ChatGptOAuthDialog({
	open,
	onOpenChange,
	isRequesting,
	isPolling,
	message,
	deviceCodeInfo,
	deviceCodeCopied,
	onCopyDeviceCode,
	onOpenDeviceLogin,
	onRestart,
}: ChatGptOAuthDialogProps) {
	return (
		<DialogRoot open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-md">
				<DialogHeader>
					<DialogTitle className="flex items-center gap-2">
						<ProviderIcon provider="openai-chatgpt" size={20} />
						Sign in with ChatGPT Plus
					</DialogTitle>
					{!message && (
						<DialogDescription>
							Copy the device code below, then sign in to your OpenAI account to
							authorize access. You must first{" "}
							<a
								href="https://chatgpt.com/security-settings"
								target="_blank"
								rel="noopener noreferrer"
								className="underline text-accent hover:text-accent/80"
							>
								enable device code login
							</a>{" "}
							in your ChatGPT security settings.
						</DialogDescription>
					)}
				</DialogHeader>

				<div className="space-y-4">
					{message && !deviceCodeInfo ? (
						/* Completed state — success or error with no active flow */
						<div
							className={`rounded-md border px-3 py-2 text-sm ${
								message.type === "success"
									? "border-green-500/20 bg-green-500/10 text-green-400"
									: "border-red-500/20 bg-red-500/10 text-red-400"
							}`}
						>
							{message.text}
						</div>
					) : isRequesting && !deviceCodeInfo ? (
						<div className="flex items-center gap-2 text-sm text-ink-dull">
							<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							Requesting device code...
						</div>
					) : deviceCodeInfo ? (
						<div className="space-y-4">
							<div className="rounded-md border border-app-line p-3">
								<div className="flex items-center gap-2">
									<span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent/15 text-[11px] font-semibold text-accent">
										1
									</span>
									<p className="text-sm text-ink-dull">Copy this device code</p>
								</div>
								<div className="mt-2.5 flex items-center gap-2 pl-7">
									<code className="rounded border border-app-line bg-app-darkerBox px-3 py-1.5 font-mono text-base tracking-widest text-ink">
										{deviceCodeInfo.userCode}
									</code>
									<Button
										onClick={onCopyDeviceCode}
										size="md"
										variant={deviceCodeCopied ? "gray" : "outline"}
									>
										{deviceCodeCopied ? "Copied" : "Copy"}
									</Button>
								</div>
							</div>

							<div
								className={`rounded-md border border-app-line p-3 ${!deviceCodeCopied ? "opacity-50" : ""}`}
							>
								<div className="flex items-center gap-2">
									<span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent/15 text-[11px] font-semibold text-accent">
										2
									</span>
									<p className="text-sm text-ink-dull">
										Open OpenAI and paste the code
									</p>
								</div>
								<div className="mt-2.5 pl-7">
									<Button
										onClick={onOpenDeviceLogin}
										disabled={!deviceCodeCopied}
										size="md"
										variant="outline"
									>
										Open login page
									</Button>
								</div>
							</div>

							{isPolling && !message && (
								<div className="flex items-center gap-2 text-sm text-ink-faint">
									<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
									Waiting for sign-in confirmation...
								</div>
							)}

							{message && (
								<div
									className={`rounded-md border px-3 py-2 text-sm ${
										message.type === "success"
											? "border-green-500/20 bg-green-500/10 text-green-400"
											: "border-red-500/20 bg-red-500/10 text-red-400"
									}`}
								>
									{message.text}
								</div>
							)}
						</div>
					) : null}
				</div>

				<DialogFooter>
					{message && !deviceCodeInfo ? (
						/* Completed — show Done (or Retry for errors) */
						message.type === "success" ? (
							<Button onClick={() => onOpenChange(false)} size="md">
								Done
							</Button>
						) : (
							<>
								<Button
									onClick={() => onOpenChange(false)}
									variant="outline"
									size="md"
								>
									Close
								</Button>
								<Button
									onClick={onRestart}
									disabled={isRequesting}
									loading={isRequesting}
									size="md"
								>
									Try again
								</Button>
							</>
						)
					) : (
						<>
							<Button
								onClick={() => onOpenChange(false)}
								variant="outline"
								size="md"
							>
								Cancel
							</Button>
							{deviceCodeInfo && (
								<Button
									onClick={onRestart}
									disabled={isRequesting}
									loading={isRequesting}
									variant="outline"
									size="md"
								>
									Get new code
								</Button>
							)}
						</>
					)}
				</DialogFooter>
			</DialogContent>
		</DialogRoot>
	);
}
