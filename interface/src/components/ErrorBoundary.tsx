import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
	children: ReactNode;
}

interface State {
	hasError: boolean;
	error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
	constructor(props: Props) {
		super(props);
		this.state = { hasError: false, error: null };
	}

	static getDerivedStateFromError(error: Error): State {
		return { hasError: true, error };
	}

	componentDidCatch(error: Error, info: ErrorInfo) {
		console.error("[ErrorBoundary]", error, info.componentStack);
	}

	render() {
		if (!this.state.hasError) {
			return this.props.children;
		}

		return (
			<div className="flex h-screen w-screen items-center justify-center bg-app">
				<div className="max-w-md space-y-4 rounded-lg border border-red-500/20 bg-red-500/10 p-6">
					<h2 className="text-lg font-semibold text-red-400">
						Something went wrong
					</h2>
					<p className="text-sm text-ink-faint">
						The interface crashed unexpectedly. This is usually caused by a
						rendering error.
					</p>
					{this.state.error && (
						<pre className="overflow-auto rounded bg-app-darkBox p-3 text-xs text-ink-dull">
							{this.state.error.message}
						</pre>
					)}
					<button
						type="button"
						onClick={() => window.location.reload()}
						className="rounded bg-red-500/20 px-4 py-2 text-sm font-medium text-red-400 transition-colors hover:bg-red-500/30"
					>
						Reload
					</button>
				</div>
			</div>
		);
	}
}
