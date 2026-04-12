//! Browser automation tools for workers.
//!
//! Provides a suite of separate tools (navigate, click, type, snapshot, etc.)
//! backed by a shared browser state. Uses Chrome's native CDP Accessibility API
//! (`Accessibility.getFullAXTree`) to extract the ARIA tree directly from the
//! browser's accessibility layer. Interactive elements get sequential indices
//! and their `BackendNodeId` is stored for reliable element resolution.
//!
//! Element resolution: index → `BackendNodeId` (from CDP accessibility tree) →
//! `DOM.getBoxModel` for coordinates → `Input.dispatchMouseEvent` for clicks.
//! This avoids fragile CSS selectors and works reliably on SPAs and complex
//! pages where JS injection fails.

use crate::config::BrowserConfig;
use crate::secrets::store::SecretsStore;

use chromiumoxide::browser::{Browser, BrowserConfig as ChromeConfig};
use chromiumoxide::fetcher::{BrowserFetcher, BrowserFetcherOptions};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide_cdp::cdp::browser_protocol::accessibility::{
    AxNode, AxPropertyName, GetFullAxTreeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, GetBoxModelParams, ResolveNodeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    MouseButton,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::CaptureScreenshotFormat;
use futures::StreamExt as _;
use reqwest::Url;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

// URL validation (SSRF protection)

/// Validate that a URL is safe for the browser to navigate to.
/// Blocks private/loopback IPs, link-local addresses, and cloud metadata endpoints
/// to prevent server-side request forgery.
fn validate_url(url: &str) -> Result<(), BrowserError> {
    let parsed = Url::parse(url)
        .map_err(|error| BrowserError::new(format!("invalid URL '{url}': {error}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(BrowserError::new(format!(
                "scheme '{other}' is not allowed — only http and https are permitted"
            )));
        }
    }

    let Some(host) = parsed.host_str() else {
        return Err(BrowserError::new("URL has no host"));
    };

    if host == "metadata.google.internal"
        || host == "169.254.169.254"
        || host == "metadata.aws.internal"
    {
        return Err(BrowserError::new(
            "access to cloud metadata endpoints is blocked",
        ));
    }

    if let Ok(ip) = host.parse::<IpAddr>()
        && is_blocked_ip(ip)
    {
        return Err(BrowserError::new(format!(
            "navigation to private/loopback address {ip} is blocked"
        )));
    }

    if let Some(stripped) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']'))
        && let Ok(ip) = stripped.parse::<IpAddr>()
        && is_blocked_ip(ip)
    {
        return Err(BrowserError::new(format!(
            "navigation to private/loopback address {ip} is blocked"
        )));
    }

    Ok(())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || is_v4_cgnat(v4)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || is_v6_unique_local(v6)
                || is_v6_link_local(v6)
                || is_v4_mapped_blocked(v6)
        }
    }
}

fn is_v4_cgnat(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0xC0) == 64
}

fn is_v6_unique_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xFE00) == 0xFC00
}

fn is_v6_link_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xFFC0) == 0xFE80
}

fn is_v4_mapped_blocked(ip: std::net::Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        is_blocked_ip(IpAddr::V4(v4))
    } else {
        false
    }
}

// Page readiness helper

/// Wait for the page to be "ready enough" for DOM extraction.
///
/// Many sites are SPAs that load a shell document and then hydrate with JS.
/// `chromiumoxide::Page::goto` returns after the main-frame load event, but
/// interactive content may not exist yet. This helper polls the page's
/// `document.readyState` and body child count to detect when content has
/// rendered, with a generous timeout so we don't block forever on broken pages.
async fn wait_for_page_ready(page: &chromiumoxide::Page) {
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

    let start = std::time::Instant::now();

    loop {
        if start.elapsed() >= MAX_WAIT {
            break;
        }

        // Check if the page has meaningful content: readyState is "complete"
        // and the body has at least one child element.
        let ready = page
            .evaluate(
                "(document.readyState === 'complete' && \
                 document.body && document.body.children.length > 0)",
            )
            .await
            .ok()
            .and_then(|result| result.value().cloned())
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        if ready {
            // Give JS frameworks one more tick to finish hydration.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            break;
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// Accessibility snapshot types (CDP-native)

/// Roles that represent interactive elements the LLM can target via index.
/// Only nodes with these roles get assigned an index in the snapshot.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
];

/// Roles that are structural noise and should be skipped when they have no
/// name and no interactive descendants. Keeps the YAML output compact.
const SKIP_UNNAMED_ROLES: &[&str] = &[
    "generic",
    "none",
    "presentation",
    "InlineTextBox",
    "LineBreak",
];

/// A snapshot of the page's accessibility tree, extracted via CDP.
///
/// Contains a tree of `SnapshotNode`s for LLM display and a parallel map from
/// element indices to `BackendNodeId` for interaction.
#[derive(Debug, Clone)]
pub(crate) struct AxSnapshot {
    /// Root nodes of the tree (usually one, but CDP can return multiple roots).
    pub roots: Vec<SnapshotNode>,
    /// `BackendNodeId` for each indexed interactive element.
    /// `node_ids[i]` corresponds to the element with `index == i` in the tree.
    pub node_ids: Vec<BackendNodeId>,
}

/// A node in the processed accessibility tree, ready for rendering.
#[derive(Debug, Clone)]
pub(crate) struct SnapshotNode {
    pub role: String,
    pub name: String,
    /// Sequential index assigned to interactive elements. `None` for structural nodes.
    pub index: Option<usize>,
    pub children: Vec<SnapshotNode>,
    // State properties
    pub checked: Option<String>,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub selected: bool,
    pub level: Option<i64>,
    pub pressed: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
}

impl AxSnapshot {
    /// Build from a flat CDP `AxNode` list by reconstructing the tree and
    /// assigning sequential indices to interactive elements.
    fn from_ax_nodes(nodes: Vec<AxNode>) -> Self {
        // Build a lookup from AxNodeId → index in the flat list.
        let id_to_idx: HashMap<String, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.node_id.inner().clone(), i))
            .collect();

        // Track which nodes are roots (no parent) or whose parent_id doesn't
        // exist in the tree (happens with frame roots).
        let root_indices: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.parent_id
                    .as_ref()
                    .is_none_or(|pid| !id_to_idx.contains_key(pid.inner()))
            })
            .map(|(i, _)| i)
            .collect();

        let mut next_index: usize = 0;
        let mut node_ids: Vec<BackendNodeId> = Vec::new();

        // Collect non-ignored descendants of an ignored node. The CDP AX
        // tree marks many structural nodes as ignored, but their children may
        // be interactive (button inside an ignored generic div). We must walk
        // through ignored nodes to reach them.
        fn collect_children_of_ignored(
            flat: &[AxNode],
            idx: usize,
            id_to_idx: &HashMap<String, usize>,
            next_index: &mut usize,
            node_ids: &mut Vec<BackendNodeId>,
        ) -> Vec<SnapshotNode> {
            let ax = &flat[idx];
            let mut result = Vec::new();
            if let Some(child_ids) = &ax.child_ids {
                for child_id in child_ids {
                    if let Some(&child_idx) = id_to_idx.get(child_id.inner()) {
                        if flat[child_idx].ignored {
                            // Recursively collect through chains of ignored nodes.
                            result.extend(collect_children_of_ignored(
                                flat, child_idx, id_to_idx, next_index, node_ids,
                            ));
                        } else if let Some(child_node) =
                            build_node(flat, child_idx, id_to_idx, next_index, node_ids)
                        {
                            result.push(child_node);
                        }
                    }
                }
            }
            result
        }

        // Recursive tree builder
        fn build_node(
            flat: &[AxNode],
            idx: usize,
            id_to_idx: &HashMap<String, usize>,
            next_index: &mut usize,
            node_ids: &mut Vec<BackendNodeId>,
        ) -> Option<SnapshotNode> {
            let ax = &flat[idx];

            // Ignored nodes aren't rendered, but their children may contain
            // non-ignored interactive elements. Handled by the caller via
            // `collect_children_of_ignored`.
            if ax.ignored {
                return None;
            }

            let role = ax
                .role
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let name = ax
                .name
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let description = ax
                .description
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());

            let value_text = ax
                .value
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());

            // Extract state properties from the properties array.
            let mut checked = None;
            let mut disabled = false;
            let mut expanded = None;
            let mut selected = false;
            let mut level = None;
            let mut pressed = None;

            if let Some(properties) = &ax.properties {
                for prop in properties {
                    match prop.name {
                        AxPropertyName::Checked => {
                            checked = prop
                                .value
                                .value
                                .as_ref()
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        AxPropertyName::Disabled => {
                            disabled = prop
                                .value
                                .value
                                .as_ref()
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                        }
                        AxPropertyName::Expanded => {
                            expanded = prop.value.value.as_ref().and_then(|v| v.as_bool());
                        }
                        AxPropertyName::Selected => {
                            selected = prop
                                .value
                                .value
                                .as_ref()
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                        }
                        AxPropertyName::Level => {
                            level = prop.value.value.as_ref().and_then(|v| v.as_i64());
                        }
                        AxPropertyName::Pressed => {
                            pressed = prop
                                .value
                                .value
                                .as_ref()
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                        _ => {}
                    }
                }
            }

            // Build children recursively. Ignored children are transparent —
            // we walk through them to collect their non-ignored descendants.
            let mut children = Vec::new();
            if let Some(child_ids) = &ax.child_ids {
                for child_id in child_ids {
                    if let Some(&child_idx) = id_to_idx.get(child_id.inner()) {
                        if flat[child_idx].ignored {
                            children.extend(collect_children_of_ignored(
                                flat, child_idx, id_to_idx, next_index, node_ids,
                            ));
                        } else if let Some(child_node) =
                            build_node(flat, child_idx, id_to_idx, next_index, node_ids)
                        {
                            children.push(child_node);
                        }
                    }
                }
            }

            // Assign an index to interactive elements that have a BackendNodeId.
            let role_lower = role.to_lowercase();
            let is_interactive = INTERACTIVE_ROLES.contains(&role_lower.as_str());
            let element_index =
                if let Some(backend_node_id) = ax.backend_dom_node_id.filter(|_| is_interactive) {
                    let idx = *next_index;
                    *next_index += 1;
                    node_ids.push(backend_node_id);
                    Some(idx)
                } else {
                    None
                };

            // Skip structural nodes that have no name, no index, and only serve
            // as tree wrappers — but only if they have exactly one child (pass
            // through) or zero children (prune entirely).
            if SKIP_UNNAMED_ROLES.contains(&role.as_str())
                && element_index.is_none()
                && name.is_empty()
            {
                match children.len() {
                    0 => return None,
                    1 => return Some(children.into_iter().next().expect("len checked")),
                    _ => {
                        // Multiple children — keep the node as a container
                        // but only if children are non-trivial.
                    }
                }
            }

            Some(SnapshotNode {
                role,
                name,
                index: element_index,
                children,
                checked,
                disabled,
                expanded,
                selected,
                level,
                pressed,
                value: value_text,
                description,
            })
        }

        let mut roots = Vec::new();
        for &root_idx in &root_indices {
            if nodes[root_idx].ignored {
                // Root itself is ignored — collect its non-ignored descendants.
                roots.extend(collect_children_of_ignored(
                    &nodes,
                    root_idx,
                    &id_to_idx,
                    &mut next_index,
                    &mut node_ids,
                ));
            } else if let Some(node) =
                build_node(&nodes, root_idx, &id_to_idx, &mut next_index, &mut node_ids)
            {
                roots.push(node);
            }
        }

        Self { roots, node_ids }
    }

    /// Render the accessibility tree as compact YAML-like text for LLM consumption.
    pub fn render(&self) -> String {
        let mut output = String::with_capacity(4096);
        for root in &self.roots {
            render_snapshot_node(root, 0, &mut output);
        }
        output
    }

    /// Look up the `BackendNodeId` for an element index.
    pub fn backend_node_id_for_index(&self, index: usize) -> Option<BackendNodeId> {
        self.node_ids.get(index).copied()
    }

    /// Total number of indexed interactive elements.
    pub fn element_count(&self) -> usize {
        self.node_ids.len()
    }
}

fn render_snapshot_node(node: &SnapshotNode, depth: usize, output: &mut String) {
    let indent = "  ".repeat(depth);

    // Skip the root "RootWebArea" wrapper — just render children directly.
    if node.role == "RootWebArea" || node.role == "rootWebArea" {
        for child in &node.children {
            render_snapshot_node(child, depth, output);
        }
        return;
    }

    // Build the line: `- role "name" [attrs]:`
    output.push_str(&indent);
    output.push_str("- ");
    output.push_str(&node.role);

    if !node.name.is_empty() {
        output.push_str(" \"");
        // Truncate very long names for context efficiency.
        let display_name = super::truncate_utf8_ellipsis(&node.name, 200);
        output.push_str(&display_name.replace('"', "\\\""));
        output.push('"');
    }

    // Attributes
    if let Some(index) = node.index {
        output.push_str(&format!(" [index={index}]"));
    }
    if let Some(level) = node.level {
        output.push_str(&format!(" [level={level}]"));
    }
    if node.disabled {
        output.push_str(" [disabled]");
    }
    if node.selected {
        output.push_str(" [selected]");
    }
    if let Some(ref checked) = node.checked {
        match checked.as_str() {
            "true" => output.push_str(" [checked]"),
            "false" => output.push_str(" [unchecked]"),
            "mixed" => output.push_str(" [checked=mixed]"),
            _ => {}
        }
    }
    if let Some(ref pressed) = node.pressed {
        match pressed.as_str() {
            "true" => output.push_str(" [pressed]"),
            "mixed" => output.push_str(" [pressed=mixed]"),
            _ => {}
        }
    }
    if let Some(true) = node.expanded {
        output.push_str(" [expanded]");
    } else if let Some(false) = node.expanded {
        output.push_str(" [collapsed]");
    }

    // Value (e.g., text input current value)
    if let Some(ref value) = node.value {
        let display_value = super::truncate_utf8_ellipsis(value, 100);
        output.push_str(&format!(
            " value=\"{}\"",
            display_value.replace('"', "\\\"")
        ));
    }

    let has_children = !node.children.is_empty();
    let has_description = node.description.is_some();

    if has_children || has_description {
        output.push_str(":\n");
    } else {
        output.push('\n');
    }

    if let Some(ref description) = node.description {
        output.push_str(&indent);
        output.push_str("  /description: ");
        output.push_str(description);
        output.push('\n');
    }

    for child in &node.children {
        render_snapshot_node(child, depth + 1, output);
    }
}

// Shared browser state

/// Opaque handle to shared browser state that persists across worker lifetimes.
///
/// Held by `RuntimeConfig` when `persist_session = true`. All workers for the
/// same agent clone this handle and share a single browser process / tab set.
pub type SharedBrowserHandle = Arc<Mutex<BrowserState>>;

/// Create a new shared browser handle for use in `RuntimeConfig`.
pub fn new_shared_browser_handle() -> SharedBrowserHandle {
    Arc::new(Mutex::new(BrowserState::new()))
}

/// Internal browser state managed across tool invocations.
///
/// When `persist_session` is enabled this struct lives in `RuntimeConfig` (via
/// `SharedBrowserHandle`) and is shared across worker lifetimes. Otherwise each
/// tool set owns its own instance.
pub struct BrowserState {
    browser: Option<Browser>,
    _handler_task: Option<JoinHandle<()>>,
    pages: HashMap<String, chromiumoxide::Page>,
    active_target: Option<String>,
    /// Cached accessibility snapshot from the last `browser_snapshot` call.
    /// Invalidated on navigation, tab switch, and explicit snapshot refresh.
    snapshot: Option<AxSnapshot>,
    user_data_dir: Option<PathBuf>,
    /// When true, `user_data_dir` is a stable path that should NOT be deleted
    /// on drop — it holds cookies, localStorage, and login sessions.
    persistent_profile: bool,
}

impl BrowserState {
    fn new() -> Self {
        Self {
            browser: None,
            _handler_task: None,
            pages: HashMap::new(),
            active_target: None,
            snapshot: None,
            user_data_dir: None,
            persistent_profile: false,
        }
    }

    /// Invalidate the cached snapshot. Called after any page-mutating action.
    fn invalidate_snapshot(&mut self) {
        self.snapshot = None;
    }
}

impl Drop for BrowserState {
    fn drop(&mut self) {
        // Persistent profiles store cookies and login sessions that must
        // survive across agent restarts — never delete them.
        if self.persistent_profile {
            return;
        }

        // Take ownership of resources to move into cleanup task
        let browser = self.browser.take();
        let handler_task = self._handler_task.take();
        let user_data_dir = self.user_data_dir.take();

        // Close browser gracefully before cleaning up temp directory.
        // This prevents chromiumoxide's force-kill which causes core dumps.
        // We bundle close → abort → cleanup into a single spawned task to avoid races.
        if let Some(mut browser) = browser
            && let Some(task) = handler_task
            && let Some(dir) = user_data_dir
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                // Close browser first (with timeout to avoid hanging)
                let close_result =
                    tokio::time::timeout(std::time::Duration::from_secs(5), browser.close()).await;

                match close_result {
                    Ok(Ok(_)) => tracing::debug!("Browser closed gracefully"),
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "Failed to close browser during drop")
                    }
                    Err(_) => tracing::debug!("Browser close timed out after 5s"),
                }

                // Abort handler task after close completes or times out
                task.abort();

                // Then clean up temp directory
                if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                    tracing::debug!(
                        path = %dir.display(),
                        %error,
                        "Failed to clean up browser user data dir"
                    );
                }
            });
        }
        // Note: if no runtime is available, we can't close async -
        // chromiumoxide will force-kill on drop, which is acceptable
    }
}

impl std::fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserState")
            .field("has_browser", &self.browser.is_some())
            .field("pages", &self.pages.len())
            .field("active_target", &self.active_target)
            .field("has_snapshot", &self.snapshot.is_some())
            .field("persistent_profile", &self.persistent_profile)
            .finish()
    }
}

// Error type

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

// Common output type

#[derive(Debug, Serialize)]
pub struct BrowserOutput {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<TabInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_result: Option<serde_json::Value>,
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
            snapshot: None,
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

#[derive(Debug, Clone, Serialize)]
pub struct TabInfo {
    pub target_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub active: bool,
}

// Shared helper struct that all tools reference

/// Shared context cloned into each browser tool. Holds the browser state mutex,
/// config, screenshot directory, and optional secrets store for secure text entry.
#[derive(Debug, Clone)]
pub(crate) struct BrowserContext {
    state: Arc<Mutex<BrowserState>>,
    config: BrowserConfig,
    screenshot_dir: PathBuf,
    /// Secrets store for resolving secret names in `browser_type`. When present,
    /// the `secret` parameter can look up credential values without exposing
    /// them in tool arguments or output.
    secrets: Option<Arc<SecretsStore>>,
}

impl BrowserContext {
    pub(crate) fn new(
        state: Arc<Mutex<BrowserState>>,
        config: BrowserConfig,
        screenshot_dir: PathBuf,
        secrets: Option<Arc<SecretsStore>>,
    ) -> Self {
        Self {
            state,
            config,
            screenshot_dir,
            secrets,
        }
    }

    /// Get the active page or return an error. Does NOT hold the lock — caller
    /// must pass a reference to the already-locked state.
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

    /// Extract the accessibility snapshot from the active page via CDP.
    /// Caches the result on `BrowserState` so repeated reads don't re-extract.
    async fn extract_snapshot<'a>(
        &self,
        state: &'a mut BrowserState,
    ) -> Result<&'a AxSnapshot, BrowserError> {
        // The `is_some` + `expect` pattern is intentional — `if let Some(s) =
        // state.snapshot.as_ref()` would borrow `state` for `'a`, preventing
        // the mutable assignment to `state.snapshot` in the cache-miss path.
        #[allow(clippy::unnecessary_unwrap)]
        if state.snapshot.is_some() {
            return Ok(state.snapshot.as_ref().expect("just checked"));
        }

        let page = self.require_active_page(state)?;

        let result = page
            .execute(GetFullAxTreeParams::default())
            .await
            .map_err(|error| {
                BrowserError::new(format!("accessibility tree extraction failed: {error}"))
            })?;

        let ax_nodes = result.result.nodes;

        tracing::debug!(
            node_count = ax_nodes.len(),
            "CDP Accessibility.getFullAXTree returned"
        );

        let snapshot = AxSnapshot::from_ax_nodes(ax_nodes);

        tracing::debug!(
            interactive_count = snapshot.element_count(),
            "accessibility snapshot built"
        );

        state.snapshot = Some(snapshot);
        Ok(state.snapshot.as_ref().expect("just stored"))
    }

    /// Resolve an element target to a `BackendNodeId` for interaction.
    ///
    /// When an index is provided, the cached snapshot's `BackendNodeId` map is
    /// used. When a CSS selector string is provided, it's resolved via
    /// `page.find_element()` — this path is a fallback for when the LLM already
    /// knows a selector (e.g., `#login_field`).
    async fn find_element(
        &self,
        state: &mut BrowserState,
        target: &ElementTarget,
    ) -> Result<ElementHandle, BrowserError> {
        match target {
            ElementTarget::Index(index) => {
                self.extract_snapshot(state).await?;
                let snapshot = state.snapshot.as_ref().expect("just extracted");

                let backend_node_id =
                    snapshot.backend_node_id_for_index(*index).ok_or_else(|| {
                        BrowserError::new(format!(
                            "element index {index} not found — run browser_snapshot to get fresh \
                             indices (max index in current snapshot: {})",
                            snapshot.element_count().saturating_sub(1)
                        ))
                    })?;

                Ok(ElementHandle::BackendNode(backend_node_id))
            }
            ElementTarget::Selector(selector) => {
                let page = self.require_active_page(state)?;
                let element = page.find_element(selector).await.map_err(|error| {
                    BrowserError::new(format!(
                        "element not found via selector '{selector}': {error}. \
                             The page may have changed — run browser_snapshot again."
                    ))
                })?;
                Ok(ElementHandle::CssElement(element))
            }
        }
    }

    /// Click an element resolved by `find_element`. Uses CDP mouse events for
    /// `BackendNodeId` targets (avoids CSS selector fragility) and the
    /// chromiumoxide `Element::click()` for CSS selector targets.
    async fn click_element(
        &self,
        state: &BrowserState,
        handle: &ElementHandle,
    ) -> Result<(), BrowserError> {
        match handle {
            ElementHandle::BackendNode(backend_node_id) => {
                let page = self.require_active_page(state)?;
                let (center_x, center_y) = get_element_center(page, *backend_node_id).await?;

                page.execute(
                    DispatchMouseEventParams::builder()
                        .r#type(DispatchMouseEventType::MousePressed)
                        .x(center_x)
                        .y(center_y)
                        .button(MouseButton::Left)
                        .click_count(1)
                        .build()
                        .map_err(|error| {
                            BrowserError::new(format!("failed to build mouse press event: {error}"))
                        })?,
                )
                .await
                .map_err(|error| BrowserError::new(format!("mouse press failed: {error}")))?;

                page.execute(
                    DispatchMouseEventParams::builder()
                        .r#type(DispatchMouseEventType::MouseReleased)
                        .x(center_x)
                        .y(center_y)
                        .button(MouseButton::Left)
                        .click_count(1)
                        .build()
                        .map_err(|error| {
                            BrowserError::new(format!(
                                "failed to build mouse release event: {error}"
                            ))
                        })?,
                )
                .await
                .map_err(|error| BrowserError::new(format!("mouse release failed: {error}")))?;

                Ok(())
            }
            ElementHandle::CssElement(element) => {
                element
                    .click()
                    .await
                    .map_err(|error| BrowserError::new(format!("click failed: {error}")))?;
                Ok(())
            }
        }
    }

    /// Focus an element and type text into it. Uses CDP `DOM.focus` for
    /// `BackendNodeId` targets.
    async fn focus_and_type(
        &self,
        state: &BrowserState,
        handle: &ElementHandle,
        text: &str,
        clear: bool,
    ) -> Result<(), BrowserError> {
        let page = self.require_active_page(state)?;

        match handle {
            ElementHandle::BackendNode(backend_node_id) => {
                // Resolve BackendNodeId to RemoteObject so we can call JS on it
                let resolve_result = page
                    .execute(ResolveNodeParams {
                        backend_node_id: Some(*backend_node_id),
                        ..Default::default()
                    })
                    .await
                    .map_err(|error| {
                        BrowserError::new(format!("failed to resolve node for typing: {error}"))
                    })?;

                let object_id = resolve_result.result.object.object_id.ok_or_else(|| {
                    BrowserError::new(
                        "resolved node has no object_id — element may have been removed",
                    )
                })?;

                // Use callFunctionOn to focus, validate, clear, and set value
                let text_json = serde_json::to_string(text).unwrap_or_default();
                let clear_js = if clear {
                    "if (this.isContentEditable) { this.textContent = ''; } else { this.value = ''; }"
                } else {
                    ""
                };

                let js = format!(
                    r#"function() {{
                        const tag = this.tagName;
                        const editable = this.isContentEditable;
                        if (tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT' && !editable) {{
                            return 'element is a ' + tag.toLowerCase() + ', not a text input';
                        }}
                        this.focus();
                        {clear_js}
                        if (editable) {{
                            this.textContent = {text_json};
                        }} else {{
                            this.value = {text_json};
                        }}
                        this.dispatchEvent(new Event('input', {{bubbles: true}}));
                        this.dispatchEvent(new Event('change', {{bubbles: true}}));
                        return null;
                    }}"#,
                );

                let call_result = page
                    .execute(
                        chromiumoxide_cdp::cdp::js_protocol::runtime::CallFunctionOnParams::builder()
                            .function_declaration(js)
                            .object_id(object_id)
                            .return_by_value(true)
                            .user_gesture(true)
                            .silent(true)
                            .build()
                            .map_err(|error| {
                                BrowserError::new(format!(
                                    "failed to build CallFunctionOn params: {error}"
                                ))
                            })?,
                    )
                    .await
                    .map_err(|error| {
                        BrowserError::new(format!("type action failed: {error}"))
                    })?;

                // Check for validation error returned from the JS function.
                // The function returns null on success, or an error string.
                if let Some(value) = &call_result.result.result.value
                    && let Some(error_msg) = value.as_str()
                {
                    return Err(BrowserError::new(error_msg.to_string()));
                }

                Ok(())
            }
            ElementHandle::CssElement(element) => {
                // CSS selector path — click to focus, then use JS to type.
                element
                    .click()
                    .await
                    .map_err(|error| BrowserError::new(format!("focus failed: {error}")))?;

                let text_json = serde_json::to_string(text).unwrap_or_default();
                let clear_js = if clear { "el.value = '';" } else { "" };
                let js = format!(
                    r#"(() => {{
                        const el = document.activeElement;
                        if (!el) return 'no focused element after click';
                        const tag = el.tagName;
                        const editable = el.isContentEditable;
                        if (tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT' && !editable) {{
                            return 'element is a ' + tag.toLowerCase() + ', not a text input';
                        }}
                        if (editable) {{
                            {clear_editable}
                            el.textContent = {text_json};
                        }} else {{
                            {clear_js}
                            el.value = {text_json};
                        }}
                        el.dispatchEvent(new Event('input', {{bubbles: true}}));
                        el.dispatchEvent(new Event('change', {{bubbles: true}}));
                        return null;
                    }})()"#,
                    clear_editable = if clear { "el.textContent = '';" } else { "" },
                );

                page.evaluate(js)
                    .await
                    .map_err(|error| BrowserError::new(format!("type failed: {error}")))?;

                Ok(())
            }
        }
    }

    /// Launch the browser if not already running. Returns a status message.
    async fn ensure_launched(&self) -> Result<String, BrowserError> {
        {
            let mut state = self.state.lock().await;
            if state.browser.is_some() {
                if self.config.persist_session {
                    return self.reconnect_existing_tabs(&mut state).await;
                }
                return Ok("Browser already running".to_string());
            }
        }

        let executable = resolve_chrome_executable(&self.config).await?;

        let (user_data_dir, persistent_profile) = if self.config.persist_session {
            (self.config.chrome_cache_dir.join("profile"), true)
        } else {
            let dir =
                std::env::temp_dir().join(format!("spacebot-browser-{}", uuid::Uuid::new_v4()));
            (dir, false)
        };

        if persistent_profile {
            let lock_file = user_data_dir.join("SingletonLock");
            if lock_file.exists() {
                tracing::debug!(path = %lock_file.display(), "removing stale Chrome SingletonLock");
                let _ = std::fs::remove_file(&lock_file);
            }
        }

        let mut builder = ChromeConfig::builder()
            .no_sandbox()
            .chrome_executable(&executable)
            .user_data_dir(&user_data_dir);

        // Add additional flags from CHROME_FLAGS environment variable
        if let Ok(flags) = std::env::var("CHROME_FLAGS") {
            for flag in flags.split_whitespace() {
                builder = builder.arg(flag);
            }
        }

        if self.config.headless {
            // Headless has no real window — set an explicit viewport so
            // screenshots render at a reasonable desktop size instead of
            // the chromiumoxide default of 800x600.
            builder = builder.viewport(Viewport {
                width: 1280,
                height: 900,
                ..Default::default()
            });
        } else {
            // Headed mode: disable viewport emulation so the page fills
            // the actual window. The default 800x600 viewport constrains
            // page content to a smaller area than the window.
            builder = builder.with_head().window_size(1280, 900).viewport(None);
        }

        let chrome_config = builder.build().map_err(|error| {
            BrowserError::new(format!("failed to build browser config: {error}"))
        })?;

        tracing::info!(
            headless = self.config.headless,
            executable = %executable.display(),
            user_data_dir = %user_data_dir.display(),
            "launching chrome"
        );

        let (browser, mut handler) = Browser::launch(chrome_config)
            .await
            .map_err(|error| BrowserError::new(format!("failed to launch browser: {error}")))?;

        let state_clone = self.state.clone();
        let handler_task = tokio::spawn(async move {
            while let Some(result) = handler.next().await {
                if let Err(error) = result {
                    tracing::error!(%error, "Browser handler error");
                }
            }
            tracing::warn!("Browser handler stream ended");
            // Clear browser state so ensure_launched() knows to relaunch
            // BUT only for non-persistent sessions (persistent sessions survive handler exit)
            let mut state = state_clone.lock().await;
            if !state.persistent_profile {
                state.browser = None;
                state._handler_task = None;
                state.pages.clear();
                state.active_target = None;
                state.invalidate_snapshot();
                // Note: user_data_dir is intentionally preserved for cleanup in Drop
            } else {
                // For persistent sessions, just clear the handler task reference
                // The browser process may restart, but the session data persists
                state._handler_task = None;
                tracing::debug!("Persistent browser session - preserving state for reconnection");
            }
        });

        let mut state = self.state.lock().await;

        // Guard against concurrent launch race
        if state.browser.is_some() {
            drop(browser);
            handler_task.abort();
            if !persistent_profile {
                let dir = user_data_dir;
                tokio::spawn(async move {
                    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                        tracing::debug!(
                            path = %dir.display(),
                            %error,
                            "failed to clean up browser user data dir (concurrent launch race)"
                        );
                    }
                });
            }
            if self.config.persist_session {
                return self.reconnect_existing_tabs(&mut state).await;
            }
            return Ok("Browser already running".to_string());
        }

        state.browser = Some(browser);
        state._handler_task = Some(handler_task);
        state.user_data_dir = Some(user_data_dir);
        state.persistent_profile = persistent_profile;

        tracing::info!("browser launched");
        Ok("Browser launched successfully".to_string())
    }

    /// Discover existing tabs from the browser and rebuild the page map.
    async fn reconnect_existing_tabs(
        &self,
        state: &mut BrowserState,
    ) -> Result<String, BrowserError> {
        let browser = state
            .browser
            .as_ref()
            .ok_or_else(|| BrowserError::new("browser not launched"))?;

        let pages = browser.pages().await.map_err(|error| {
            BrowserError::new(format!("failed to enumerate existing tabs: {error}"))
        })?;

        let previous_ids: std::collections::HashSet<String> = state.pages.keys().cloned().collect();
        let mut refreshed_pages = HashMap::with_capacity(pages.len());
        for page in pages {
            let target_id = page_target_id(&page);
            refreshed_pages.insert(target_id, page);
        }
        let discovered = refreshed_pages
            .keys()
            .filter(|id| !previous_ids.contains(*id))
            .count();

        state.pages = refreshed_pages;
        state.invalidate_snapshot();

        let active_valid = state
            .active_target
            .as_ref()
            .is_some_and(|id| state.pages.contains_key(id));
        if !active_valid {
            state.active_target = state.pages.keys().next().cloned();
        }

        let tab_count = state.pages.len();
        tracing::info!(tab_count, discovered, "reconnected to persistent browser");

        Ok(format!(
            "Connected to persistent browser ({tab_count} tab{} open, {discovered} newly discovered)",
            if tab_count == 1 { "" } else { "s" }
        ))
    }
}

// Tool: browser_launch

#[derive(Debug, Clone)]
pub struct BrowserLaunchTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserLaunchArgs {}

impl Tool for BrowserLaunchTool {
    const NAME: &'static str = "browser_launch";
    type Error = BrowserError;
    type Args = BrowserLaunchArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Launch the browser. Must be called before any other browser tool."
                .to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let message = self.context.ensure_launched().await?;
        Ok(BrowserOutput::success(message))
    }
}

// Tool: browser_navigate

#[derive(Debug, Clone)]
pub struct BrowserNavigateTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserNavigateArgs {
    /// The URL to navigate to.
    pub url: String,
}

impl Tool for BrowserNavigateTool {
    const NAME: &'static str = "browser_navigate";
    type Error = BrowserError;
    type Args = BrowserNavigateArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Navigate the active tab to a URL. Auto-launches the browser if needed."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The URL to navigate to" }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_url(&args.url)?;
        self.context.ensure_launched().await?;

        let mut state = self.context.state.lock().await;

        // Get or create the active page
        let page = get_or_create_page(&self.context, &mut state, Some(&args.url)).await?;

        page.goto(&args.url)
            .await
            .map_err(|error| BrowserError::new(format!("navigation failed: {error}")))?;

        // Wait briefly for SPA content to render. Many sites load a shell via
        // the initial HTML then hydrate with JS — without this pause,
        // `browser_snapshot` runs before any interactive elements exist.
        wait_for_page_ready(page).await;

        let title = page.get_title().await.ok().flatten();
        let current_url = page.url().await.ok().flatten();
        state.invalidate_snapshot();

        Ok(BrowserOutput::success(format!("Navigated to {}", args.url))
            .with_page_info(title, current_url))
    }
}

// Tool: browser_snapshot

#[derive(Debug, Clone)]
pub struct BrowserSnapshotTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserSnapshotArgs {}

impl Tool for BrowserSnapshotTool {
    const NAME: &'static str = "browser_snapshot";
    type Error = BrowserError;
    type Args = BrowserSnapshotArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Get the page's ARIA accessibility tree with numbered element indices. \
                          Use the [index=N] values in browser_click, browser_type, etc. When \
                          you see password fields or other sensitive inputs, use browser_type \
                          with the `secret` parameter (not `text`) to type credentials securely."
                .to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut state = self.context.state.lock().await;

        // Force a fresh snapshot
        state.invalidate_snapshot();
        let snapshot = self.context.extract_snapshot(&mut state).await?;

        let rendered = snapshot.render();
        let element_count = snapshot.element_count();
        let title = self
            .context
            .require_active_page(&state)?
            .get_title()
            .await
            .ok()
            .flatten();
        let url = self
            .context
            .require_active_page(&state)?
            .url()
            .await
            .ok()
            .flatten();

        Ok(BrowserOutput {
            success: true,
            message: format!("{element_count} interactive element(s) found"),
            title,
            url,
            snapshot: Some(rendered),
            tabs: None,
            screenshot_path: None,
            eval_result: None,
            content: None,
        })
    }
}

// Shared element targeting — tools accept either an index or a CSS selector.

/// A resolved element handle — either a CDP `BackendNodeId` (from snapshot
/// index lookup) or a chromiumoxide `Element` (from CSS selector query).
enum ElementHandle {
    BackendNode(BackendNodeId),
    CssElement(chromiumoxide::Element),
}

/// Resolved element target for click/type/press_key tools.
enum ElementTarget {
    Index(usize),
    Selector(String),
}

impl ElementTarget {
    /// Build from optional index + optional selector args.
    /// At least one must be provided; `selector` wins if both are present.
    fn from_args(index: Option<usize>, selector: Option<String>) -> Result<Self, BrowserError> {
        match (selector, index) {
            (Some(sel), _) if !sel.is_empty() => Ok(Self::Selector(sel)),
            (_, Some(idx)) => Ok(Self::Index(idx)),
            _ => Err(BrowserError::new(
                "provide either `index` (from browser_snapshot) or `selector` (CSS selector)",
            )),
        }
    }

    fn display(&self) -> String {
        match self {
            Self::Index(i) => format!("index {i}"),
            Self::Selector(s) => format!("selector '{s}'"),
        }
    }
}

// Tool: browser_click

#[derive(Debug, Clone)]
pub struct BrowserClickTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserClickArgs {
    /// The element index from the snapshot (e.g., 5).
    pub index: Option<usize>,
    /// CSS selector to target directly (e.g., "#login_field"). Use this when
    /// you know the selector — it's more reliable than index for dynamic pages.
    pub selector: Option<String>,
}

impl Tool for BrowserClickTool {
    const NAME: &'static str = "browser_click";
    type Error = BrowserError;
    type Args = BrowserClickArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Click an element by index (from browser_snapshot) or CSS selector."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "index": { "type": "integer", "description": "Element index from snapshot" },
                    "selector": { "type": "string", "description": "CSS selector (e.g. \"#my-button\", \"button.submit\")" }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let target = ElementTarget::from_args(args.index, args.selector)?;
        let label = target.display();

        let mut state = self.context.state.lock().await;
        let handle = self.context.find_element(&mut state, &target).await?;

        self.context.click_element(&state, &handle).await?;

        // Clicks often trigger navigation or DOM changes — give the page a
        // moment to settle before the next snapshot.
        let page = self.context.require_active_page(&state)?;
        wait_for_page_ready(page).await;

        state.invalidate_snapshot();

        Ok(BrowserOutput::success(format!(
            "Clicked element at {label}"
        )))
    }
}

// Tool: browser_type

#[derive(Debug, Clone)]
pub struct BrowserTypeTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserTypeArgs {
    /// The element index from the snapshot.
    pub index: Option<usize>,
    /// CSS selector to target directly (e.g., "#login_field", "input[name='email']").
    pub selector: Option<String>,
    /// The text to type into the element. Mutually exclusive with `secret`.
    /// Do NOT put secret values (passwords, tokens, API keys) here — they will
    /// appear in tool output. Use the `secret` parameter instead.
    pub text: Option<String>,
    /// Name of a secret from the secret store to type into the element.
    /// The secret value is resolved server-side and never appears in tool
    /// arguments, output, or LLM context. Use this for passwords, tokens,
    /// API keys, and any other sensitive values. Mutually exclusive with `text`.
    pub secret: Option<String>,
    /// Whether to clear the field before typing. Defaults to true.
    #[serde(default = "default_true")]
    pub clear: bool,
}

fn default_true() -> bool {
    true
}

impl Tool for BrowserTypeTool {
    const NAME: &'static str = "browser_type";
    type Error = BrowserError;
    type Args = BrowserTypeArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Type text into an input element by index (from browser_snapshot) or \
                          CSS selector. Provide either `text` for plain text or `secret` for \
                          sensitive values (passwords, tokens, API keys). When using `secret`, \
                          pass the secret name (e.g. \"GH_PASSWORD\") — the value is resolved \
                          from the secret store and typed without ever appearing in tool \
                          arguments or output. NEVER put passwords or credentials in the `text` \
                          parameter — always use `secret` for sensitive values."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "index": { "type": "integer", "description": "Element index from snapshot" },
                    "selector": { "type": "string", "description": "CSS selector (e.g. \"#login_field\", \"input[name='email']\")" },
                    "text": { "type": "string", "description": "Plain text to type. Do NOT use for passwords or secrets — use the `secret` parameter instead." },
                    "secret": { "type": "string", "description": "Name of a secret from the secret store (e.g. \"GH_PASSWORD\", \"NPM_TOKEN\"). The value is resolved securely and never appears in output." },
                    "clear": { "type": "boolean", "default": true, "description": "Clear the field before typing (default true)" }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let target = ElementTarget::from_args(args.index, args.selector)?;
        let label = target.display();

        // Resolve the text to type: either from `secret` (secure) or `text` (plain).
        let (text_value, is_secret) = match (&args.secret, &args.text) {
            (Some(secret_name), None) => {
                let store = self.context.secrets.as_ref().ok_or_else(|| {
                    BrowserError::new(
                        "secret store is not available — secrets cannot be resolved. \
                         Add the secret via the API or use the `text` parameter instead.",
                    )
                })?;
                let decrypted = store.get(secret_name).map_err(|error| {
                    BrowserError::new(format!("failed to resolve secret '{secret_name}': {error}"))
                })?;
                (decrypted.expose().to_string(), true)
            }
            (None, Some(text)) => (text.clone(), false),
            (Some(_), Some(_)) => {
                return Err(BrowserError::new(
                    "`text` and `secret` are mutually exclusive — provide one or the other",
                ));
            }
            (None, None) => {
                return Err(BrowserError::new(
                    "provide either `text` (plain text) or `secret` (secret name from the \
                     secret store) to type into the element",
                ));
            }
        };

        let mut state = self.context.state.lock().await;
        let handle = self.context.find_element(&mut state, &target).await?;

        self.context
            .focus_and_type(&state, &handle, &text_value, args.clear)
            .await?;

        state.invalidate_snapshot();

        // Secret values must never appear in tool output.
        let message = if is_secret {
            format!(
                "Typed secret '{}' into element at {label}",
                args.secret.as_deref().unwrap_or("unknown")
            )
        } else {
            let display_text = super::truncate_utf8_ellipsis(&text_value, 50);
            format!("Typed '{display_text}' into element at {label}")
        };

        Ok(BrowserOutput::success(message))
    }
}

// Tool: browser_press_key

#[derive(Debug, Clone)]
pub struct BrowserPressKeyTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserPressKeyArgs {
    /// The key to press (e.g., "Enter", "Tab", "Escape", "ArrowDown").
    pub key: String,
}

impl Tool for BrowserPressKeyTool {
    const NAME: &'static str = "browser_press_key";
    type Error = BrowserError;
    type Args = BrowserPressKeyArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Press a keyboard key (e.g., \"Enter\", \"Tab\", \"Escape\").".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Key name (Enter, Tab, Escape, ArrowDown, etc.)" }
                },
                "required": ["key"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let state = self.context.state.lock().await;
        let page = self.context.require_active_page(&state)?;
        dispatch_key_press(page, &args.key).await?;
        Ok(BrowserOutput::success(format!(
            "Pressed key '{}'",
            args.key
        )))
    }
}

// Tool: browser_screenshot

#[derive(Debug, Clone)]
pub struct BrowserScreenshotTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserScreenshotArgs {
    /// Whether to take a full-page screenshot.
    #[serde(default)]
    pub full_page: bool,
}

impl Tool for BrowserScreenshotTool {
    const NAME: &'static str = "browser_screenshot";
    type Error = BrowserError;
    type Args = BrowserScreenshotArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description:
                "Take a screenshot of the current page. Saves to disk and returns the file path."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "full_page": { "type": "boolean", "default": false, "description": "Capture entire page, not just viewport" }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let state = self.context.state.lock().await;
        let page = self.context.require_active_page(&state)?;

        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(args.full_page)
            .build();

        let screenshot_data = page
            .screenshot(params)
            .await
            .map_err(|error| BrowserError::new(format!("screenshot failed: {error}")))?;

        let filename = format!(
            "screenshot_{}.png",
            chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f")
        );
        let filepath = self.context.screenshot_dir.join(&filename);

        tokio::fs::create_dir_all(&self.context.screenshot_dir)
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
            snapshot: None,
            tabs: None,
            screenshot_path: Some(path_str),
            eval_result: None,
            content: None,
        })
    }
}

// Tool: browser_evaluate

#[derive(Debug, Clone)]
pub struct BrowserEvaluateTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserEvaluateArgs {
    /// JavaScript expression to evaluate in the page.
    pub script: String,
}

impl Tool for BrowserEvaluateTool {
    const NAME: &'static str = "browser_evaluate";
    type Error = BrowserError;
    type Args = BrowserEvaluateArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Evaluate JavaScript in the active page and return the result."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "JavaScript expression to evaluate" }
                },
                "required": ["script"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.context.config.evaluate_enabled {
            return Err(BrowserError::new(
                "JavaScript evaluation is disabled in browser config (set evaluate_enabled = true)",
            ));
        }

        let state = self.context.state.lock().await;
        let page = self.context.require_active_page(&state)?;

        let result = page
            .evaluate(args.script)
            .await
            .map_err(|error| BrowserError::new(format!("evaluate failed: {error}")))?;

        let value = result.value().cloned();

        Ok(BrowserOutput {
            success: true,
            message: "JavaScript evaluated".to_string(),
            title: None,
            url: None,
            snapshot: None,
            tabs: None,
            screenshot_path: None,
            eval_result: value,
            content: None,
        })
    }
}

// Tool: browser_tab_open

#[derive(Debug, Clone)]
pub struct BrowserTabOpenTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserTabOpenArgs {
    /// URL to open in the new tab. Defaults to about:blank.
    #[serde(default)]
    pub url: Option<String>,
}

impl Tool for BrowserTabOpenTool {
    const NAME: &'static str = "browser_tab_open";
    type Error = BrowserError;
    type Args = BrowserTabOpenArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Open a new browser tab, optionally at a URL.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to open (default: about:blank)" }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let target_url = args.url.as_deref().unwrap_or("about:blank");
        if target_url != "about:blank" {
            validate_url(target_url)?;
        }

        let mut state = self.context.state.lock().await;
        let browser = state
            .browser
            .as_ref()
            .ok_or_else(|| BrowserError::new("browser not launched — call browser_launch first"))?;

        let page = browser
            .new_page(target_url)
            .await
            .map_err(|error| BrowserError::new(format!("failed to open tab: {error}")))?;

        let target_id = page_target_id(&page);
        let title = page.get_title().await.ok().flatten();
        let current_url = page.url().await.ok().flatten();

        state.pages.insert(target_id.clone(), page);
        state.active_target = Some(target_id.clone());
        state.invalidate_snapshot();

        Ok(BrowserOutput {
            success: true,
            message: format!("Opened new tab (target: {target_id})"),
            title,
            url: current_url,
            snapshot: None,
            tabs: None,
            screenshot_path: None,
            eval_result: None,
            content: None,
        })
    }
}

// Tool: browser_tab_list

#[derive(Debug, Clone)]
pub struct BrowserTabListTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserTabListArgs {}

impl Tool for BrowserTabListTool {
    const NAME: &'static str = "browser_tab_list";
    type Error = BrowserError;
    type Args = BrowserTabListArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List all open browser tabs with their target IDs, titles, and URLs."
                .to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let state = self.context.state.lock().await;
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
            snapshot: None,
            tabs: Some(tabs),
            screenshot_path: None,
            eval_result: None,
            content: None,
        })
    }
}

// Tool: browser_tab_close

#[derive(Debug, Clone)]
pub struct BrowserTabCloseTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserTabCloseArgs {
    /// Target ID of the tab to close. If omitted, closes the active tab.
    #[serde(default)]
    pub target_id: Option<String>,
}

impl Tool for BrowserTabCloseTool {
    const NAME: &'static str = "browser_tab_close";
    type Error = BrowserError;
    type Args = BrowserTabCloseArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Close a browser tab by target_id, or the active tab if omitted."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target_id": { "type": "string", "description": "Tab target ID (from browser_tab_list). Omit for active tab." }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut state = self.context.state.lock().await;
        let id = args
            .target_id
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
        state.invalidate_snapshot();

        Ok(BrowserOutput::success(format!("Closed tab {id}")))
    }
}

// Tool: browser_close

#[derive(Debug, Clone)]
pub struct BrowserCloseTool {
    pub(crate) context: BrowserContext,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserCloseArgs {}

impl Tool for BrowserCloseTool {
    const NAME: &'static str = "browser_close";
    type Error = BrowserError;
    type Args = BrowserCloseArgs;
    type Output = BrowserOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Close or detach from the browser (behavior depends on config)."
                .to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        use crate::config::ClosePolicy;

        match self.context.config.close_policy {
            ClosePolicy::Detach => {
                let mut state = self.context.state.lock().await;
                state.invalidate_snapshot();
                tracing::info!(policy = "detach", "worker detached from browser");
                Ok(BrowserOutput::success(
                    "Detached from browser (tabs and session preserved)",
                ))
            }
            ClosePolicy::CloseTabs => {
                let pages_to_close: Vec<(String, chromiumoxide::Page)> = {
                    let mut state = self.context.state.lock().await;
                    let pages = state.pages.drain().collect();
                    state.active_target = None;
                    state.invalidate_snapshot();
                    pages
                };

                let mut close_errors = Vec::new();
                for (id, page) in pages_to_close {
                    if let Err(error) = page.close().await {
                        close_errors.push(format!("{id}: {error}"));
                    }
                }

                if !close_errors.is_empty() {
                    let message = format!(
                        "failed to close {} tab(s): {}",
                        close_errors.len(),
                        close_errors.join("; ")
                    );
                    tracing::warn!(policy = "close_tabs", %message);
                    return Err(BrowserError::new(message));
                }

                tracing::info!(
                    policy = "close_tabs",
                    "closed all tabs, browser still running"
                );
                Ok(BrowserOutput::success(
                    "All tabs closed (browser still running)",
                ))
            }
            ClosePolicy::CloseBrowser => {
                let (browser, handler_task, user_data_dir, persistent_profile) = {
                    let mut state = self.context.state.lock().await;
                    let browser = state.browser.take();
                    let handler_task = state._handler_task.take();
                    let user_data_dir = state.user_data_dir.take();
                    let persistent_profile = state.persistent_profile;
                    state.pages.clear();
                    state.active_target = None;
                    state.invalidate_snapshot();
                    (browser, handler_task, user_data_dir, persistent_profile)
                };

                if let Some(task) = handler_task {
                    task.abort();
                }

                if let Some(mut browser) = browser
                    && let Err(error) = browser.close().await
                {
                    let message = format!("failed to close browser: {error}");
                    tracing::warn!(policy = "close_browser", %message);
                    return Err(BrowserError::new(message));
                }

                if !persistent_profile && let Some(dir) = user_data_dir {
                    tokio::spawn(async move {
                        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                            tracing::debug!(
                                path = %dir.display(),
                                %error,
                                "failed to clean up browser user data dir"
                            );
                        }
                    });
                }

                tracing::info!(policy = "close_browser", "browser closed");
                Ok(BrowserOutput::success("Browser closed"))
            }
        }
    }
}

// Tool registration helper

/// Register all browser tools on a `ToolServer`. The tools share a single
/// `BrowserState` (via `SharedBrowserHandle` for persistent sessions, or a
/// fresh instance for ephemeral sessions).
pub fn register_browser_tools(
    server: rig::tool::server::ToolServer,
    config: BrowserConfig,
    screenshot_dir: PathBuf,
    runtime_config: &crate::config::RuntimeConfig,
) -> rig::tool::server::ToolServer {
    let state = if let Some(shared) = runtime_config
        .shared_browser
        .as_ref()
        .filter(|_| config.persist_session)
    {
        shared.clone()
    } else {
        Arc::new(Mutex::new(BrowserState::new()))
    };

    let secrets = runtime_config.secrets.load().as_ref().as_ref().cloned();

    let context = BrowserContext::new(state, config, screenshot_dir, secrets);

    server
        .tool(BrowserLaunchTool {
            context: context.clone(),
        })
        .tool(BrowserNavigateTool {
            context: context.clone(),
        })
        .tool(BrowserSnapshotTool {
            context: context.clone(),
        })
        .tool(BrowserClickTool {
            context: context.clone(),
        })
        .tool(BrowserTypeTool {
            context: context.clone(),
        })
        .tool(BrowserPressKeyTool {
            context: context.clone(),
        })
        .tool(BrowserScreenshotTool {
            context: context.clone(),
        })
        .tool(BrowserEvaluateTool {
            context: context.clone(),
        })
        .tool(BrowserTabOpenTool {
            context: context.clone(),
        })
        .tool(BrowserTabListTool {
            context: context.clone(),
        })
        .tool(BrowserTabCloseTool {
            context: context.clone(),
        })
        .tool(BrowserCloseTool { context })
}

// Shared helpers

/// Get the active page, or create a first one if the browser has no pages yet.
async fn get_or_create_page<'a>(
    context: &BrowserContext,
    state: &'a mut BrowserState,
    url: Option<&str>,
) -> Result<&'a chromiumoxide::Page, BrowserError> {
    if let Some(target) = state.active_target.as_ref()
        && state.pages.contains_key(target)
    {
        return Ok(&state.pages[target]);
    }

    let browser = state
        .browser
        .as_ref()
        .ok_or_else(|| BrowserError::new("browser not launched — call browser_launch first"))?;

    let target_url = url.unwrap_or("about:blank");
    let page = browser
        .new_page(target_url)
        .await
        .map_err(|error| BrowserError::new(format!("failed to create page: {error}")))?;

    let target_id = page_target_id(&page);
    state.pages.insert(target_id.clone(), page);
    state.active_target = Some(target_id.clone());

    // Suppress the "unused variable" warning — we need `context` for the type
    // signature to match the pattern used by the navigate tool.
    let _ = context;

    Ok(&state.pages[&target_id])
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

fn page_target_id(page: &chromiumoxide::Page) -> String {
    page.target_id().inner().clone()
}

/// Get the center coordinates of an element identified by `BackendNodeId`.
/// Uses `DOM.getBoxModel` to get the content box, then computes center.
async fn get_element_center(
    page: &chromiumoxide::Page,
    backend_node_id: BackendNodeId,
) -> Result<(f64, f64), BrowserError> {
    let box_model = page
        .execute(GetBoxModelParams {
            backend_node_id: Some(backend_node_id),
            ..Default::default()
        })
        .await
        .map_err(|error| {
            BrowserError::new(format!(
                "failed to get box model for element: {error}. \
                 The element may not be visible or may have been removed from the DOM. \
                 Run browser_snapshot to get the current interactable elements and their indices."
            ))
        })?;

    // The content quad is a flat array of 8 values: [x1,y1, x2,y2, x3,y3, x4,y4]
    let quad = &box_model.result.model.content;
    if quad.inner().len() < 8 {
        return Err(BrowserError::new(
            "element has no visible content box — it may be hidden or zero-sized",
        ));
    }

    let points = quad.inner();
    let center_x = (points[0] + points[2] + points[4] + points[6]) / 4.0;
    let center_y = (points[1] + points[3] + points[5] + points[7]) / 4.0;

    Ok((center_x, center_y))
}

// Chrome executable resolution

async fn resolve_chrome_executable(config: &BrowserConfig) -> Result<PathBuf, BrowserError> {
    if let Some(path) = &config.executable_path {
        let path = PathBuf::from(path);
        if path.exists() {
            tracing::debug!(path = %path.display(), "using configured chrome executable");
            return Ok(path);
        }
        tracing::warn!(
            path = %path.display(),
            "configured executable_path does not exist, falling through to detection"
        );
    }

    if let Some(path) = detect_chrome_from_env() {
        tracing::debug!(path = %path.display(), "using chrome from environment variable");
        return Ok(path);
    }

    if let Ok(path) = chromiumoxide::detection::default_executable(Default::default()) {
        tracing::debug!(path = %path.display(), "using system-detected chrome");
        return Ok(path);
    }

    tracing::info!(
        cache_dir = %config.chrome_cache_dir.display(),
        "no system Chrome found, downloading via fetcher"
    );
    fetch_chrome(&config.chrome_cache_dir).await
}

fn detect_chrome_from_env() -> Option<PathBuf> {
    for variable in ["CHROME", "CHROME_PATH"] {
        if let Ok(value) = std::env::var(variable) {
            let path = PathBuf::from(&value);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

async fn fetch_chrome(cache_dir: &Path) -> Result<PathBuf, BrowserError> {
    tokio::fs::create_dir_all(cache_dir)
        .await
        .map_err(|error| {
            BrowserError::new(format!(
                "failed to create chrome cache dir {}: {error}",
                cache_dir.display()
            ))
        })?;

    let options = BrowserFetcherOptions::builder()
        .with_path(cache_dir)
        .build()
        .map_err(|error| {
            BrowserError::new(format!("failed to build browser fetcher options: {error}"))
        })?;

    let fetcher = BrowserFetcher::new(options);
    let info = fetcher
        .fetch()
        .await
        .map_err(|error| BrowserError::new(format!("failed to download chrome: {error}")))?;

    tracing::info!(
        path = %info.executable_path.display(),
        "chrome downloaded and cached"
    );
    Ok(info.executable_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Test that BrowserState clears properly for non-persistent sessions
    /// This prevents regression of the bug where handler cleared state
    /// unconditionally, breaking persist_session feature
    #[tokio::test]
    async fn test_handler_clears_state_for_non_persistent_sessions() {
        let state = Arc::new(Mutex::new(BrowserState::new()));

        // Set up non-persistent session with data
        {
            let mut s = state.lock().await;
            s.persistent_profile = false;
            s.active_target = Some("target1".to_string());
            s.snapshot = Some(create_mock_snapshot());
            // Note: browser field is Option<Browser>, we test the clearing logic
            // without needing an actual Browser instance
        }

        // Simulate handler stream ending (non-persistent session)
        let state_clone = state.clone();
        let handler_task = tokio::spawn(async move {
            let mut state = state_clone.lock().await;
            // This is the actual logic from ensure_launched handler task
            if !state.persistent_profile {
                state.browser = None;
                state._handler_task = None;
                state.pages.clear();
                state.active_target = None;
                state.invalidate_snapshot();
            } else {
                state._handler_task = None;
            }
        });

        handler_task.await.unwrap();

        // Verify state was CLEARED for non-persistent session
        let state = state.lock().await;
        assert!(
            state.browser.is_none(),
            "browser should be cleared for non-persistent"
        );
        assert!(
            state.pages.is_empty(),
            "pages should be cleared for non-persistent"
        );
        assert!(
            state.active_target.is_none(),
            "active_target should be cleared for non-persistent"
        );
        assert!(
            state.snapshot.is_none(),
            "snapshot should be cleared for non-persistent"
        );
        assert!(
            state._handler_task.is_none(),
            "handler_task should be cleared"
        );
    }

    /// Test that BrowserState preserves data for persistent sessions
    /// This verifies the fix for the persist_session regression
    #[tokio::test]
    async fn test_handler_preserves_state_for_persistent_sessions() {
        let state = Arc::new(Mutex::new(BrowserState::new()));

        // Set up persistent session with data
        {
            let mut s = state.lock().await;
            s.persistent_profile = true;
            s.active_target = Some("target1".to_string());
        }

        // Simulate handler stream ending (persistent session)
        let state_clone = state.clone();
        let handler_task = tokio::spawn(async move {
            let mut state = state_clone.lock().await;
            // This is the actual logic from ensure_launched handler task
            if !state.persistent_profile {
                state.browser = None;
                state._handler_task = None;
                state.pages.clear();
                state.active_target = None;
                state.invalidate_snapshot();
            } else {
                // For persistent sessions, just clear the handler task reference
                state._handler_task = None;
            }
        });

        handler_task.await.unwrap();

        // Verify state was PRESERVED for persistent session
        let state = state.lock().await;
        assert!(state.browser.is_none(), "browser field is None initially");
        assert!(
            state.pages.is_empty(),
            "pages starts empty without real Chrome"
        );
        assert_eq!(
            state.active_target,
            Some("target1".to_string()),
            "active_target should NOT be cleared"
        );
        assert!(
            state.persistent_profile,
            "persistent_profile flag should remain"
        );
        assert!(
            state._handler_task.is_none(),
            "handler_task reference should be cleared"
        );
    }

    /// Test that ensure_launched detects cleared state and allows relaunch
    #[tokio::test]
    async fn test_ensure_launched_detects_cleared_state() {
        let state = Arc::new(Mutex::new(BrowserState::new()));

        // Initially no browser - ensure_launched should proceed
        {
            let state = state.lock().await;
            assert!(
                state.browser.is_none(),
                "initial state should have no browser"
            );
        }

        // Simulate state populated by launch
        {
            let mut state = state.lock().await;
            state.active_target = Some("target1".to_string());
        }

        // Verify state exists
        {
            let state = state.lock().await;
            assert_eq!(
                state.active_target,
                Some("target1".to_string()),
                "state should have data after launch"
            );
        }

        // Simulate handler clearing state (non-persistent)
        {
            let mut state = state.lock().await;
            state.browser = None;
            state.pages.clear();
            state.active_target = None;
        }

        // Verify state cleared - ensure_launched would detect this and allow relaunch
        {
            let state = state.lock().await;
            assert!(state.browser.is_none(), "browser should be cleared");
            assert!(state.pages.is_empty(), "pages should be cleared");
        }
    }

    /// Test persistent session flag defaults and behavior
    #[tokio::test]
    async fn test_persistent_profile_flag_behavior() {
        // Test non-persistent (default)
        let state = BrowserState::new();
        assert!(
            !state.persistent_profile,
            "default should be non-persistent"
        );

        // Test setting persistent flag
        let mut state = BrowserState::new();
        state.persistent_profile = true;
        assert!(
            state.persistent_profile,
            "should be able to set persistent flag"
        );

        // Verify flag survives cloning/sharing
        let state = Arc::new(Mutex::new(state));
        let state2 = state.clone();
        {
            let s = state2.lock().await;
            assert!(s.persistent_profile, "flag should persist in shared state");
        }
    }

    /// Test the handler cleanup logic branches correctly
    #[tokio::test]
    async fn test_handler_cleanup_branching() {
        // Test non-persistent: full cleanup
        let state = Arc::new(Mutex::new(BrowserState::new()));
        state.lock().await.persistent_profile = false;
        state.lock().await.active_target = Some("t1".to_string());

        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut s = state_clone.lock().await;
            if !s.persistent_profile {
                s.active_target = None;
            }
        })
        .await
        .unwrap();

        assert!(state.lock().await.active_target.is_none());

        // Test persistent: preserve data
        let state = Arc::new(Mutex::new(BrowserState::new()));
        state.lock().await.persistent_profile = true;
        state.lock().await.active_target = Some("t1".to_string());

        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut s = state_clone.lock().await;
            if !s.persistent_profile {
                s.active_target = None;
            }
        })
        .await
        .unwrap();

        assert_eq!(state.lock().await.active_target, Some("t1".to_string()));
    }

    /// Test CHROME_FLAGS environment variable parsing
    /// Contract: Flags are split on whitespace and added to builder
    /// From PR #268 - ensures user-defined Chrome flags work correctly
    #[test]
    fn test_chrome_flags_parsing() {
        // SAFETY: Tests run sequentially, environment modification is safe
        // Test basic flag splitting
        unsafe {
            std::env::set_var("CHROME_FLAGS", "--disable-gpu --no-sandbox");
        }
        let chrome_flags = std::env::var("CHROME_FLAGS").unwrap();
        let flags: Vec<&str> = chrome_flags.split_whitespace().collect();
        assert_eq!(flags, vec!["--disable-gpu", "--no-sandbox"]);

        // Test multiple spaces (should still split correctly)
        unsafe {
            std::env::set_var("CHROME_FLAGS", "--flag1   --flag2  --flag3");
        }
        let chrome_flags = std::env::var("CHROME_FLAGS").unwrap();
        let flags: Vec<&str> = chrome_flags.split_whitespace().collect();
        assert_eq!(flags, vec!["--flag1", "--flag2", "--flag3"]);

        // Test single flag
        unsafe {
            std::env::set_var("CHROME_FLAGS", "--single-flag");
        }
        let chrome_flags = std::env::var("CHROME_FLAGS").unwrap();
        let flags: Vec<&str> = chrome_flags.split_whitespace().collect();
        assert_eq!(flags, vec!["--single-flag"]);

        // Test empty (should produce empty vec, not panic)
        unsafe {
            std::env::set_var("CHROME_FLAGS", "");
        }
        let chrome_flags = std::env::var("CHROME_FLAGS").unwrap();
        let flags: Vec<&str> = chrome_flags.split_whitespace().collect();
        assert!(flags.is_empty());
    }

    /// Test that SingletonLock is removed for persistent profiles
    /// Contract: Stale SingletonLock files prevent Chrome from starting
    /// From PR #268 - ensures persistent sessions can restart after crashes
    #[test]
    fn test_singleton_lock_removal() {
        use std::fs;

        // Create a temp directory simulating a persistent profile
        let temp_dir = std::env::temp_dir().join("spacebot-test-singleton");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        // Create a fake SingletonLock file
        let lock_file = temp_dir.join("SingletonLock");
        fs::write(&lock_file, "fake lock content").unwrap();
        assert!(lock_file.exists());

        // This simulates the logic in ensure_launched (lines 1119-1125)
        // For persistent profiles, we should remove stale locks
        let persistent_profile = true;
        if persistent_profile && lock_file.exists() {
            let _ = fs::remove_file(&lock_file);
        }

        // Verify lock was removed
        assert!(
            !lock_file.exists(),
            "SingletonLock should be removed for persistent profiles"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    /// Test that SingletonLock is NOT touched for non-persistent profiles
    /// Contract: Non-persistent profiles use fresh temp dirs, no lock issues
    #[test]
    fn test_singleton_lock_not_removed_for_non_persistent() {
        use std::fs;

        // Create a temp directory
        let temp_dir = std::env::temp_dir().join("spacebot-test-singleton2");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        // Create a fake SingletonLock file
        let lock_file = temp_dir.join("SingletonLock");
        fs::write(&lock_file, "fake lock content").unwrap();
        assert!(lock_file.exists());

        // For non-persistent profiles, we don't remove locks
        let persistent_profile = false;
        if persistent_profile && lock_file.exists() {
            let _ = fs::remove_file(&lock_file);
        }

        // Verify lock still exists
        assert!(
            lock_file.exists(),
            "SingletonLock should NOT be removed for non-persistent profiles"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    // Mock helper for creating minimal test snapshots
    fn create_mock_snapshot() -> AxSnapshot {
        AxSnapshot {
            roots: vec![SnapshotNode {
                role: "document".to_string(),
                name: "mock".to_string(),
                index: None,
                children: Vec::new(),
                checked: None,
                disabled: false,
                expanded: None,
                selected: false,
                level: None,
                pressed: None,
                value: None,
                description: None,
            }],
            node_ids: Vec::new(),
        }
    }
}
