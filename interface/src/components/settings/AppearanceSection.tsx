import {
	useTheme,
	THEMES,
	type ThemeId,
	type ThemeMode,
	type ThemeOption,
} from "@/hooks/useTheme";

const MODE_OPTIONS: {id: ThemeMode; label: string; description: string}[] = [
	{id: "light", label: "Light", description: "Always use the chosen light theme"},
	{id: "dark", label: "Dark", description: "Always use the chosen dark theme"},
	{
		id: "system",
		label: "Auto",
		description: "Follow your system's light/dark preference",
	},
];

/** Settings panel: pick mode (light/dark/auto) and the theme used in each
 * surface. The dark/light pickers stay visible together in auto mode so users
 * can preview both without flipping their OS preference. */
export function AppearanceSection() {
	const {
		mode,
		setMode,
		lightTheme,
		setLightTheme,
		darkTheme,
		setDarkTheme,
		theme,
	} = useTheme();

	const lightThemes = THEMES.filter((t) => t.isLight);
	const darkThemes = THEMES.filter((t) => !t.isLight);

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Appearance</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Choose how the dashboard handles light and dark mode.
				</p>
			</div>

			<ModeSelector mode={mode} setMode={setMode} />

			<div className="mt-8 space-y-8">
				{(mode === "light" || mode === "system") && (
					<ThemePickerSection
						title="Light theme"
						subtitle={
							mode === "system"
								? "Used when your system is set to light mode."
								: "Currently active."
						}
						themes={lightThemes}
						selected={lightTheme}
						onSelect={setLightTheme}
						active={theme}
						groupName="theme-light"
					/>
				)}
				{(mode === "dark" || mode === "system") && (
					<ThemePickerSection
						title="Dark theme"
						subtitle={
							mode === "system"
								? "Used when your system is set to dark mode."
								: "Currently active."
						}
						themes={darkThemes}
						selected={darkTheme}
						onSelect={setDarkTheme}
						active={theme}
						groupName="theme-dark"
					/>
				)}
			</div>
		</div>
	);
}

/** Mode picker (light / dark / auto) backed by native radio inputs so screen
 * readers and arrow-key navigation work without custom ARIA. */
function ModeSelector({
	mode,
	setMode,
}: {
	mode: ThemeMode;
	setMode: (m: ThemeMode) => void;
}) {
	return (
		<fieldset className="space-y-2">
			<legend className="text-xs font-semibold uppercase tracking-wide text-ink-dull">
				Mode
			</legend>
			<div className="grid grid-cols-3 gap-2">
				{MODE_OPTIONS.map((opt) => {
					const selected = mode === opt.id;
					return (
						<label
							key={opt.id}
							className={`flex cursor-pointer flex-col items-start rounded-lg border p-3 text-left transition-colors focus-within:ring-2 focus-within:ring-accent ${
								selected
									? "border-accent bg-accent/10"
									: "border-app-line bg-app-box hover:border-app-line/80 hover:bg-app-hover"
							}`}
						>
							<input
								type="radio"
								name="theme-mode"
								value={opt.id}
								checked={selected}
								onChange={() => setMode(opt.id)}
								className="sr-only"
							/>
							<div className="flex w-full items-center justify-between">
								<span className="text-sm font-medium text-ink">
									{opt.label}
								</span>
								{selected && (
									<span className="h-2 w-2 rounded-full bg-accent" />
								)}
							</div>
							<p className="mt-1 text-xs text-ink-dull">{opt.description}</p>
						</label>
					);
				})}
			</div>
		</fieldset>
	);
}

/** Theme grid for a single surface (light or dark). `groupName` must differ
 * between the two pickers so each is its own native radio group. */
function ThemePickerSection({
	title,
	subtitle,
	themes,
	selected,
	onSelect,
	active,
	groupName,
}: {
	title: string;
	subtitle: string;
	themes: ThemeOption[];
	selected: ThemeId;
	onSelect: (id: ThemeId) => void;
	active: ThemeId;
	groupName: string;
}) {
	return (
		<fieldset>
			<div className="mb-3">
				<legend className="text-xs font-semibold uppercase tracking-wide text-ink-dull">
					{title}
				</legend>
				<p className="mt-1 text-xs text-ink-faint">{subtitle}</p>
			</div>
			<div className="grid grid-cols-2 gap-3">
				{themes.map((t) => {
					const isSelected = selected === t.id;
					const isActive = active === t.id;
					return (
						<label
							key={t.id}
							className={`group relative flex cursor-pointer flex-col items-start rounded-lg border p-4 text-left transition-colors focus-within:ring-2 focus-within:ring-accent ${
								isSelected
									? "border-accent bg-accent/10"
									: "border-app-line bg-app-box hover:border-app-line/80 hover:bg-app-hover"
							}`}
						>
							<input
								type="radio"
								name={groupName}
								value={t.id}
								checked={isSelected}
								onChange={() => onSelect(t.id)}
								className="sr-only"
							/>
							<div className="flex w-full items-center justify-between gap-2">
								<span className="text-sm font-medium text-ink">{t.name}</span>
								<div className="flex items-center gap-1">
									{isActive && !isSelected && (
										<span
											className="text-tiny text-ink-faint"
											title="Currently active"
										>
											• active
										</span>
									)}
									{isSelected && (
										<span className="h-2 w-2 rounded-full bg-accent" />
									)}
								</div>
							</div>
							<p className="mt-1 text-sm text-ink-dull">{t.description}</p>
							<ThemePreview themeId={t.id} />
						</label>
					);
				})}
			</div>
		</fieldset>
	);
}

/** Inline preview swatches per theme — cheap visual cue without computing
 * resolved CSS vars. Pull a representative bg/sidebar/accent from each
 * theme's known palette. */
const PREVIEW_COLORS: Record<
	ThemeId,
	{bg: string; sidebar: string; accent: string}
> = {
	default: {bg: "#1c1d26", sidebar: "#101118", accent: "#2499ff"},
	vanilla: {bg: "#ffffff", sidebar: "#f5f5f6", accent: "#2499ff"},
	midnight: {bg: "#121428", sidebar: "#0a0b14", accent: "#2499ff"},
	noir: {bg: "#080808", sidebar: "#000000", accent: "#2499ff"},
	slate: {bg: "#151619", sidebar: "#0e0f12", accent: "#2499ff"},
	nord: {bg: "#1a1e27", sidebar: "#11141b", accent: "#2499ff"},
	mocha: {bg: "#1a1614", sidebar: "#110f0d", accent: "#2499ff"},
	"catppuccin-latte": {bg: "#eff1f5", sidebar: "#e6e9ef", accent: "#8839ef"},
	"solarized-light": {bg: "#fdf6e3", sidebar: "#eee8d5", accent: "#268bd2"},
	"solarized-dark": {bg: "#002b36", sidebar: "#073642", accent: "#268bd2"},
};

/** Tiny inline swatch (background + sidebar + accent stripes) drawn from a
 * static palette so we don't have to compute resolved CSS vars per card. */
function ThemePreview({themeId}: {themeId: ThemeId}) {
	const c = PREVIEW_COLORS[themeId];

	return (
		<div
			className="mt-3 flex h-12 w-full overflow-hidden rounded border border-app-line/50"
			style={{backgroundColor: c.bg}}
		>
			<div
				className="w-8 border-r"
				style={{backgroundColor: c.sidebar, borderColor: c.accent + "30"}}
			/>
			<div className="flex flex-1 flex-col gap-1 p-1.5">
				<div
					className="h-1.5 w-12 rounded-sm"
					style={{backgroundColor: c.accent}}
				/>
				<div
					className="h-1 w-16 rounded-sm opacity-30"
					style={{backgroundColor: c.accent}}
				/>
				<div
					className="h-1 w-10 rounded-sm opacity-20"
					style={{backgroundColor: c.accent}}
				/>
			</div>
		</div>
	);
}
