import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

export default defineConfig({
	plugins: [react()],
	resolve: {
		alias: {
			"@": path.resolve(__dirname, "src"),
		},
	},
	server: {
		port: 19840,
		proxy: {
			"/api": {
				target: "http://127.0.0.1:19898",
				changeOrigin: true,
				// SSE: the default http-proxy timeout (2 min) kills long-lived
				// event-stream connections.  Setting timeout to 0 disables it.
				// The proxyRes handler also strips buffering hints so chunks
				// flush immediately.
				timeout: 0,
				configure: (proxy) => {
					proxy.on("proxyReq", (_proxyReq, req, _res) => {
						// Disable socket timeout for SSE requests so Node
						// doesn't close the connection after 2 minutes of
						// "inactivity" (SSE heartbeats aren't frequent enough).
						if (req.headers.accept?.includes("text/event-stream")) {
							_proxyReq.socket?.setTimeout?.(0);
						}
					});
					proxy.on("proxyRes", (proxyRes, req) => {
						const ct = proxyRes.headers["content-type"] ?? "";
						if (ct.includes("text/event-stream")) {
							proxyRes.headers["cache-control"] = "no-cache";
							proxyRes.headers["x-accel-buffering"] = "no";
							// Keep the socket alive indefinitely for SSE
							proxyRes.socket?.setTimeout?.(0);
							req.socket?.setTimeout?.(0);
						}
					});
				},
			},
		},
	},
	build: {
		outDir: "dist",
		emptyOutDir: true,
		sourcemap: true,
	},
});
