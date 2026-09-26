/**
 * Embeddable entry point for the OpenCode app.
 *
 * Exports a `mountOpenCode` function that renders the full OpenCode SPA
 * into an arbitrary DOM element using a MemoryRouter (no window.history
 * interference). Designed to be consumed by host apps (e.g. Spacebot)
 * that already have their own router.
 *
 * Unlike entry.tsx, this module has NO top-level side effects — it only
 * executes when `mountOpenCode` is called.
 */

import "@/index.css"
import { File } from "@opencode-ai/ui/file"
import { I18nProvider } from "@opencode-ai/ui/context"
import { DialogProvider } from "@opencode-ai/ui/context/dialog"
import { FileComponentProvider } from "@opencode-ai/ui/context/file"
import { MarkedProvider } from "@opencode-ai/ui/context/marked"
import { Font } from "@opencode-ai/ui/font"
import { ThemeProvider, useTheme, type DesktopTheme, type ColorScheme } from "@opencode-ai/ui/theme"
import { MetaProvider } from "@solidjs/meta"
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router"
import { createEffect, createSignal, ErrorBoundary, lazy, onMount, type ParentProps, Show, Suspense } from "solid-js"
import { render } from "solid-js/web"
// Theme overrides use `var(--color-*)` SpaceUI tokens rather than static hex,
// so the embed tracks whichever Spacebot theme class is on <html> (dark,
// midnight, noir, slate, nord, mocha, vanilla). CSS custom properties
// inherit across Shadow DOM boundaries, so the embed picks them up even
// though it mounts into a shadow root. Seeds stay hex — OpenCode's theme
// engine tints/shades them in TypeScript at registration time.
import spacebotTheme from "./spacebot-theme.json"
import { CommandProvider } from "@/context/command"
import { CommentsProvider } from "@/context/comments"
import { FileProvider } from "@/context/file"
import { GlobalSDKProvider } from "@/context/global-sdk"
import { GlobalSyncProvider } from "@/context/global-sync"
import { HighlightsProvider } from "@/context/highlights"
import { LanguageProvider, useLanguage } from "@/context/language"
import { LayoutProvider } from "@/context/layout"
import { ModelsProvider } from "@/context/models"
import { NotificationProvider } from "@/context/notification"
import { PermissionProvider } from "@/context/permission"
import { type Platform, PlatformProvider, usePlatform } from "@/context/platform"
import { PromptProvider } from "@/context/prompt"
import { type ServerConnection, ServerProvider, useServer } from "@/context/server"
import { SettingsProvider } from "@/context/settings"
import { TerminalProvider } from "@/context/terminal"
import DirectoryLayout from "@/pages/directory-layout"
import Layout from "@/pages/layout"
import { ErrorPage } from "./pages/error"

const Home = lazy(() => import("@/pages/home"))
const Session = lazy(() => import("@/pages/session"))
const Loading = () => <div class="size-full" />

const HomeRoute = () => (
  <Suspense fallback={<Loading />}>
    <Home />
  </Suspense>
)

const SessionRoute = () => (
  <SessionProviders>
    <Suspense fallback={<Loading />}>
      <Session />
    </Suspense>
  </SessionProviders>
)

function UiI18nBridge(props: ParentProps) {
  const language = useLanguage()
  return <I18nProvider value={{ locale: language.locale, t: language.t }}>{props.children}</I18nProvider>
}

function MarkedProviderWithNativeParser(props: ParentProps) {
  const platform = usePlatform()
  return <MarkedProvider nativeParser={platform.parseMarkdown}>{props.children}</MarkedProvider>
}

function AppShellProviders(props: ParentProps) {
  return (
    <SettingsProvider>
      <PermissionProvider>
        <LayoutProvider>
          <NotificationProvider>
            <ModelsProvider>
              <CommandProvider>
                <HighlightsProvider>
                  <Layout>{props.children}</Layout>
                </HighlightsProvider>
              </CommandProvider>
            </ModelsProvider>
          </NotificationProvider>
        </LayoutProvider>
      </PermissionProvider>
    </SettingsProvider>
  )
}

function SessionProviders(props: ParentProps) {
  return (
    <TerminalProvider>
      <FileProvider>
        <PromptProvider>
          <CommentsProvider>{props.children}</CommentsProvider>
        </PromptProvider>
      </FileProvider>
    </TerminalProvider>
  )
}

function RouterRoot(props: ParentProps) {
  return <AppShellProviders>{props.children}</AppShellProviders>
}

function ServerKey(props: ParentProps) {
  const server = useServer()
  return (
    <Show when={server.key} keyed>
      {props.children}
    </Show>
  )
}

/**
 * Registers and activates a custom theme + color scheme inside the
 * ThemeProvider. Theme registration runs once on mount; colorScheme
 * tracks props reactively so the host can flip light/dark without
 * remounting (preserves session state).
 */
function ThemeInjector(props: ParentProps & { theme?: DesktopTheme; colorScheme?: ColorScheme }) {
  const ctx = useTheme()
  onMount(() => {
    const theme = props.theme
    if (theme) {
      ctx.registerTheme(theme)
      ctx.setTheme(theme.id)
    }
  })
  // Reactive — re-fires whenever the host updates the colorScheme signal
  createEffect(() => {
    if (props.colorScheme) {
      ctx.setColorScheme(props.colorScheme)
    }
  })
  return <>{props.children}</>
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

export type MountOpenCodeConfig = {
  /** URL of the OpenCode server, e.g. "http://127.0.0.1:12345" */
  serverUrl: string

  /**
   * Initial route to navigate to inside the embedded app.
   * e.g. "/<base64dir>/session/<sessionId>"
   * Defaults to "/" (home / project picker).
   */
  initialRoute?: string

  /**
   * Custom theme to register and activate. If omitted, the built-in
   * Spacebot theme is used. Pass `null` to skip theme injection
   * entirely and use OpenCode's default theme.
   */
  theme?: DesktopTheme | null

  /**
   * Force a color scheme. Defaults to "dark" to match Spacebot's UI.
   * Pass "system" to respect the user's OS preference.
   */
  colorScheme?: ColorScheme
}

export type MountOpenCodeHandle = {
  /** Tear down the SolidJS app and remove all DOM nodes. */
  dispose: () => void

  /**
   * Navigate the embedded app to a new route.
   * e.g. handle.navigate("/<base64dir>/session/<sessionId>")
   */
  navigate: (route: string) => void

  /**
   * Update the active color scheme ("light" | "dark" | "system") at runtime.
   * Lets host apps re-theme the embed when their own theme changes,
   * without remounting the embedded SolidJS app.
   */
  setColorScheme: (scheme: ColorScheme) => void
}

/**
 * Mount the OpenCode SPA into a DOM element.
 *
 * - Uses MemoryRouter so it never touches window.history / window.location.
 * - The caller is responsible for providing a container element (can be inside
 *   a Shadow DOM for CSS isolation).
 * - Returns a handle with `dispose()` for cleanup and `navigate()` for
 *   programmatic route changes.
 */
export function mountOpenCode(
  container: HTMLElement,
  config: MountOpenCodeConfig,
): MountOpenCodeHandle {
  const { serverUrl, initialRoute = "/", colorScheme: initialColorScheme = "dark" } = config
  // Reactive signal so the host can flip color scheme post-mount
  // (handle.setColorScheme) without remounting the SolidJS tree.
  const [colorScheme, setColorScheme] = createSignal<ColorScheme>(initialColorScheme)
  // Resolve theme: undefined → default Spacebot theme, null → no injection
  const theme = config.theme === undefined
    ? (spacebotTheme as DesktopTheme)
    : config.theme ?? undefined

  // Create an in-memory history that never touches the real URL bar.
  const memory = createMemoryHistory()
  // Set the initial route before render so the router starts there.
  memory.set({ value: initialRoute })

  const platform: Platform = {
    platform: "web",
    version: "embed",
    openLink: (url) => window.open(url, "_blank", "noopener,noreferrer"),
    back: () => memory.go(-1),
    forward: () => memory.go(1),
    restart: async () => {
      // No-op in embedded mode — the host app controls lifecycle.
    },
    notify: async () => {
      // Notifications don't make sense in embedded mode.
    },
    // Don't let the embedded app read/write the host's localStorage
    // for defaultServerUrl — we control the server URL via config.
    getDefaultServerUrl: async () => serverUrl,
    setDefaultServerUrl: () => {},
  }

  const server: ServerConnection.Http = {
    type: "http",
    http: { url: serverUrl },
  }
  // Inline ServerConnection.key() to avoid namespace bundling issues.
  // ServerConnection.Key.make is just a branded string cast.
  const serverKey = serverUrl as ServerConnection.Key

  const dispose = render(
    () => (
      <PlatformProvider value={platform}>
        <MetaProvider>
          <Font />
          <ThemeProvider>
            <ThemeInjector theme={theme} colorScheme={colorScheme()}>
            <LanguageProvider>
              <UiI18nBridge>
                <ErrorBoundary fallback={(error) => <ErrorPage error={error} />}>
                  <DialogProvider>
                    <MarkedProviderWithNativeParser>
                      <FileComponentProvider component={File}>
                        <ServerProvider defaultServer={serverKey} servers={[server]}>
                          <ServerKey>
                            <GlobalSDKProvider>
                              <GlobalSyncProvider>
                                <MemoryRouter
                                  history={memory}
                                  root={(routerProps) => (
                                    <RouterRoot>{routerProps.children}</RouterRoot>
                                  )}
                                >
                                  <Route path="/" component={HomeRoute} />
                                  <Route path="/:dir" component={DirectoryLayout}>
                                    <Route path="/" />
                                    <Route path="/session/:id?" component={SessionRoute} />
                                  </Route>
                                </MemoryRouter>
                              </GlobalSyncProvider>
                            </GlobalSDKProvider>
                          </ServerKey>
                        </ServerProvider>
                      </FileComponentProvider>
                    </MarkedProviderWithNativeParser>
                  </DialogProvider>
                </ErrorBoundary>
              </UiI18nBridge>
            </LanguageProvider>
            </ThemeInjector>
          </ThemeProvider>
        </MetaProvider>
      </PlatformProvider>
    ),
    container,
  )

  return {
    dispose,
    navigate: (route: string) => {
      memory.set({ value: route })
    },
    setColorScheme: (scheme: ColorScheme) => {
      setColorScheme(scheme)
    },
  }
}
