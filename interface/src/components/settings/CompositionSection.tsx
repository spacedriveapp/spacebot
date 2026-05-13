import {useChatInputPrefs} from "@/hooks/useChatInputPrefs";

export function CompositionSection() {
	const {prefs, setEnterToSubmit} = useChatInputPrefs();

	const options: {
		value: boolean;
		title: string;
		description: string;
	}[] = [
		{
			value: true,
			title: "Enter to send",
			description: "Enter sends the message. Shift+Enter inserts a new line.",
		},
		{
			value: false,
			title: "Enter for new line",
			description:
				"Enter inserts a new line. ⌘/Ctrl+Enter (or Option+Enter) sends.",
		},
	];

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Message input
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Choose how the Enter key behaves in the chat composer.
				</p>
			</div>
			<div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
				{options.map((opt) => {
					const active = prefs.enterToSubmit === opt.value;
					return (
						<button
							key={String(opt.value)}
							onClick={() => setEnterToSubmit(opt.value)}
							className={`flex flex-col items-start rounded-lg border p-4 text-left transition-colors ${
								active
									? "border-accent bg-accent/10"
									: "border-app-line bg-app-box hover:border-app-line/80 hover:bg-app-hover"
							}`}
						>
							<div className="flex w-full items-center justify-between">
								<span className="text-sm font-medium text-ink">
									{opt.title}
								</span>
								{active && <span className="h-2 w-2 rounded-full bg-accent" />}
							</div>
							<p className="mt-1 text-sm text-ink-dull">{opt.description}</p>
						</button>
					);
				})}
			</div>
		</div>
	);
}
