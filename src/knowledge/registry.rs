//! Source registry for the knowledge retrieval plane.

use crate::config::McpServerConfig;

use super::types::{
    KnowledgeSourceCapability, KnowledgeSourceDescriptor, KnowledgeSourceEnablement,
    KnowledgeSourceKind,
};

pub const NATIVE_MEMORY_SOURCE_ID: &str = "native_memory";
pub const QMD_SOURCE_ID: &str = "qmd";
pub const GOOGLE_WORKSPACE_DRIVE_SOURCE_ID: &str = "google_workspace_drive";
pub const GOOGLE_WORKSPACE_GMAIL_SOURCE_ID: &str = "google_workspace_gmail";
pub const GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID: &str = "google_workspace_calendar";

/// Registry of known retrieval sources and their current enablement.
#[derive(Debug, Clone)]
pub struct KnowledgeSourceRegistry {
    sources: Vec<KnowledgeSourceDescriptor>,
}

impl KnowledgeSourceRegistry {
    pub fn from_mcp_servers(mcp_servers: &[McpServerConfig]) -> Self {
        let qmd_enablement = qmd_enablement(mcp_servers);

        Self {
            sources: vec![
                KnowledgeSourceDescriptor {
                    id: NATIVE_MEMORY_SOURCE_ID.to_string(),
                    label: "Native memory".to_string(),
                    kind: KnowledgeSourceKind::NativeMemory,
                    capabilities: vec![
                        KnowledgeSourceCapability::HybridSearch,
                        KnowledgeSourceCapability::TypedMemoryRecall,
                    ],
                    enablement: KnowledgeSourceEnablement::enabled(),
                },
                KnowledgeSourceDescriptor {
                    id: QMD_SOURCE_ID.to_string(),
                    label: "QMD".to_string(),
                    kind: KnowledgeSourceKind::Qmd,
                    capabilities: vec![KnowledgeSourceCapability::DocumentSearch],
                    enablement: qmd_enablement,
                },
                KnowledgeSourceDescriptor {
                    id: GOOGLE_WORKSPACE_DRIVE_SOURCE_ID.to_string(),
                    label: "Google Workspace Drive".to_string(),
                    kind: KnowledgeSourceKind::GoogleWorkspace,
                    capabilities: vec![KnowledgeSourceCapability::DocumentSearch],
                    enablement: KnowledgeSourceEnablement::placeholder(
                        "Google Workspace bridge adapter support is modeled for Drive; execution adapter is not yet wired.",
                    ),
                },
                KnowledgeSourceDescriptor {
                    id: GOOGLE_WORKSPACE_GMAIL_SOURCE_ID.to_string(),
                    label: "Google Workspace Gmail".to_string(),
                    kind: KnowledgeSourceKind::GoogleWorkspace,
                    capabilities: vec![KnowledgeSourceCapability::EmailSearch],
                    enablement: KnowledgeSourceEnablement::placeholder(
                        "Google Workspace bridge adapter support is modeled for Gmail; execution adapter is not yet wired.",
                    ),
                },
                KnowledgeSourceDescriptor {
                    id: GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID.to_string(),
                    label: "Google Workspace Calendar".to_string(),
                    kind: KnowledgeSourceKind::GoogleWorkspace,
                    capabilities: vec![KnowledgeSourceCapability::CalendarSearch],
                    enablement: KnowledgeSourceEnablement::placeholder(
                        "Google Workspace bridge adapter support is modeled for Calendar; execution adapter is not yet wired.",
                    ),
                },
            ],
        }
    }

    pub fn sources(&self) -> &[KnowledgeSourceDescriptor] {
        &self.sources
    }

    pub fn get(&self, source_id: &str) -> Option<&KnowledgeSourceDescriptor> {
        self.sources.iter().find(|source| source.id == source_id)
    }

    pub fn default_source_ids(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|source| {
                matches!(
                    source.enablement.state,
                    super::types::KnowledgeSourceEnablementState::Enabled
                        | super::types::KnowledgeSourceEnablementState::Configured
                )
            })
            .map(|source| source.id.clone())
            .collect()
    }
}

fn qmd_enablement(mcp_servers: &[McpServerConfig]) -> KnowledgeSourceEnablement {
    if mcp_servers
        .iter()
        .any(|server| server.enabled && server.name.eq_ignore_ascii_case("qmd"))
    {
        return KnowledgeSourceEnablement::configured(
            "QMD MCP transport is configured and available through the native retrieval adapter.",
        );
    }

    if mcp_servers
        .iter()
        .any(|server| !server.enabled && server.name.eq_ignore_ascii_case("qmd"))
    {
        return KnowledgeSourceEnablement::disabled(
            "QMD is present in config but disabled for this agent.",
        );
    }

    KnowledgeSourceEnablement::disabled("No QMD MCP server is configured for this agent.")
}

#[cfg(test)]
mod tests {
    use super::{
        GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID, GOOGLE_WORKSPACE_DRIVE_SOURCE_ID,
        GOOGLE_WORKSPACE_GMAIL_SOURCE_ID, KnowledgeSourceRegistry, NATIVE_MEMORY_SOURCE_ID,
        QMD_SOURCE_ID,
    };
    use crate::config::{McpServerConfig, McpTransport};
    use crate::knowledge::{
        KnowledgeSourceCapability, KnowledgeSourceEnablementState, KnowledgeSourceKind,
    };

    #[test]
    fn registry_models_qmd_and_google_workspace_placeholders() {
        let registry = KnowledgeSourceRegistry::from_mcp_servers(&[McpServerConfig {
            name: "qmd".to_string(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:8765/mcp".to_string(),
                headers: std::collections::HashMap::new(),
            },
            enabled: true,
        }]);

        let qmd = registry.get(QMD_SOURCE_ID).expect("qmd source present");
        assert_eq!(qmd.kind, KnowledgeSourceKind::Qmd);
        assert_eq!(
            qmd.enablement.state,
            KnowledgeSourceEnablementState::Configured
        );
        assert!(
            qmd.capabilities
                .contains(&KnowledgeSourceCapability::DocumentSearch)
        );

        let drive = registry
            .get(GOOGLE_WORKSPACE_DRIVE_SOURCE_ID)
            .expect("drive placeholder");
        assert_eq!(drive.kind, KnowledgeSourceKind::GoogleWorkspace);
        assert_eq!(
            drive.enablement.state,
            KnowledgeSourceEnablementState::Placeholder
        );
        assert!(
            drive
                .capabilities
                .contains(&KnowledgeSourceCapability::DocumentSearch)
        );

        let gmail = registry
            .get(GOOGLE_WORKSPACE_GMAIL_SOURCE_ID)
            .expect("gmail placeholder");
        assert!(
            gmail
                .capabilities
                .contains(&KnowledgeSourceCapability::EmailSearch)
        );

        let calendar = registry
            .get(GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID)
            .expect("calendar placeholder");
        assert!(
            calendar
                .capabilities
                .contains(&KnowledgeSourceCapability::CalendarSearch)
        );
    }

    #[test]
    fn registry_models_google_workspace_sources_under_one_family() {
        let registry = KnowledgeSourceRegistry::from_mcp_servers(&[]);
        let source_ids = [
            GOOGLE_WORKSPACE_DRIVE_SOURCE_ID,
            GOOGLE_WORKSPACE_GMAIL_SOURCE_ID,
            GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID,
        ];

        for source_id in source_ids {
            let source = registry
                .get(source_id)
                .expect("google workspace source exists");
            assert_eq!(source.kind, KnowledgeSourceKind::GoogleWorkspace);
            assert!(
                source.enablement.state
                    == crate::knowledge::KnowledgeSourceEnablementState::Placeholder
                    || source.enablement.state
                        == crate::knowledge::KnowledgeSourceEnablementState::Disabled
            );
            assert!(
                source
                    .enablement
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("bridge adapter"))
            );
        }
    }

    #[test]
    fn default_source_ids_include_configured_qmd() {
        let registry = KnowledgeSourceRegistry::from_mcp_servers(&[McpServerConfig {
            name: "qmd".to_string(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:8765/mcp".to_string(),
                headers: std::collections::HashMap::new(),
            },
            enabled: true,
        }]);

        let source_ids = registry.default_source_ids();
        assert!(source_ids.contains(&NATIVE_MEMORY_SOURCE_ID.to_string()));
        assert!(source_ids.contains(&QMD_SOURCE_ID.to_string()));
        assert!(!source_ids.contains(&GOOGLE_WORKSPACE_DRIVE_SOURCE_ID.to_string()));
    }
}
