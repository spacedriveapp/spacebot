use super::state::ApiState;
use crate::error::{Error as CrateError, WikiError};
use crate::wiki::{CreateWikiPageInput, EditWikiPageInput, WikiPageType, WikiStore};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Map a crate-level wiki error to an HTTP status.
fn wiki_error_status(error: CrateError) -> StatusCode {
    match error {
        CrateError::Wiki(wiki) => match *wiki {
            WikiError::NotFound { .. } | WikiError::VersionNotFound { .. } => StatusCode::NOT_FOUND,
            WikiError::EditFailed(_) => StatusCode::BAD_REQUEST,
            WikiError::Database(inner) => {
                tracing::error!(%inner, "wiki database error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            WikiError::Other(inner) => {
                tracing::error!(%inner, "wiki store error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        },
        other => {
            tracing::error!(error = %other, "wiki handler error");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WikiListQuery {
    #[serde(default)]
    page_type: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WikiSearchQuery {
    query: String,
    #[serde(default)]
    page_type: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WikiHistoryQuery {
    #[serde(default = "default_history_limit")]
    limit: i64,
}

fn default_history_limit() -> i64 {
    20
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WikiVersionQuery {
    #[serde(default)]
    version: Option<i64>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreatePageRequest {
    title: String,
    page_type: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    related: Vec<String>,
    #[serde(default)]
    edit_summary: Option<String>,
    /// Who is creating this page: agent_id or user identifier.
    #[serde(default = "default_author")]
    author_id: String,
    #[serde(default = "default_author_type")]
    author_type: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct EditPageRequest {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    #[serde(default)]
    edit_summary: Option<String>,
    #[serde(default = "default_author")]
    author_id: String,
    #[serde(default = "default_author_type")]
    author_type: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct RestoreVersionRequest {
    version: i64,
    #[serde(default = "default_author")]
    author_id: String,
    #[serde(default = "default_author_type")]
    author_type: String,
}

fn default_author() -> String {
    "user".to_string()
}

fn default_author_type() -> String {
    "user".to_string()
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WikiListResponse {
    pages: Vec<crate::wiki::WikiPageSummary>,
    total: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WikiPageResponse {
    page: crate::wiki::WikiPage,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WikiHistoryResponse {
    versions: Vec<crate::wiki::WikiPageVersion>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WikiActionResponse {
    success: bool,
    message: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_wiki_store(state: &ApiState) -> Result<Arc<WikiStore>, StatusCode> {
    state
        .wiki_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn parse_page_type(s: Option<&str>) -> Result<Option<WikiPageType>, StatusCode> {
    match s {
        None => Ok(None),
        Some(v) => Ok(Some(WikiPageType::parse(v).ok_or(StatusCode::BAD_REQUEST)?)),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /wiki — list all wiki pages
#[utoipa::path(
    get,
    path = "/wiki",
    params(WikiListQuery),
    responses(
        (status = 200, body = WikiListResponse),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn list_pages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WikiListQuery>,
) -> Result<Json<WikiListResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let page_type = parse_page_type(query.page_type.as_deref())?;
    let pages = store.list(page_type).await.map_err(wiki_error_status)?;
    let total = pages.len();
    Ok(Json(WikiListResponse { pages, total }))
}

/// GET /wiki/search — search wiki pages
#[utoipa::path(
    get,
    path = "/wiki/search",
    params(WikiSearchQuery),
    responses(
        (status = 200, body = WikiListResponse),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn search_pages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WikiSearchQuery>,
) -> Result<Json<WikiListResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let page_type = parse_page_type(query.page_type.as_deref())?;
    let pages = store
        .search(&query.query, page_type)
        .await
        .map_err(wiki_error_status)?;
    let total = pages.len();
    Ok(Json(WikiListResponse { pages, total }))
}

/// POST /wiki — create a new wiki page
#[utoipa::path(
    post,
    path = "/wiki",
    request_body = CreatePageRequest,
    responses(
        (status = 200, body = WikiPageResponse),
        (status = 400, description = "Invalid page_type"),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn create_page(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreatePageRequest>,
) -> Result<Json<WikiPageResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let page_type = WikiPageType::parse(&request.page_type).ok_or(StatusCode::BAD_REQUEST)?;

    let page = store
        .create(CreateWikiPageInput {
            title: request.title,
            page_type,
            content: request.content,
            related: request.related,
            author_type: request.author_type,
            author_id: request.author_id,
            edit_summary: request.edit_summary,
        })
        .await
        .map_err(wiki_error_status)?;

    Ok(Json(WikiPageResponse { page }))
}

/// GET /wiki/:slug — read a wiki page
#[utoipa::path(
    get,
    path = "/wiki/{slug}",
    params(
        ("slug" = String, Path, description = "Page slug"),
        WikiVersionQuery,
    ),
    responses(
        (status = 200, body = WikiPageResponse),
        (status = 404, description = "Page not found"),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn get_page(
    State(state): State<Arc<ApiState>>,
    Path(slug): Path<String>,
    Query(query): Query<WikiVersionQuery>,
) -> Result<Json<WikiPageResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let page = store
        .read(&slug, query.version)
        .await
        .map_err(wiki_error_status)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(WikiPageResponse { page }))
}

/// POST /wiki/:slug/edit — apply a partial edit
#[utoipa::path(
    post,
    path = "/wiki/{slug}/edit",
    params(("slug" = String, Path, description = "Page slug")),
    request_body = EditPageRequest,
    responses(
        (status = 200, body = WikiPageResponse),
        (status = 400, description = "Edit match failed"),
        (status = 404, description = "Page not found"),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn edit_page(
    State(state): State<Arc<ApiState>>,
    Path(slug): Path<String>,
    Json(request): Json<EditPageRequest>,
) -> Result<Json<WikiPageResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let page = store
        .edit(EditWikiPageInput {
            slug,
            old_string: request.old_string,
            new_string: request.new_string,
            replace_all: request.replace_all,
            edit_summary: request.edit_summary,
            author_type: request.author_type,
            author_id: request.author_id,
        })
        .await
        .map_err(wiki_error_status)?;
    Ok(Json(WikiPageResponse { page }))
}

/// GET /wiki/:slug/history — list version history
#[utoipa::path(
    get,
    path = "/wiki/{slug}/history",
    params(
        ("slug" = String, Path, description = "Page slug"),
        WikiHistoryQuery,
    ),
    responses(
        (status = 200, body = WikiHistoryResponse),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn get_history(
    State(state): State<Arc<ApiState>>,
    Path(slug): Path<String>,
    Query(query): Query<WikiHistoryQuery>,
) -> Result<Json<WikiHistoryResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let versions = store
        .history(&slug, query.limit)
        .await
        .map_err(wiki_error_status)?;
    Ok(Json(WikiHistoryResponse { versions }))
}

/// POST /wiki/:slug/restore — restore to a historical version
#[utoipa::path(
    post,
    path = "/wiki/{slug}/restore",
    params(("slug" = String, Path, description = "Page slug")),
    request_body = RestoreVersionRequest,
    responses(
        (status = 200, body = WikiPageResponse),
        (status = 404, description = "Page or version not found"),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn restore_version(
    State(state): State<Arc<ApiState>>,
    Path(slug): Path<String>,
    Json(request): Json<RestoreVersionRequest>,
) -> Result<Json<WikiPageResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    let page = store
        .restore(
            &slug,
            request.version,
            &request.author_type,
            &request.author_id,
        )
        .await
        .map_err(wiki_error_status)?;
    Ok(Json(WikiPageResponse { page }))
}

/// DELETE /wiki/:slug — archive a page
#[utoipa::path(
    delete,
    path = "/wiki/{slug}",
    params(("slug" = String, Path, description = "Page slug")),
    responses(
        (status = 200, body = WikiActionResponse),
        (status = 503, description = "Wiki store not initialized"),
    ),
    tag = "wiki",
)]
pub(super) async fn archive_page(
    State(state): State<Arc<ApiState>>,
    Path(slug): Path<String>,
) -> Result<Json<WikiActionResponse>, StatusCode> {
    let store = get_wiki_store(&state)?;
    store.archive(&slug).await.map_err(wiki_error_status)?;
    Ok(Json(WikiActionResponse {
        success: true,
        message: format!("Page '{slug}' archived"),
    }))
}
