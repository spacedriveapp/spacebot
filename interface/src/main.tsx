import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import "./ui/style/style.scss";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-sans/700.css";

// WKWebView (macOS) renders at a slightly smaller effective scale than browsers.
// Only apply zoom compensation there — WebView2 (Windows) doesn't need it.
import { IS_TAURI } from "./platform";
if (IS_TAURI) {
	const isMac = /mac/i.test((navigator as any).userAgentData?.platform ?? navigator.platform ?? "");
	if (isMac) {
		document.body.style.zoom = "1.1";
	}
}

ReactDOM.createRoot(document.getElementById("root")!).render(
	<React.StrictMode>
		<App />
	</React.StrictMode>,
);
