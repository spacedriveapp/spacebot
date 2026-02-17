//! Browser automation tool for workers.
//!
//! Provides navigation, element interaction, screenshots, and page observation
//! via headless Chrome using chromiumoxide. Uses an accessibility-tree based
//! ref system for LLM-friendly element addressing.

use crate::config::BrowserConfig;

use chromiumoxide::browser::{Browser, BrowserConfig as ChromeConfig};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide_cdp::cdp::browser_protocol::accessibility::{
    EnableParams as AxEnableParams, GetFullAxTreeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::CaptureScreenshotFormat;
use futures::StreamExt as _;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Tool for browser automation (worker-only).
#[derive(Debug, Clone)]
pub struct BrowserTool {
    state: Arc<Mutex<BrowserState>>,
    config: BrowserConfig,
    screenshot_dir: PathBuf,
}

/// Internal browser state managed across tool invocations within a single worker.
struct BrowserState {
    browser: Option<Browser>,
    /// Background task driving the CDP WebSocket handler.
    _handler_task: Option<JoinHandle<()>>,
    /// Tracked pages by target ID.
    pages: HashMap<String, chromiumoxide::Page>,
    /// Currently active page target ID.
    active_target: Option<String>,
    /// Element ref map from the last snapshot, keyed by ref like "e1".
    element_refs: HashMap<String, ElementRef>,
    /// Counter for generating element refs.
    next_ref: usize,
}

impl std::fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserState")
            .field("has_browser", &self.browser.is_some())
            .field("pages", &self.pages.len())
            .field("active_target", &self.active_target)
            .field("element_refs", &self.element_refs.len())
            .finish()
    }
}

/// Stored info about an element from the accessibility tree snapshot.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ElementRef {
    role: String,
    name: Option<String>,
    description: Option<String>,
    /// The AX node ID from the accessibility tree.
    ax_node_id: String,
    backend_node_id: Option<i64>,
}

impl BrowserTool {
    pub fn new(config: BrowserConfig, screenshot_dir: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(BrowserState {
                browser: None,
                _handler_task: None,
                pages: HashMap::new(),
                active_target: None,
                element_refs: HashMap::new(),
                next_ref: 0,
            })),
            config,
            screenshot_dir,
        }
    }
}

/// Error type for browser tool operations.
#[derive(Debug, thiserror::Error)]
#[error("Browser error: {message}")]
pub struct BrowserError {
    pub message: String,
}

impl BrowserError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The action to perform.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    /// Launch the browser. Must be called before any other action.
    Launch,
    /// Navigate the active tab to a URL.
    Navigate,
    /// Open a new tab, optionally at a URL.
    Open,
    /// List all open tabs.
    Tabs,
    /// Focus a tab by its target ID.
    Focus,
    /// Close a tab by its target ID (or the active tab if omitted).
    CloseTab,
    /// Get an accessibility tree snapshot of the active page with element refs.
    Snapshot,
    /// Perform an interaction on an element by ref.
    Act,
    /// Take a screenshot of the active page or a specific element.
    Screenshot,
    /// Evaluate JavaScript in the active page.
    Evaluate,
    /// Get the page HTML content.
    Content,
    /// Shut down the browser.
    Close,
}

/// The kind of interaction to perform via the `act` action.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActKind {
    /// Click an element by ref.
    Click,
    /// Type text into an element by ref.
    Type,
    /// Press a keyboard key (e.g., "Enter", "Tab", "Escape").
    PressKey,
    /// Hover over an element by ref.
    Hover,
    /// Scroll an element into the viewport by ref.
    ScrollIntoView,
    /// Focus an element by ref.
    Focus,
}

/// Arguments for the browser tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserArgs {
    /// The action to perform.
    pub action: BrowserAction,
    /// URL for navigate/open actions.
    pub url: Option<String>,
    /// Target ID for focus/close_tab actions.
    pub target_id: Option<String>,
    /// Element reference (e.g., "e3") for act/screenshot actions.
    pub element_ref: Option<String>,
    /// Kind of interaction for the act action.
    pub act_kind: Option<ActKind>,
    /// Text to type for act:type.
    pub text: Option<String>,
    /// Key to press for act:press_key (e.g., "Enter", "Tab").
    pub key: Option<String>,
    /// Whether to take a full-page screenshot.
    #[serde(default)]
    pub full_page: bool,
    /// JavaScript expression to evaluate.
    pub script: Option<String>,
}

/// Output from the browser tool.
#[derive(Debug, Serialize)]
pub struct BrowserOutput {
    /// Whether the action succeeded.
    pub success: bool,
    /// Human-readable result message.
    pub message: String,
    /// Page title (when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Current URL (when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Element snapshot data from the accessibility tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<ElementSummary>>,
    /// List of open tabs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<TabInfo>>,
    /// Path to saved screenshot file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    /// JavaScript evaluation result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_result: Option<serde_json::Value>,
    /// Page HTML content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl BrowserOutput {
    fn success(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            title: None,
            url: None,
            elements: None,
            tabs: None,
            screenshot_path: None,
            eval_result: None,
            content: None,
        }
    }

    fn with_page_info(mut self, title: Option<String>, url: Option<String>) -> Self {
        self.title = title;
        self.url = url;
        self
    }
}

/// Summary of an interactive element from the accessibility tree.
#[derive(Debug, Clone, Serialize)]
pub struct ElementSummary {
    /// Short ref like "e1", "e2" for use in subsequent act calls.
    pub ref_id: String,
    /// ARIA role (e.g., "button", "textbox", "link").
    pub role: String,
    /// Accessible name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Accessible description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Current value (for inputs, sliders, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// Info about an open browser tab.
#[derive(Debug, Clone, Serialize)]
pub struct TabInfo {
    pub target_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub active: bool,
}

/// Roles that are interactive and worth assigning refs to.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "link",
    "listbox",
    "menu",
    "menubar",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "scrollbar",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
];

/// Max elements to assign refs to in a single snapshot.
const MAX_ELEMENT_REFS: usize = 200;

impl Tool for BrowserTool {
    const NAME: &'static str = "browser";

    type Error = BrowserError;
    type Args = BrowserArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/browser").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["launch", "navigate", "open", "tabs", "focus", "close_tab",
                                 "snapshot", "act", "screenshot", "evaluate", "content", "close"],
                        "description": "The browser action to perform"
                    },
                    "url": {
                        "type": "string",
                        "description": "URL for navigate/open actions"
                    },
                    "target_id": {
                        "type": "string",
                        "description": "Tab target ID for focus/close_tab actions"
                    },
                    "element_ref": {
                        "type": "string",
                        "description": "Element reference from snapshot (e.g., \"e3\") for act/screenshot"
                    },
                    "act_kind": {
                        "type": "string",
                        "enum": ["click", "type", "press_key", "hover", "scroll_into_view", "focus"],
                        "description": "Kind of interaction for the act action"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type for act:type"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key to press for act:press_key (e.g., \"Enter\", \"Tab\", \"Escape\")"
                    },
                    "full_page": {
                        "type": "boolean",
                        "default": false,
                        "description": "Take full-page screenshot instead of viewport only"
                    },
                    "script": {
                        "type": "string",
                        "description": "JavaScript expression to evaluate (requires evaluate_enabled in config)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.action {
            BrowserAction::Launch => self.handle_launch().await,
            BrowserAction::Navigate => self.handle_navigate(args.url).await,
            BrowserAction::Open => self.handle_open(args.url).await,
            BrowserAction::Tabs => self.handle_tabs().await,
            BrowserAction::Focus => self.handle_focus(args.target_id).await,
            BrowserAction::CloseTab => self.handle_close_tab(args.target_id).await,
            BrowserAction::Snapshot => self.handle_snapshot().await,
            BrowserAction::Act => {
                self.handle_act(args.act_kind, args.element_ref, args.text, args.key)
                    .await
            }
            BrowserAction::Screenshot => {
                self.handle_screenshot(args.element_ref, args.full_page)
                    .await
            }
            BrowserAction::Evaluate => self.handle_evaluate(args.script).await,
            BrowserAction::Content => self.handle_content().await,
            BrowserAction::Close => self.handle_close().await,
        }
    }
}

impl BrowserTool {
    async fn handle_launch(&self) -> Result<BrowserOutput, BrowserError> {
        let mut state = self.state.lock().await;

        if state.browser.is_some() {
            return Ok(BrowserOutput::success("Browser already running"));
        }

        let mut builder = ChromeConfig::builder().no_sandbox();

        if !self.config.headless {
            builder = builder.with_head().window_size(1280, 900);
        }

        if let Some(path) = &self.config.executable_path {
            builder = builder.chrome_executable(path);
        }

        let chrome_config = builder.build().map_err(|error| {
            BrowserError::new(format!("failed to build browser config: {error}"))
        })?;

        tracing::info!(
            headless = self.config.headless,
            executable = ?self.config.executable_path,
            "launching chrome"
        );

        let (browser, mut handler) = Browser::launch(chrome_config)
            .await
            .map_err(|error| BrowserError::new(format!("failed to launch browser: {error}")))?;

        let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        state.browser = Some(browser);
        state._handler_task = Some(handler_task);

        tracing::info!("browser launched");
        Ok(BrowserOutput::success("Browser launched successfully"))
    }

    async fn handle_navigate(&self, url: Option<String>) -> Result<BrowserOutput, BrowserError> {
        let Some(url) = url else {
            return Err(BrowserError::new("url is required for navigate action"));
        };

        let mut state = self.state.lock().await;
        let page = self.get_or_create_page(&mut state, Some(&url)).await?;

        page.goto(&url)
            .await
            .map_err(|error| BrowserError::new(format!("navigation failed: {error}")))?;

        let title = page.get_title().await.ok().flatten();
        let current_url = page.url().await.ok().flatten();

        // Clear stale element refs on navigation
        state.element_refs.clear();
        state.next_ref = 0;

        Ok(
            BrowserOutput::success(format!("Navigated to {url}"))
                .with_page_info(title, current_url),
        )
    }

    async fn handle_open(&self, url: Option<String>) -> Result<BrowserOutput, BrowserError> {
        let mut state = self.state.lock().await;
        let browser = state
            .browser
            .as_ref()
            .ok_or_else(|| BrowserError::new("browser not launched — call launch first"))?;

        let target_url = url.as_deref().unwrap_or("about:blank");

        let page = browser
            .new_page(target_url)
            .await
            .map_err(|error| BrowserError::new(format!("failed to open tab: {error}")))?;

        let target_id = page_target_id(&page);
        let title = page.get_title().await.ok().flatten();
        let current_url = page.url().await.ok().flatten();

        state.pages.insert(target_id.clone(), page);
        state.active_target = Some(target_id.clone());

        // Clear refs when switching pages
        state.element_refs.clear();
        state.next_ref = 0;

        Ok(BrowserOutput {
            tabs: None,
            elements: None,
            screenshot_path: None,
            eval_result: None,
            content: None,
            success: true,
            message: format!("Opened new tab (target: {target_id})"),
            title,
            url: current_url,
        })
    }

    async fn handle_tabs(&self) -> Result<BrowserOutput, BrowserError> {
        let state = self.state.lock().await;

        let mut tabs = Vec::new();
        for (target_id, page) in &state.pages {
            let title = page.get_title().await.ok().flatten();
            let url = page.url().await.ok().flatten();
            let active = state.active_target.as_ref() == Some(target_id);

            tabs.push(TabInfo {
                target_id: target_id.clone(),
                title,
                url,
                active,
            });
        }

        let count = tabs.len();
        Ok(BrowserOutput {
            success: true,
            message: format!("{count} tab(s) open"),
            title: None,
            url: None,
            elements: None,
            tabs: Some(tabs),
            screenshot_path: None,
            eval_result: None,
            content: None,
        })
    }

    async fn handle_focus(&self, target_id: Option<String>) -> Result<BrowserOutput, BrowserError> {
        let Some(target_id) = target_id else {
            return Err(BrowserError::new("target_id is required for focus action"));
        };

        let mut state = self.state.lock().await;

        if !state.pages.contains_key(&target_id) {
            return Err(BrowserError::new(format!(
                "no tab with target_id '{target_id}'"
            )));
        }

        state.active_target = Some(target_id.clone());
        state.element_refs.clear();
        state.next_ref = 0;

        Ok(BrowserOutput::success(format!("Focused tab {target_id}")))
    }

    async fn handle_close_tab(
        &self,
        target_id: Option<String>,
    ) -> Result<BrowserOutput, BrowserError> {
        let mut state = self.state.lock().await;

        let id = target_id
            .or_else(|| state.active_target.clone())
            .ok_or_else(|| BrowserError::new("no active tab to close"))?;

        let page = state
            .pages
            .remove(&id)
            .ok_or_else(|| BrowserError::new(format!("no tab with target_id '{id}'")))?;

        page.close()
            .await
            .map_err(|error| BrowserError::new(format!("failed to close tab: {error}")))?;

        if state.active_target.as_ref() == Some(&id) {
            state.active_target = state.pages.keys().next().cloned();
        }

        state.element_refs.clear();
        state.next_ref = 0;

        Ok(BrowserOutput::success(format!("Closed tab {id}")))
    }

    async fn handle_snapshot(&self) -> Result<BrowserOutput, BrowserError> {
        let mut state = self.state.lock().await;
        let page = self.require_active_page(&state)?.clone();

        // Enable accessibility domain if not already enabled
        page.execute(AxEnableParams::default())
            .await
            .map_err(|error| {
                BrowserError::new(format!("failed to enable accessibility: {error}"))
            })?;

        let ax_tree = page
            .execute(GetFullAxTreeParams::default())
            .await
            .map_err(|error| {
                BrowserError::new(format!("failed to get accessibility tree: {error}"))
            })?;

        state.element_refs.clear();
        state.next_ref = 0;

        let mut elements = Vec::new();

        for node in &ax_tree.result.nodes {
            if node.ignored {
                continue;
            }

            let role = extract_ax_value_string(&node.role);
            let Some(role) = role else { continue };

            let role_lower = role.to_lowercase();
            let is_interactive = INTERACTIVE_ROLES.contains(&role_lower.as_str());

            if !is_interactive {
                continue;
            }

            if state.next_ref >= MAX_ELEMENT_REFS {
                break;
            }

            let name = extract_ax_value_string(&node.name);
            let description = extract_ax_value_string(&node.description);
            let value = extract_ax_value_string(&node.value);
            let backend_node_id = node
                .backend_dom_node_id
                .as_ref()
                .map(|id| id.inner().clone());

            let ref_id = format!("e{}", state.next_ref);
            state.next_ref += 1;

            state.element_refs.insert(
                ref_id.clone(),
                ElementRef {
                    role: role.clone(),
                    name: name.clone(),
                    description: description.clone(),
                    ax_node_id: node.node_id.inner().clone(),
                    backend_node_id,
                },
            );

            elements.push(ElementSummary {
                ref_id,
                role,
                name,
                description,
                value,
            });
        }

        let title = page.get_title().await.ok().flatten();
        let url = page.url().await.ok().flatten();
        let count = elements.len();

        Ok(BrowserOutput {
            success: true,
            message: format!("{count} interactive element(s) found"),
            title,
            url,
            elements: Some(elements),
            tabs: None,
            screenshot_path: None,
            eval_result: None,
            content: None,
        })
    }

    async fn handle_act(
        &self,
        act_kind: Option<ActKind>,
        element_ref: Option<String>,
        text: Option<String>,
        key: Option<String>,
    ) -> Result<BrowserOutput, BrowserError> {
        let Some(act_kind) = act_kind else {
            return Err(BrowserError::new("act_kind is required for act action"));
        };

        let state = self.state.lock().await;
        let page = self.require_active_page(&state)?;

        match act_kind {
            ActKind::Click => {
                let element = self.resolve_element_ref(&state, page, element_ref).await?;
                element
                    .click()
                    .await
                    .map_err(|error| BrowserError::new(format!("click failed: {error}")))?;
                Ok(BrowserOutput::success("Clicked element"))
            }
            ActKind::Type => {
                let Some(text) = text else {
                    return Err(BrowserError::new("text is required for act:type"));
                };
                let element = self.resolve_element_ref(&state, page, element_ref).await?;
                element
                    .click()
                    .await
                    .map_err(|error| BrowserError::new(format!("focus failed: {error}")))?;
                element
                    .type_str(&text)
                    .await
                    .map_err(|error| BrowserError::new(format!("type failed: {error}")))?;
                Ok(BrowserOutput::success(format!(
                    "Typed '{}' into element",
                    truncate_for_display(&text, 50)
                )))
            }
            ActKind::PressKey => {
                let Some(key) = key else {
                    return Err(BrowserError::new("key is required for act:press_key"));
                };
                if element_ref.is_some() {
                    let element = self.resolve_element_ref(&state, page, element_ref).await?;
                    element
                        .press_key(&key)
                        .await
                        .map_err(|error| BrowserError::new(format!("press_key failed: {error}")))?;
                } else {
                    dispatch_key_press(page, &key).await?;
                }
                Ok(BrowserOutput::success(format!("Pressed key '{key}'")))
            }
            ActKind::Hover => {
                let element = self.resolve_element_ref(&state, page, element_ref).await?;
                element
                    .hover()
                    .await
                    .map_err(|error| BrowserError::new(format!("hover failed: {error}")))?;
                Ok(BrowserOutput::success("Hovered over element"))
            }
            ActKind::ScrollIntoView => {
                let element = self.resolve_element_ref(&state, page, element_ref).await?;
                element.scroll_into_view().await.map_err(|error| {
                    BrowserError::new(format!("scroll_into_view failed: {error}"))
                })?;
                Ok(BrowserOutput::success("Scrolled element into view"))
            }
            ActKind::Focus => {
                let element = self.resolve_element_ref(&state, page, element_ref).await?;
                element
                    .focus()
                    .await
                    .map_err(|error| BrowserError::new(format!("focus failed: {error}")))?;
                Ok(BrowserOutput::success("Focused element"))
            }
        }
    }

    async fn handle_screenshot(
        &self,
        element_ref: Option<String>,
        full_page: bool,
    ) -> Result<BrowserOutput, BrowserError> {
        let state = self.state.lock().await;
        let page = self.require_active_page(&state)?;

        let screenshot_data = if let Some(ref_id) = element_ref {
            let element = self.resolve_element_ref(&state, page, Some(ref_id)).await?;
            element
                .screenshot(CaptureScreenshotFormat::Png)
                .await
                .map_err(|error| BrowserError::new(format!("element screenshot failed: {error}")))?
        } else {
            let params = ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(full_page)
                .build();
            page.screenshot(params)
                .await
                .map_err(|error| BrowserError::new(format!("screenshot failed: {error}")))?
        };

        // Save to disk
        let filename = format!(
            "screenshot_{}.png",
            chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f")
        );
        let filepath = self.screenshot_dir.join(&filename);

        tokio::fs::create_dir_all(&self.screenshot_dir)
            .await
            .map_err(|error| {
                BrowserError::new(format!("failed to create screenshot dir: {error}"))
            })?;

        tokio::fs::write(&filepath, &screenshot_data)
            .await
            .map_err(|error| BrowserError::new(format!("failed to save screenshot: {error}")))?;

        let path_str = filepath.to_string_lossy().to_string();
        let size_kb = screenshot_data.len() / 1024;

        tracing::debug!(path = %path_str, size_kb, "screenshot saved");

        Ok(BrowserOutput {
            success: true,
            message: format!("Screenshot saved ({size_kb}KB)"),
            title: None,
            url: None,
            elements: None,
            tabs: None,
            screenshot_path: Some(path_str),
            eval_result: None,
            content: None,
        })
    }

    async fn handle_evaluate(&self, script: Option<String>) -> Result<BrowserOutput, BrowserError> {
        if !self.config.evaluate_enabled {
            return Err(BrowserError::new(
                "JavaScript evaluation is disabled in browser config (set evaluate_enabled = true)",
            ));
        }

        let Some(script) = script else {
            return Err(BrowserError::new("script is required for evaluate action"));
        };

        let state = self.state.lock().await;
        let page = self.require_active_page(&state)?;

        let result = page
            .evaluate(script)
            .await
            .map_err(|error| BrowserError::new(format!("evaluate failed: {error}")))?;

        let value = result.value().cloned();

        Ok(BrowserOutput {
            success: true,
            message: "JavaScript evaluated".to_string(),
            title: None,
            url: None,
            elements: None,
            tabs: None,
            screenshot_path: None,
            eval_result: value,
            content: None,
        })
    }

    async fn handle_content(&self) -> Result<BrowserOutput, BrowserError> {
        let state = self.state.lock().await;
        let page = self.require_active_page(&state)?;

        let html = page
            .content()
            .await
            .map_err(|error| BrowserError::new(format!("failed to get page content: {error}")))?;

        let title = page.get_title().await.ok().flatten();
        let url = page.url().await.ok().flatten();

        // Truncate very large pages for LLM consumption
        let truncated = if html.len() > 100_000 {
            format!(
                "{}... [truncated, {} bytes total]",
                &html[..100_000],
                html.len()
            )
        } else {
            html
        };

        Ok(BrowserOutput {
            success: true,
            message: "Page content retrieved".to_string(),
            title,
            url,
            elements: None,
            tabs: None,
            screenshot_path: None,
            eval_result: None,
            content: Some(truncated),
        })
    }

    async fn handle_close(&self) -> Result<BrowserOutput, BrowserError> {
        let mut state = self.state.lock().await;

        if let Some(mut browser) = state.browser.take() {
            if let Err(error) = browser.close().await {
                tracing::warn!(%error, "browser close returned error");
            }
        }

        state.pages.clear();
        state.active_target = None;
        state.element_refs.clear();
        state.next_ref = 0;
        state._handler_task = None;

        tracing::info!("browser closed");
        Ok(BrowserOutput::success("Browser closed"))
    }

    /// Get the active page, or create a first one if the browser has no pages yet.
    async fn get_or_create_page<'a>(
        &self,
        state: &'a mut BrowserState,
        url: Option<&str>,
    ) -> Result<&'a chromiumoxide::Page, BrowserError> {
        if state.active_target.is_some() {
            let target = state.active_target.as_ref().expect("checked above");
            if state.pages.contains_key(target) {
                return Ok(&state.pages[target]);
            }
        }

        let browser = state
            .browser
            .as_ref()
            .ok_or_else(|| BrowserError::new("browser not launched — call launch first"))?;

        let target_url = url.unwrap_or("about:blank");
        let page = browser
            .new_page(target_url)
            .await
            .map_err(|error| BrowserError::new(format!("failed to create page: {error}")))?;

        let target_id = page_target_id(&page);
        state.pages.insert(target_id.clone(), page);
        state.active_target = Some(target_id.clone());

        Ok(&state.pages[&target_id])
    }

    /// Get the active page or return an error.
    fn require_active_page<'a>(
        &self,
        state: &'a BrowserState,
    ) -> Result<&'a chromiumoxide::Page, BrowserError> {
        let target = state
            .active_target
            .as_ref()
            .ok_or_else(|| BrowserError::new("no active tab — navigate or open a tab first"))?;

        state
            .pages
            .get(target)
            .ok_or_else(|| BrowserError::new("active tab no longer exists"))
    }

    /// Resolve an element ref (like "e3") to a chromiumoxide Element on the page.
    async fn resolve_element_ref(
        &self,
        state: &BrowserState,
        page: &chromiumoxide::Page,
        element_ref: Option<String>,
    ) -> Result<chromiumoxide::Element, BrowserError> {
        let Some(ref_id) = element_ref else {
            return Err(BrowserError::new("element_ref is required for this action"));
        };

        let elem_ref = state.element_refs.get(&ref_id).ok_or_else(|| {
            BrowserError::new(format!(
                "unknown element ref '{ref_id}' — run snapshot first to get element refs"
            ))
        })?;

        // Use backend_node_id to find the element via CSS selector derived from role+name,
        // or fall back to XPath with aria role and name attributes
        let selector = build_selector_for_ref(elem_ref);

        page.find_element(&selector).await.map_err(|error| {
            BrowserError::new(format!(
                "failed to find element for ref '{ref_id}' (selector: {selector}): {error}"
            ))
        })
    }
}

/// Dispatch a key press event to the page via CDP Input domain.
async fn dispatch_key_press(page: &chromiumoxide::Page, key: &str) -> Result<(), BrowserError> {
    let key_down = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyDown)
        .key(key)
        .build()
        .map_err(|error| BrowserError::new(format!("failed to build key event: {error}")))?;

    page.execute(key_down)
        .await
        .map_err(|error| BrowserError::new(format!("key down failed: {error}")))?;

    let key_up = DispatchKeyEventParams::builder()
        .r#type(DispatchKeyEventType::KeyUp)
        .key(key)
        .build()
        .map_err(|error| BrowserError::new(format!("failed to build key event: {error}")))?;

    page.execute(key_up)
        .await
        .map_err(|error| BrowserError::new(format!("key up failed: {error}")))?;

    Ok(())
}

/// Extract the string value from an AxValue option.
fn extract_ax_value_string(
    ax_value: &Option<chromiumoxide_cdp::cdp::browser_protocol::accessibility::AxValue>,
) -> Option<String> {
    let val = ax_value.as_ref()?;
    val.value
        .as_ref()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
}

/// Build a CSS selector from an ElementRef's role and name.
fn build_selector_for_ref(elem_ref: &ElementRef) -> String {
    // Use ARIA role attribute as primary selector, with name for disambiguation
    let role_selector = format!("[role='{}']", elem_ref.role);

    if let Some(name) = &elem_ref.name {
        // Escape single quotes in the name for CSS selector safety
        let escaped = name.replace('\'', "\\'");
        format!("{role_selector}[aria-label='{escaped}']")
    } else {
        role_selector
    }
}

/// Extract target ID string from a Page.
fn page_target_id(page: &chromiumoxide::Page) -> String {
    page.target_id().inner().clone()
}

/// Truncate a string for display, appending "..." if truncated.
fn truncate_for_display(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("{}...", &text[..max_len])
    }
}
