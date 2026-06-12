import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
	plugins: [react(), tailwindcss()],

	resolve: {
		dedupe: ["react", "react-dom"],
		alias: [
			// Pin React to a single copy (prevents "Invalid hook call")
			{
				find: /^react$/,
				replacement: path.resolve(
					__dirname,
					"./node_modules/react/index.js",
				),
			},
			{
				find: /^react\/jsx-runtime$/,
				replacement: path.resolve(
					__dirname,
					"./node_modules/react/jsx-runtime.js",
				),
			},
			{
				find: /^react\/jsx-dev-runtime$/,
				replacement: path.resolve(
					__dirname,
					"./node_modules/react/jsx-dev-runtime.js",
				),
			},
			{
				find: /^react-dom$/,
				replacement: path.resolve(
					__dirname,
					"./node_modules/react-dom/index.js",
				),
			},
			{
				find: /^react-dom\/client$/,
				replacement: path.resolve(
					__dirname,
					"./node_modules/react-dom/client.js",
				),
			},

			// Project alias
			{ find: "@", replacement: path.resolve(__dirname, "src") },
		],
	},

	optimizeDeps: {
		exclude: [
			"@spacedrive/tokens",
			"@spacedrive/primitives",
			"@spacedrive/ai",
			"@spacedrive/forms",
			"@spacedrive/explorer",
		],
	},

	server: {
		port: 19840,
		fs: {
			allow: [path.resolve(__dirname, "..")],
		},
		proxy: {
			"/api": {
				target: "http://127.0.0.1:19898",
				changeOrigin: true,
				timeout: 0,
				configure: (proxy) => {
					proxy.on("proxyReq", (_proxyReq, req, _res) => {
						if (req.headers.accept?.includes("text/event-stream")) {
							_proxyReq.socket?.setTimeout?.(0);
						}
					});
					proxy.on("proxyRes", (proxyRes, req) => {
						const ct = proxyRes.headers["content-type"] ?? "";
						if (ct.includes("text/event-stream")) {
							proxyRes.headers["cache-control"] = "no-cache";
							proxyRes.headers["x-accel-buffering"] = "no";
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
