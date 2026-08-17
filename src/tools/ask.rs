//! Ask tool: presents the user with a question and selectable answer options.
//!
//! Renders buttons or a select menu on platforms that support them, and as a
//! numbered list on text-only channels. Answers arrive as enriched interaction
//! messages that include the original question text for context.

use crate::api::ApiState;
use crate::conversation::ConversationLogger;
use crate::questions::{NewQuestion, QuestionStore};
use crate::{ChannelId, OutboundResponse, RoutedSender};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::tools::reply::RepliedFlag;

/// Generate a short random question ID for the custom_id prefix.
fn new_question_id() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// Tool that asks the user a question with selectable options.
#[derive(Clone)]
pub struct AskTool {
    question_store: QuestionStore,
    sender: RoutedSender,
    conversation_logger: ConversationLogger,
    channel_id: ChannelId,
    agent_id: String,
    agent_display_name: String,
    replied_flag: RepliedFlag,
    api_state: Option<Arc<ApiState>>,
}

impl std::fmt::Debug for AskTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskTool")
            .field("channel_id", &self.channel_id)
            .field("agent_id", &self.agent_id)
            .field("agent_display_name", &self.agent_display_name)
            .finish()
    }
}

impl AskTool {
    /// Create a new ask tool bound to a conversation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        question_store: QuestionStore,
        sender: RoutedSender,
        conversation_logger: ConversationLogger,
        channel_id: ChannelId,
        agent_id: String,
        agent_display_name: String,
        replied_flag: RepliedFlag,
        api_state: Option<Arc<ApiState>>,
    ) -> Self {
        Self {
            question_store,
            sender,
            conversation_logger,
            channel_id,
            agent_id,
            agent_display_name,
            replied_flag,
            api_state,
        }
    }
}

/// Error type for ask tool.
#[derive(Debug, thiserror::Error)]
#[error("Ask failed: {0}")]
pub struct AskError(String);

/// Arguments for ask tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// The question to ask.
    pub question: String,
    /// Selectable answers. 2 to 10 options.
    pub options: Vec<AskOptionArg>,
    /// Allow picking more than one option. Renders as a select menu
    /// with multi-select where supported.
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AskOptionArg {
    /// Short label shown on the button (keep under ~40 chars).
    pub label: String,
    /// Optional longer description, shown where the platform supports it
    /// (select menu descriptions, portal UI) and in the text fallback.
    #[serde(default)]
    pub description: Option<String>,
}

/// Output from ask tool.
#[derive(Debug, Serialize)]
pub struct AskOutput {
    pub question_id: String,
    pub question: String,
    pub options_count: usize,
    pub message: String,
}

fn build_text_fallback(question: &str, options: &[AskOptionArg]) -> String {
    let mut text = format!("{}\n", question.trim_end());
    for (i, opt) in options.iter().enumerate() {
        match &opt.description {
            Some(desc) if !desc.trim().is_empty() => {
                text.push_str(&format!(
                    "{}. {} — {}\n",
                    i + 1,
                    opt.label.trim(),
                    desc.trim()
                ));
            }
            _ => {
                text.push_str(&format!("{}. {}\n", i + 1, opt.label.trim()));
            }
        }
    }
    text.trim_end().to_string()
}

fn build_interactive_elements(
    question_id: &str,
    options: &[AskOptionArg],
    multi_select: bool,
) -> Vec<crate::InteractiveElements> {
    if multi_select || options.len() > 5 {
        // Select menu
        let select_options: Vec<crate::SelectOption> = options
            .iter()
            .enumerate()
            .map(|(idx, opt)| crate::SelectOption {
                label: opt.label.clone(),
                value: format!("ask:{}:{}", question_id, idx),
                description: opt.description.clone(),
                emoji: None,
            })
            .collect();

        let placeholder = if multi_select {
            "Select one or more options…".to_string()
        } else {
            "Select an option…".to_string()
        };

        vec![crate::InteractiveElements::Select {
            select: crate::SelectMenu {
                custom_id: format!("ask:{}:menu", question_id),
                options: select_options,
                placeholder: Some(placeholder),
            },
        }]
    } else {
        // Buttons
        let buttons: Vec<crate::Button> = options
            .iter()
            .enumerate()
            .map(|(idx, opt)| crate::Button {
                label: opt.label.clone(),
                custom_id: Some(format!("ask:{question_id}:{idx}")),
                style: crate::ButtonStyle::Primary,
                url: None,
            })
            .collect();
        vec![crate::InteractiveElements::Buttons { buttons }]
    }
}

pub(crate) const ASK_CUSTOM_ID_PREFIX: &str = "ask:";

/// Parse an ask custom_id into (question_id, option_index).
/// custom_id format: `ask:{question_id}:{idx}` or `ask:{question_id}:menu` for selects.
pub fn parse_ask_custom_id(custom_id: &str) -> Option<(&str, Option<usize>)> {
    let stripped = custom_id.strip_prefix(ASK_CUSTOM_ID_PREFIX)?;
    let (question_id, idx_part) = stripped.rsplit_once(':')?;
    if idx_part == "menu" {
        Some((question_id, None))
    } else {
        let idx: usize = idx_part.parse().ok()?;
        Some((question_id, Some(idx)))
    }
}

impl Tool for AskTool {
    const NAME: &'static str = "ask";

    type Error = AskError;
    type Args = AskArgs;
    type Output = AskOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let parameters = serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user."
                },
                "options": {
                    "type": "array",
                    "description": "Selectable answer options. Minimum 2, maximum 10.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {
                                "type": "string",
                                "description": "Short label shown on the button or select option."
                            },
                            "description": {
                                "type": "string",
                                "description": "Optional longer description for the option."
                            }
                        },
                        "required": ["label"]
                    }
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "Allow the user to pick more than one option. Renders as a multi-select menu where supported. Defaults to false."
                }
            },
            "required": ["question", "options"]
        });

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/ask").to_string(),
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let question = args.question.trim().to_string();
        if question.is_empty() {
            return Err(AskError("question must not be empty".into()));
        }

        // Trim and validate options
        let options: Vec<AskOptionArg> = args
            .options
            .into_iter()
            .map(|mut opt| {
                opt.label = opt.label.trim().to_string();
                opt.description = opt.description.map(|d| d.trim().to_string());
                opt
            })
            .filter(|opt| !opt.label.is_empty())
            .collect();

        if options.len() < 2 {
            return Err(AskError("at least 2 non-empty options are required".into()));
        }
        if options.len() > 10 {
            return Err(AskError("at most 10 options are allowed".into()));
        }

        let question_id = new_question_id();

        // Build text fallback (numbered list) — this IS the message on
        // text-only channels, and provides context on button channels.
        let text = build_text_fallback(&question, &options);

        // Build interactive elements
        let interactive_elements =
            build_interactive_elements(&question_id, &options, args.multi_select);

        tracing::info!(
            question_id = %question_id,
            channel_id = %self.channel_id,
            question_len = question.len(),
            options_count = options.len(),
            multi_select = args.multi_select,
            "ask tool sent question"
        );

        // Persist the pending question
        let store_options: Vec<crate::questions::AskOption> = options
            .iter()
            .map(|opt| crate::questions::AskOption {
                label: opt.label.clone(),
                description: opt.description.clone(),
            })
            .collect();

        let new_q = NewQuestion {
            question_id: question_id.clone(),
            agent_id: self.agent_id.clone(),
            channel_id: self.channel_id.to_string(),
            question: question.clone(),
            options: store_options,
            multi_select: args.multi_select,
            message_ref: None,
        };

        if let Err(e) = self.question_store.insert(&new_q).await {
            tracing::error!(error = %e, "failed to persist pending question");
            return Err(AskError(format!("failed to store question: {e}")));
        }

        // Send via RichMessage
        let response = OutboundResponse::RichMessage {
            text,
            blocks: vec![],
            cards: vec![],
            interactive_elements,
            poll: None,
        };

        if let Err(error) = self.sender.send_confirmed(response).await {
            if let Err(cleanup_error) = self.question_store.delete_unresolved(&question_id).await {
                tracing::warn!(%cleanup_error, %question_id, "failed to remove undelivered question");
            }
            return Err(AskError(format!("failed to send question: {error}")));
        }

        // Drain accumulated channel tool calls and pack into message metadata
        let tool_calls_json = if let Some(ref api_state) = self.api_state {
            let calls = api_state.take_channel_tool_calls(&self.channel_id).await;
            if calls.is_empty() {
                None
            } else {
                serde_json::to_string(&calls).ok()
            }
        } else {
            None
        };

        self.conversation_logger.log_bot_message_with_metadata(
            &self.channel_id,
            &format!("[ask] {question}"),
            Some(&self.agent_display_name),
            tool_calls_json,
        );

        // Mark turn as handled so the channel doesn't send fallback text
        self.replied_flag.store(true, Ordering::Relaxed);

        Ok(AskOutput {
            question_id,
            question,
            options_count: options.len(),
            message: "Question sent. The answer will arrive as a future message. End your turn now — do not speculate about the answer.".to_string(),
        })
    }
}
