import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import "./styles.css";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-sans/700.css";

// WKWebView (macOS) renders at a slightly smaller effective scale than browsers.
// Only apply zoom compensation there — WebView2 (Windows) doesn't need it.
import {IS_DESKTOP, IS_MACOS} from "./platform";
if (IS_DESKTOP && IS_MACOS) {
	document.body.style.zoom = "1.1";
}

ReactDOM.createRoot(document.getElementById("root")!).render(
	<React.StrictMode>
		<App />
	</React.StrictMode>,
);
