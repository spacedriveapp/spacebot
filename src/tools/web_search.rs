//! Web search tool using the Brave Search API (task workers only).

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const BRAVE_WEB_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// Tool for searching the web via Brave Search.
#[derive(Debug, Clone)]
pub struct WebSearchTool {
    client: reqwest::Client,
    api_key: String,
}

impl WebSearchTool {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .gzip(true)
            .build()
            .expect("hardcoded reqwest client config");

        Self {
            client,
            api_key: api_key.into(),
        }
    }
}

/// Error type for web search tool.
#[derive(Debug, thiserror::Error)]
pub enum WebSearchError {
    #[error("Web search request failed: {0}")]
    RequestFailed(String),

    #[error("Failed to parse search response: {0}")]
    InvalidResponse(String),

    #[error("Rate limited by Brave Search API")]
    RateLimited,
}

/// Arguments for web search tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebSearchArgs {
    /// The search query.
    pub query: String,
    /// Number of results to return (1-20, default 5).
    #[serde(default = "default_count")]
    pub count: u8,
    /// Country code for search localization (e.g. "us", "gb", "de").
    pub country: Option<String>,
    /// Language code for search results (e.g. "en", "es", "fr").
    pub search_lang: Option<String>,
    /// Time filter for results. Options: "pd" (past day), "pw" (past week),
    /// "pm" (past month), "py" (past year).
    pub freshness: Option<String>,
}

fn default_count() -> u8 {
    5
}

/// Output from web search tool.
#[derive(Debug, Serialize)]
pub struct WebSearchOutput {
    /// The search results.
    pub results: Vec<SearchResult>,
    /// The query that was searched.
    pub query: String,
    /// Total number of results returned.
    pub result_count: usize,
}

/// A single web search result.
#[derive(Debug, Serialize)]
pub struct SearchResult {
    /// Page title.
    pub title: String,
    /// Page URL.
    pub url: String,
    /// Description snippet from the page.
    pub description: String,
    /// How old the result is (e.g. "2 days ago"), when available.
    pub age: Option<String>,
}

// -- Brave API response types (private, only model what we need) --

#[derive(Debug, Deserialize)]
struct BraveApiResponse {
    #[serde(default)]
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveWebResult>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
    age: Option<String>,
}

impl Tool for WebSearchTool {
    const NAME: &'static str = "web_search";

    type Error = WebSearchError;
    type Args = WebSearchArgs;
    type Output = WebSearchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Search the web using Brave Search. Returns page titles, URLs, and description snippets for the top results. Use this to find current information, look up documentation, research topics, or verify facts.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query. Be specific for better results."
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "default": 5,
                        "description": "Number of results to return (1-20)"
                    },
                    "country": {
                        "type": "string",
                        "description": "Country code for localized results (e.g. \"us\", \"gb\", \"de\")"
                    },
                    "search_lang": {
                        "type": "string",
                        "description": "Language code for results (e.g. \"en\", \"es\", \"fr\")"
                    },
                    "freshness": {
                        "type": "string",
                        "enum": ["pd", "pw", "pm", "py"],
                        "description": "Time filter: pd (past day), pw (past week), pm (past month), py (past year)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let count = args.count.clamp(1, 20);

        let mut request = self
            .client
            .get(BRAVE_WEB_SEARCH_URL)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", &args.query)])
            .query(&[("count", &count.to_string())]);

        if let Some(country) = &args.country {
            request = request.query(&[("country", country)]);
        }
        if let Some(search_lang) = &args.search_lang {
            request = request.query(&[("search_lang", search_lang)]);
        }
        if let Some(freshness) = &args.freshness {
            request = request.query(&[("freshness", freshness)]);
        }

        let response = request
            .send()
            .await
            .map_err(|error| WebSearchError::RequestFailed(error.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(WebSearchError::RateLimited);
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "failed to read response body".into());
            return Err(WebSearchError::RequestFailed(format!(
                "HTTP {status}: {body}"
            )));
        }

        let api_response: BraveApiResponse = response
            .json()
            .await
            .map_err(|error| WebSearchError::InvalidResponse(error.to_string()))?;

        let results: Vec<SearchResult> = api_response
            .web
            .map(|web| {
                web.results
                    .into_iter()
                    .map(|result| SearchResult {
                        title: clean_html_tags(&result.title),
                        url: result.url,
                        description: clean_html_tags(&result.description),
                        age: result.age,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let result_count = results.len();

        Ok(WebSearchOutput {
            results,
            query: args.query,
            result_count,
        })
    }
}

/// Strip basic HTML tags (like <strong>) from Brave API text fields.
fn clean_html_tags(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_tag = false;

    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_html_tags() {
        assert_eq!(
            clean_html_tags("Best <strong>Greek</strong> Restaurants"),
            "Best Greek Restaurants"
        );
        assert_eq!(clean_html_tags("no tags here"), "no tags here");
        assert_eq!(clean_html_tags("<b>all bold</b>"), "all bold");
        assert_eq!(clean_html_tags(""), "");
    }

    #[test]
    fn test_default_count() {
        let args: WebSearchArgs = serde_json::from_str(r#"{"query": "test"}"#).unwrap();
        assert_eq!(args.count, 5);
    }
}
