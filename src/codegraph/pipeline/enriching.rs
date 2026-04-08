//! Phase 9a (Wave 6): LLM-driven label generation for Community nodes.
//!
//! Queries the top-N communities by size, gathers their dominant symbol
//! names, and asks an LLM to generate a short name + description for
//! each. Results are batched (10 communities per request) so the total
//! number of LLM calls stays bounded on large projects.
//!
//! The call goes out through `LlmManager::http_client()` directly — we
//! don't build on top of `rig::Agent` because enrichment is a trivial
//! single-shot text generation, and the rig stack assumes an agent
//! context (conversation history, tools, streaming) we don't need here.
//!
//! If any step fails (no API key, rate limit, malformed response), we
//! log and continue — phase 10 still runs and produces a valid index,
//! just without LLM labels.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::codegraph::db::SharedCodeGraphDb;
use crate::llm::LlmManager;

/// How many communities we batch into a single LLM request. Kept low
/// so token budget per request stays under ~2k input + 500 output.
const BATCH_SIZE: usize = 10;

/// Maximum top symbols we include per community in the prompt. More
/// than this bloats the request without improving label quality.
const MAX_SYMBOLS_PER_COMMUNITY: usize = 8;

/// Hard cap on communities we bother enriching — anything past this
/// is statistically unlikely to be a meaningful module-level cluster.
const MAX_COMMUNITIES: usize = 50;

/// Anthropic model used for enrichment. Using the cheap Haiku tier
/// since the task is simple and latency matters more than quality.
const ENRICHMENT_MODEL: &str = "claude-haiku-4-5-20251001";

const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Enrich community nodes with LLM-generated labels.
pub async fn enrich(
    project_id: &str,
    db: &SharedCodeGraphDb,
    llm: &LlmManager,
) -> Result<()> {
    tracing::debug!(
        project_id = %project_id,
        "enriching communities with LLM labels"
    );

    // 1. Grab the API key up front. If it's missing we bail immediately
    //    so we don't waste time building a prompt nothing can consume.
    let api_key = match llm.get_api_key("anthropic") {
        Ok(key) => key,
        Err(err) => {
            tracing::info!(%err, "LLM enrichment skipped — no anthropic key");
            return Ok(());
        }
    };

    // 2. Query communities ordered by node_count descending. LadybugDB's
    //    Cypher supports ORDER BY and LIMIT, so we can do this in one query.
    let pid = cypher_escape(project_id);
    let community_rows = db
        .query(&format!(
            "MATCH (c:Community) WHERE c.project_id = '{pid}' \
             RETURN c.qualified_name, c.name, c.node_count, c.file_count, c.function_count \
             ORDER BY c.node_count DESC LIMIT {MAX_COMMUNITIES}"
        ))
        .await?;

    if community_rows.is_empty() {
        tracing::info!(project_id = %project_id, "no communities to enrich");
        return Ok(());
    }

    // 3. For each community, gather a handful of representative symbol
    //    names by following MEMBER_OF edges.
    let mut communities: Vec<CommunityContext> = Vec::new();
    for row in &community_rows {
        let comm_qname = match row.first() {
            Some(lbug::Value::String(s)) => s.clone(),
            _ => continue,
        };
        let current_name = match row.get(1) {
            Some(lbug::Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let node_count = cell_to_u64(row.get(2)).unwrap_or(0);
        let file_count = cell_to_u64(row.get(3)).unwrap_or(0);
        let function_count = cell_to_u64(row.get(4)).unwrap_or(0);

        let comm_escaped = cypher_escape(&comm_qname);
        let member_rows = db
            .query(&format!(
                "MATCH (n)-[r:CodeRelation]->(c:Community) \
                 WHERE r.type = 'MEMBER_OF' AND c.qualified_name = '{comm_escaped}' \
                 RETURN n.name LIMIT {MAX_SYMBOLS_PER_COMMUNITY}"
            ))
            .await?;

        let symbols: Vec<String> = member_rows
            .iter()
            .filter_map(|r| match r.first() {
                Some(lbug::Value::String(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();

        communities.push(CommunityContext {
            qualified_name: comm_qname,
            current_name,
            node_count,
            file_count,
            function_count,
            top_symbols: symbols,
        });
    }

    tracing::info!(
        project_id = %project_id,
        communities = communities.len(),
        "dispatching LLM enrichment batches"
    );

    // 4. Process in batches. Each batch is one HTTP call.
    let http_client = llm.http_client();
    let mut updated = 0u64;

    for batch in communities.chunks(BATCH_SIZE) {
        match enrich_batch(http_client, &api_key, batch).await {
            Ok(labels) => {
                for label in labels {
                    if let Err(err) = apply_label(db, &pid, &label).await {
                        tracing::debug!(%err, "failed to apply community label");
                    } else {
                        updated += 1;
                    }
                }
            }
            Err(err) => {
                tracing::warn!(%err, "LLM enrichment batch failed, continuing with next");
            }
        }
    }

    tracing::info!(
        project_id = %project_id,
        updated,
        "LLM enrichment complete"
    );

    Ok(())
}

/// Build the prompt, call Anthropic Messages API, parse the response.
async fn enrich_batch(
    client: &reqwest::Client,
    api_key: &str,
    batch: &[CommunityContext],
) -> Result<Vec<CommunityLabel>> {
    let prompt = build_prompt(batch);

    let body = json!({
        "model": ENRICHMENT_MODEL,
        "max_tokens": 800,
        "messages": [
            {
                "role": "user",
                "content": prompt,
            }
        ],
    });

    let response = client
        .post(ANTHROPIC_ENDPOINT)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("sending anthropic request")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("anthropic request failed ({status}): {text}");
    }

    let parsed: AnthropicResponse = response
        .json()
        .await
        .context("parsing anthropic response")?;

    let text = parsed
        .content
        .into_iter()
        .find_map(|block| (block.kind == "text").then_some(block.text))
        .context("no text block in anthropic response")?;

    parse_labels(&text, batch)
}

/// Build the user prompt with one JSON entry per community. The model
/// is instructed to return a strict JSON array so we can parse without
/// tolerating free-form prose.
fn build_prompt(batch: &[CommunityContext]) -> String {
    let mut entries = Vec::new();
    for (i, comm) in batch.iter().enumerate() {
        entries.push(format!(
            "  {{\
             \n    \"id\": {i},\
             \n    \"current_name\": \"{}\",\
             \n    \"members\": {},\
             \n    \"files\": {},\
             \n    \"functions\": {},\
             \n    \"top_symbols\": [{}]\
             \n  }}",
            comm.current_name.replace('"', "\\\""),
            comm.node_count,
            comm.file_count,
            comm.function_count,
            comm.top_symbols
                .iter()
                .map(|s| format!("\"{}\"", s.replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    format!(
        "You are labeling functional clusters of code in a software project. \
         For each cluster below, produce a short human-readable name (3-6 words) \
         and a one-sentence description of what the cluster does.\n\n\
         Return ONLY a JSON array (no markdown fences) with one object per input \
         cluster, each with fields `id`, `name`, `description`. Preserve the \
         input `id` exactly.\n\n\
         Clusters:\n[\n{}\n]",
        entries.join(",\n")
    )
}

/// Parse the model's JSON response back into typed labels, pairing each
/// `id` with the original community so we can update LadybugDB by
/// `qualified_name`.
fn parse_labels(text: &str, batch: &[CommunityContext]) -> Result<Vec<CommunityLabel>> {
    // Strip any ``` fences the model might have added despite our
    // instructions, then find the JSON array.
    let trimmed = text.trim().trim_start_matches("```json").trim_start_matches("```");
    let trimmed = trimmed.trim_end_matches("```").trim();

    let parsed: Vec<ModelLabel> = serde_json::from_str(trimmed)
        .with_context(|| format!("parsing LLM response as JSON array: {trimmed}"))?;

    let mut out = Vec::new();
    for label in parsed {
        if let Some(comm) = batch.get(label.id) {
            out.push(CommunityLabel {
                qualified_name: comm.qualified_name.clone(),
                name: label.name,
                description: label.description,
            });
        }
    }
    Ok(out)
}

/// Update a single Community node in LadybugDB.
async fn apply_label(
    db: &SharedCodeGraphDb,
    pid: &str,
    label: &CommunityLabel,
) -> Result<()> {
    let qname = cypher_escape(&label.qualified_name);
    let name = cypher_escape(&label.name);
    let description = cypher_escape(&label.description);

    let stmt = format!(
        "MATCH (c:Community) WHERE c.qualified_name = '{qname}' \
         AND c.project_id = '{pid}' \
         SET c.name = '{name}', c.description = '{description}'"
    );
    db.execute(&stmt).await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct CommunityContext {
    qualified_name: String,
    current_name: String,
    node_count: u64,
    file_count: u64,
    function_count: u64,
    top_symbols: Vec<String>,
}

#[derive(Debug, Clone)]
struct CommunityLabel {
    qualified_name: String,
    name: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct ModelLabel {
    id: usize,
    name: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

fn cell_to_u64(cell: Option<&lbug::Value>) -> Option<u64> {
    match cell? {
        lbug::Value::Int64(n) => Some(*n as u64),
        lbug::Value::Int32(n) => Some(*n as u64),
        lbug::Value::Int16(n) => Some(*n as u64),
        _ => None,
    }
}
