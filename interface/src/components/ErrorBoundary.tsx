import {Component, type ErrorInfo, type ReactNode} from "react";

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
		this.state = {hasError: false, error: null};
	}

	static getDerivedStateFromError(error: Error): State {
		return {hasError: true, error};
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
				<div className="max-w-md space-y-4 rounded-lg border border-status-error/20 bg-status-error/10 p-6">
					<h2 className="text-lg font-semibold text-status-error">
						Something went wrong
					</h2>
					<p className="text-sm text-ink-faint">
						The interface crashed unexpectedly. This is usually caused by a
						rendering error.
					</p>
					{this.state.error && (
						<pre className="overflow-auto rounded bg-app-dark-box p-3 text-xs text-ink-dull">
							{this.state.error.message}
						</pre>
					)}
					<button
						type="button"
						onClick={() => window.location.reload()}
						className="rounded bg-status-error/20 px-4 py-2 text-sm font-medium text-status-error transition-colors hover:bg-status-error/30"
					>
						Reload
					</button>
				</div>
			</div>
		);
	}
}
