import { useSetTopBar } from "@/components/TopBar";

const TIERS = [
	{
		name: "Open Source",
		price: "Free",
		subtitle: "For individual developers",
		note: null,
		cta: { label: "Get Started", href: "https://github.com/spacedriveapp/spacebot" },
		highlight: false,
		features: [
			"Full Agent Orchestration",
			"CLI + Web Dashboard",
			"BYOK — Bring Your Own API Key",
			"All Messaging Adapters",
			"MCP Marketplace",
			"Multi-Agent Kanban Board",
			"Community Support",
		],
	},
	{
		name: "Teams",
		price: "\u20AC0/mo*",
		subtitle: "*through 2026",
		note: "then \u20AC20/mo/user",
		cta: { label: "Get Started", href: "#" },
		highlight: true,
		features: [
			"All Features of OSS +",
			"Centralized Billing",
			"Team Management Dashboard",
			"Role-Based Access Control",
			"Limit Inference Providers",
			"Priority Support",
		],
	},
	{
		name: "Enterprise",
		price: "Custom",
		subtitle: "SSO, SLA & Dedicated Support",
		note: null,
		cta: { label: "Contact Sales", href: "mailto:mail@marcmantei.com" },
		highlight: false,
		features: [
			"All Features in Teams +",
			"SSO / OIDC",
			"SLA",
			"Dedicated Support",
			"Audit Logs",
			"Advanced Config Management",
			"Fine-Grained Permissioning",
		],
	},
] as const;

export function Pricing() {
	useSetTopBar(
		<div className="flex h-full items-center px-6">
			<h1 className="font-plex text-sm font-medium text-ink">Pricing</h1>
		</div>,
	);

	return (
		<div className="flex flex-1 flex-col overflow-y-auto">
			<div className="mx-auto w-full max-w-5xl px-6 py-12">
				{/* Header */}
				<div className="mb-12 text-center">
					<h1 className="font-plex text-4xl font-bold text-ink">
						Simple, transparent pricing
					</h1>
					<p className="mx-auto mt-4 max-w-xl text-base text-ink-dull">
						Spacebot is free for individual developers. Pay only for AI
						inference on a usage basis — no subscriptions, no vendor lock-in.
					</p>
				</div>

				{/* Tier Cards */}
				<div className="grid gap-6 md:grid-cols-3">
					{TIERS.map((tier) => (
						<div
							key={tier.name}
							className={`flex flex-col rounded-lg border p-6 ${
								tier.highlight
									? "border-accent bg-accent/5"
									: "border-app-line bg-app-box"
							}`}
						>
							{/* Tier Header */}
							<div className="mb-6 text-center">
								<p className="text-xs font-semibold uppercase tracking-wider text-ink-faint">
									{tier.name}
								</p>
								<p className="mt-3 font-plex text-4xl font-bold text-ink">
									{tier.price}
								</p>
								<p className="mt-1 text-sm text-ink-dull">
									{tier.subtitle}
								</p>
								{tier.note && (
									<p className="mt-0.5 text-xs text-ink-faint">
										{tier.note}
									</p>
								)}
							</div>

							{/* CTA */}
							<a
								href={tier.cta.href}
								target="_blank"
								rel="noopener noreferrer"
								className={`mb-6 block rounded-md px-4 py-2.5 text-center text-sm font-medium transition-colors ${
									tier.highlight
										? "bg-accent text-white hover:bg-accent-deep"
										: "bg-app-button text-ink hover:bg-app-hover"
								}`}
							>
								{tier.cta.label}
							</a>

							{/* Feature Divider Label */}
							<p className="mb-3 text-center text-xs font-medium text-ink-faint">
								Features
							</p>
							<div className="mb-4 h-px bg-app-line" />

							{/* Feature List */}
							<ul className="flex-1 space-y-3">
								{tier.features.map((feature) => (
									<li
										key={feature}
										className="flex items-start gap-2 text-sm text-ink-dull"
									>
										<svg
											className="mt-0.5 h-4 w-4 shrink-0 text-accent"
											viewBox="0 0 20 20"
											fill="currentColor"
										>
											<path
												fillRule="evenodd"
												d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z"
												clipRule="evenodd"
											/>
										</svg>
										{feature}
									</li>
								))}
							</ul>
						</div>
					))}
				</div>

				{/* Footer Note */}
				<div className="mt-10 text-center">
					<p className="text-sm text-ink-faint">
						Inference costs are paid directly to your LLM provider.
						No markup, no hidden fees.
					</p>
				</div>
			</div>
		</div>
	);
}
