//! Repo-to-repo dependency edges: declared knowledge about how the repos in a
//! project relate ("the web client is generated from the api contract").
//!
//! Two things use what is declared here: the project view draws it, and the
//! workflow step editor asks [`ProjectStore::repo_dependency_suggestions`] what
//! else it should offer to touch. Neither creates workflow edges from a
//! declaration — see the comment on that function, which is the design.

use super::store::ProjectStore;
use crate::error::Result;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One declared edge, with both repo names resolved so a caller can render it
/// without a second query.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RepoDependency {
    pub project_id: String,
    /// The dependent repo — the one that has to change when the other does.
    pub repo_id: String,
    pub repo_name: String,
    /// The repo depended upon.
    pub depends_on_repo_id: String,
    pub depends_on_repo_name: String,
    /// Free-text label (`generated_from`, `consumes`, `vendors`, …). Nothing
    /// branches on it; it is shown to people.
    pub kind: Option<String>,
    /// Why the dependency exists, in the author's words.
    pub note: Option<String>,
    pub created_at: String,
}

/// Everything a declaration needs. `kind` and `note` are optional because a
/// bare "these two are related" is still worth recording.
#[derive(Debug, Clone)]
pub struct DeclareRepoDependencyInput {
    pub project_id: String,
    pub repo_id: String,
    pub depends_on_repo_id: String,
    pub kind: Option<String>,
    pub note: Option<String>,
}

/// The declared neighbourhood of one repo, in both directions.
///
/// Returned by the suggestion query. The step editor holds a repo and needs
/// both halves: "you are editing a step in `api`; `web` depends on it" comes
/// from `dependents`, and "this step is in `web`, which is generated from
/// `api`" comes from `dependencies`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RepoDependencySuggestions {
    /// The repo the question was asked about.
    pub repo_id: String,
    /// Repos that declare a dependency **on** this one — downstream. Editing
    /// this repo is a reason to offer a step in each of these.
    pub dependents: Vec<RepoDependency>,
    /// Repos this one declares a dependency **on** — upstream.
    pub dependencies: Vec<RepoDependency>,
}

/// Why a declaration was refused.
///
/// Cycles are deliberately absent: see [`ProjectStore::declare_repo_dependency`].
#[derive(Debug, thiserror::Error)]
pub enum RepoDependencyError {
    #[error("a repo cannot depend on itself")]
    SelfDependency { repo_id: String },
    #[error("repo {repo_id} does not exist")]
    UnknownRepo { repo_id: String },
    #[error("repo {repo_id} belongs to a different project")]
    ForeignRepo { repo_id: String },
    #[error("that dependency is already declared")]
    Duplicate {
        repo_id: String,
        depends_on_repo_id: String,
    },
    #[error("repo dependency storage error: {0}")]
    Storage(String),
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

impl ProjectStore {
    /// Declare that `repo_id` depends on `depends_on_repo_id`.
    ///
    /// Refuses a self-dependency, an unknown repo, a repo belonging to another
    /// project, and a duplicate of an existing declaration.
    ///
    /// **Cycles are allowed on purpose.** Two repos generating each other is a
    /// real arrangement — a contract repo whose types are regenerated from a
    /// server's reflection output, which itself is built against the contract —
    /// and it is not a mistake to say so. Refusing cycles would only be
    /// justified if something walked this graph to decide execution order, and
    /// nothing does: these edges produce suggestions and a picture, both of
    /// which terminate. The moment anything tries to *derive* an ordering from
    /// them, cycles become that consumer's problem to detect, and this refusal
    /// list is the wrong place to have pre-solved it.
    pub async fn declare_repo_dependency(
        &self,
        input: DeclareRepoDependencyInput,
    ) -> std::result::Result<RepoDependency, RepoDependencyError> {
        if input.repo_id == input.depends_on_repo_id {
            return Err(RepoDependencyError::SelfDependency {
                repo_id: input.repo_id,
            });
        }

        // Both endpoints must exist and must live in the project the caller
        // named. A declaration spanning projects would draw an arrow the
        // project view has no node for.
        for repo_id in [&input.repo_id, &input.depends_on_repo_id] {
            let repo = self
                .get_repo(repo_id)
                .await
                .map_err(|error| RepoDependencyError::Storage(error.to_string()))?
                .ok_or_else(|| RepoDependencyError::UnknownRepo {
                    repo_id: repo_id.clone(),
                })?;
            if repo.project_id != input.project_id {
                return Err(RepoDependencyError::ForeignRepo {
                    repo_id: repo_id.clone(),
                });
            }
        }

        let result = sqlx::query(
            r#"
            INSERT INTO repo_dependencies (project_id, repo_id, depends_on_repo_id, kind, note)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(&input.project_id)
        .bind(&input.repo_id)
        .bind(&input.depends_on_repo_id)
        .bind(&input.kind)
        .bind(&input.note)
        .execute(self.pool())
        .await;

        if let Err(error) = result {
            // The primary key is the duplicate check. Reporting it as a named
            // refusal rather than a storage failure is what lets the API answer
            // 409 instead of 500.
            let is_duplicate = matches!(
                &error,
                sqlx::Error::Database(db) if db.code().as_deref() == Some("1555")
                    || db.message().contains("UNIQUE constraint failed")
            );
            return Err(if is_duplicate {
                RepoDependencyError::Duplicate {
                    repo_id: input.repo_id,
                    depends_on_repo_id: input.depends_on_repo_id,
                }
            } else {
                RepoDependencyError::Storage(error.to_string())
            });
        }

        self.get_repo_dependency(&input.project_id, &input.repo_id, &input.depends_on_repo_id)
            .await
            .map_err(|error| RepoDependencyError::Storage(error.to_string()))?
            .ok_or_else(|| RepoDependencyError::Storage("declaration missing after insert".into()))
    }

    /// Fetch a single declaration.
    pub async fn get_repo_dependency(
        &self,
        project_id: &str,
        repo_id: &str,
        depends_on_repo_id: &str,
    ) -> Result<Option<RepoDependency>> {
        let row = sqlx::query(&format!(
            "{SELECT_WITH_NAMES} WHERE d.project_id = ? AND d.repo_id = ? AND d.depends_on_repo_id = ?"
        ))
        .bind(project_id)
        .bind(repo_id)
        .bind(depends_on_repo_id)
        .fetch_optional(self.pool())
        .await
        .context("failed to fetch repo dependency")?;

        row.map(|r| row_to_dependency(&r)).transpose()
    }

    /// Every declaration in a project — what the project view draws.
    pub async fn list_repo_dependencies(&self, project_id: &str) -> Result<Vec<RepoDependency>> {
        let rows = sqlx::query(&format!(
            "{SELECT_WITH_NAMES} WHERE d.project_id = ? ORDER BY dependent.name ASC, dependency.name ASC"
        ))
        .bind(project_id)
        .fetch_all(self.pool())
        .await
        .context("failed to list repo dependencies")?;

        rows.iter().map(row_to_dependency).collect()
    }

    /// Edit the label and note on an existing declaration. Returns `None` if
    /// there is no such declaration.
    ///
    /// The edge itself is not editable: repointing it is a different statement
    /// about the repos, so it is a delete and a declare, and the created_at of
    /// the new one is honest about when it was said.
    pub async fn update_repo_dependency(
        &self,
        project_id: &str,
        repo_id: &str,
        depends_on_repo_id: &str,
        kind: Option<&str>,
        note: Option<&str>,
    ) -> Result<Option<RepoDependency>> {
        let result = sqlx::query(
            r#"
            UPDATE repo_dependencies
            SET kind = ?, note = ?
            WHERE project_id = ? AND repo_id = ? AND depends_on_repo_id = ?
            "#,
        )
        .bind(kind)
        .bind(note)
        .bind(project_id)
        .bind(repo_id)
        .bind(depends_on_repo_id)
        .execute(self.pool())
        .await
        .context("failed to update repo dependency")?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }

        self.get_repo_dependency(project_id, repo_id, depends_on_repo_id)
            .await
    }

    /// Withdraw a declaration. Returns whether there was one.
    pub async fn delete_repo_dependency(
        &self,
        project_id: &str,
        repo_id: &str,
        depends_on_repo_id: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            DELETE FROM repo_dependencies
            WHERE project_id = ? AND repo_id = ? AND depends_on_repo_id = ?
            "#,
        )
        .bind(project_id)
        .bind(repo_id)
        .bind(depends_on_repo_id)
        .execute(self.pool())
        .await
        .context("failed to delete repo dependency")?;

        Ok(result.rows_affected() > 0)
    }

    /// The suggestion query: what is declared around this repo, in both
    /// directions.
    ///
    /// This is what the workflow step editor calls. You are adding a step that
    /// runs in `api`; this answers "`web` declares that it is generated from
    /// `api`", and the editor offers to add a `web` step downstream.
    ///
    /// # It offers. It does not add.
    ///
    /// The obvious next thought is "the system already knows `web` depends on
    /// `api`, so why not just create the edge". Do not. A derived edge makes a
    /// run wait on work that was never going to happen, and the person
    /// debugging the stall has no reason to suspect a repo relationship they
    /// may not know exists — the template they are reading does not mention it,
    /// because nobody wrote it there. Nothing in the failure points back at the
    /// declaration that caused it.
    ///
    /// A suggestion fails the recoverable way: it appears, you take it or you
    /// don't, and what runs is what someone agreed to. That also keeps stale
    /// declarations cheap — a wrong declaration that only ever suggests is a
    /// nuisance, while one that silently created edges would be a fault, and
    /// the whole reason this table is safe to leave lying around is that it
    /// cannot become one.
    ///
    /// The same rule applies to the other consumers: a workflow edge that
    /// contradicts a declaration is a remark, not an error, and a declaration
    /// with no corresponding step is a hint. The declaration describes the
    /// repos; it does not describe the pipeline, and a template may
    /// legitimately disagree with it.
    pub async fn repo_dependency_suggestions(
        &self,
        repo_id: &str,
    ) -> Result<RepoDependencySuggestions> {
        let dependent_rows = sqlx::query(&format!(
            "{SELECT_WITH_NAMES} WHERE d.depends_on_repo_id = ? ORDER BY dependent.name ASC"
        ))
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .context("failed to list repos depending on this one")?;

        let dependency_rows = sqlx::query(&format!(
            "{SELECT_WITH_NAMES} WHERE d.repo_id = ? ORDER BY dependency.name ASC"
        ))
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .context("failed to list repos this one depends on")?;

        Ok(RepoDependencySuggestions {
            repo_id: repo_id.to_string(),
            dependents: dependent_rows
                .iter()
                .map(row_to_dependency)
                .collect::<Result<Vec<_>>>()?,
            dependencies: dependency_rows
                .iter()
                .map(row_to_dependency)
                .collect::<Result<Vec<_>>>()?,
        })
    }
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

/// Both endpoints joined to their repo names. Every read resolves names so no
/// caller has to fetch the repo list to render an arrow.
const SELECT_WITH_NAMES: &str = r#"
    SELECT d.*, dependent.name AS repo_name, dependency.name AS depends_on_repo_name
    FROM repo_dependencies d
    JOIN project_repos dependent ON d.repo_id = dependent.id
    JOIN project_repos dependency ON d.depends_on_repo_id = dependency.id
"#;

fn row_to_dependency(row: &sqlx::sqlite::SqliteRow) -> Result<RepoDependency> {
    Ok(RepoDependency {
        project_id: row.try_get("project_id").context("missing project_id")?,
        repo_id: row.try_get("repo_id").context("missing repo_id")?,
        repo_name: row.try_get("repo_name").context("missing repo_name")?,
        depends_on_repo_id: row
            .try_get("depends_on_repo_id")
            .context("missing depends_on_repo_id")?,
        depends_on_repo_name: row
            .try_get("depends_on_repo_name")
            .context("missing depends_on_repo_name")?,
        kind: row.try_get("kind").unwrap_or(None),
        note: row.try_get("note").unwrap_or(None),
        created_at: row.try_get("created_at").context("missing created_at")?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::store::{CreateProjectInput, CreateRepoInput, ProjectRepo};

    use serde_json::Value;
    use sqlx::SqlitePool;

    async fn setup_store() -> ProjectStore {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("failed to create in-memory pool");
        sqlx::migrate!("./migrations/global")
            .run(&pool)
            .await
            .expect("failed to run migrations");
        ProjectStore::new(pool)
    }

    async fn project(store: &ProjectStore, root_path: &str) -> String {
        store
            .create_project(CreateProjectInput {
                name: root_path.into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: root_path.into(),
                settings: Value::Object(Default::default()),
            })
            .await
            .expect("failed to create project")
            .id
    }

    async fn repo(store: &ProjectStore, project_id: &str, name: &str) -> ProjectRepo {
        store
            .create_repo(CreateRepoInput {
                project_id: project_id.to_string(),
                name: name.into(),
                path: name.into(),
                remote_url: String::new(),
                default_branch: "main".into(),
                current_branch: None,
                description: String::new(),
            })
            .await
            .expect("failed to create repo")
    }

    async fn declare(
        store: &ProjectStore,
        project_id: &str,
        dependent: &ProjectRepo,
        dependency: &ProjectRepo,
        kind: Option<&str>,
    ) -> std::result::Result<RepoDependency, RepoDependencyError> {
        store
            .declare_repo_dependency(DeclareRepoDependencyInput {
                project_id: project_id.to_string(),
                repo_id: dependent.id.clone(),
                depends_on_repo_id: dependency.id.clone(),
                kind: kind.map(str::to_string),
                note: None,
            })
            .await
    }

    /// A declaration is part of the project, not a side table someone has to
    /// know to ask for. If this regresses the project view loses the arrows and
    /// the relationship is invisible again — which is the entire problem the
    /// table exists to solve.
    #[tokio::test]
    async fn declared_dependency_is_returned_with_the_project() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-with-project").await;
        let api = repo(&store, &project_id, "api").await;
        let web = repo(&store, &project_id, "web").await;

        declare(&store, &project_id, &web, &api, Some("generated_from"))
            .await
            .expect("declaration should be accepted");

        let full = store
            .get_project_with_relations(&project_id)
            .await
            .expect("failed to load project")
            .expect("project not found");

        assert_eq!(full.repo_dependencies.len(), 1);
        let edge = &full.repo_dependencies[0];
        assert_eq!(edge.repo_id, web.id);
        assert_eq!(edge.depends_on_repo_id, api.id);
        // Names come resolved so the view can draw without a second query.
        assert_eq!(edge.repo_name, "web");
        assert_eq!(edge.depends_on_repo_name, "api");
        assert_eq!(edge.kind.as_deref(), Some("generated_from"));
    }

    /// `kind` is free text and stays free text. If a closed vocabulary is ever
    /// introduced, this is the test that will fail, and the failure is the
    /// argument: the second project to use this feature will want a word
    /// nobody thought of.
    #[tokio::test]
    async fn dependency_kind_accepts_a_label_nobody_planned_for() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-free-kind").await;
        let api = repo(&store, &project_id, "api").await;
        let web = repo(&store, &project_id, "web").await;

        let edge = declare(
            &store,
            &project_id,
            &web,
            &api,
            Some("vendors-the-protobufs-by-hand"),
        )
        .await
        .expect("an unplanned label should be accepted");

        assert_eq!(edge.kind.as_deref(), Some("vendors-the-protobufs-by-hand"));
    }

    /// A repo depending on itself is not a statement about anything, and a
    /// self-loop drawn in the project view or offered as a suggestion would be
    /// noise the reader has to dismiss every time.
    #[tokio::test]
    async fn self_dependency_is_refused() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-self").await;
        let api = repo(&store, &project_id, "api").await;

        let error = declare(&store, &project_id, &api, &api, None)
            .await
            .expect_err("a repo must not be allowed to depend on itself");

        assert!(
            matches!(error, RepoDependencyError::SelfDependency { .. }),
            "expected SelfDependency, got {error:?}"
        );
    }

    /// Declarations are scoped to a project because the project view is what
    /// draws them. An edge to a repo in another project would render as an
    /// arrow to a node that is not on the canvas, and would leak the existence
    /// of one project's repos into another's.
    #[tokio::test]
    async fn dependency_on_a_repo_in_another_project_is_refused() {
        let store = setup_store().await;
        let ours = project(&store, "/tmp/rd-ours").await;
        let theirs = project(&store, "/tmp/rd-theirs").await;
        let web = repo(&store, &ours, "web").await;
        let foreign_api = repo(&store, &theirs, "api").await;

        let error = declare(&store, &ours, &web, &foreign_api, None)
            .await
            .expect_err("a cross-project declaration must be refused");

        assert!(
            matches!(error, RepoDependencyError::ForeignRepo { ref repo_id } if *repo_id == foreign_api.id),
            "expected ForeignRepo for the foreign repo, got {error:?}"
        );

        // And the same refusal when the *dependent* side is the foreign one.
        let error = declare(&store, &ours, &foreign_api, &web, None)
            .await
            .expect_err("a cross-project declaration must be refused in both directions");
        assert!(
            matches!(error, RepoDependencyError::ForeignRepo { .. }),
            "expected ForeignRepo, got {error:?}"
        );
    }

    /// Declaring the same edge twice is the same declaration. Silently
    /// accepting it would either duplicate the arrow in the project view or
    /// quietly overwrite a note someone wrote, so it is refused by name and the
    /// caller can choose to update instead.
    #[tokio::test]
    async fn duplicate_declaration_is_refused_and_leaves_the_original_intact() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-dupe").await;
        let api = repo(&store, &project_id, "api").await;
        let web = repo(&store, &project_id, "web").await;

        declare(&store, &project_id, &web, &api, Some("generated_from"))
            .await
            .expect("first declaration should be accepted");

        let error = declare(&store, &project_id, &web, &api, Some("consumes"))
            .await
            .expect_err("the second declaration must be refused");
        assert!(
            matches!(error, RepoDependencyError::Duplicate { .. }),
            "expected Duplicate, got {error:?}"
        );

        // The refusal must not have clobbered what was already there.
        let existing = store
            .list_repo_dependencies(&project_id)
            .await
            .expect("failed to list");
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].kind.as_deref(), Some("generated_from"));
    }

    /// Mutual generation is a real arrangement, not a mistake, and nothing
    /// walks this graph to order execution — so a cycle among declarations is
    /// accepted. If this ever starts failing, something has begun deriving an
    /// ordering from declarations, which is exactly the thing this feature
    /// promises not to do.
    #[tokio::test]
    async fn mutual_dependency_between_two_repos_is_allowed() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-cycle").await;
        let contract = repo(&store, &project_id, "contract").await;
        let server = repo(&store, &project_id, "server").await;

        declare(&store, &project_id, &server, &contract, Some("consumes"))
            .await
            .expect("first direction should be accepted");
        declare(
            &store,
            &project_id,
            &contract,
            &server,
            Some("generated_from"),
        )
        .await
        .expect("the reverse direction is a legitimate statement and must be accepted");

        let edges = store
            .list_repo_dependencies(&project_id)
            .await
            .expect("failed to list");
        assert_eq!(edges.len(), 2);
    }

    /// The suggestion query is the consumer that makes this table more than
    /// decoration. The step editor holds one repo and needs both halves: who
    /// depends on it (offer a downstream step) and what it depends on. If
    /// either direction regresses, the editor silently stops suggesting and the
    /// declaration goes back to being knowledge nobody is reminded of.
    #[tokio::test]
    async fn suggestion_query_answers_in_both_directions() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-suggest").await;
        let schema = repo(&store, &project_id, "schema").await;
        let api = repo(&store, &project_id, "api").await;
        let web = repo(&store, &project_id, "web").await;

        // web -> api -> schema
        declare(&store, &project_id, &web, &api, Some("generated_from"))
            .await
            .expect("web -> api");
        declare(&store, &project_id, &api, &schema, Some("consumes"))
            .await
            .expect("api -> schema");

        let suggestions = store
            .repo_dependency_suggestions(&api.id)
            .await
            .expect("failed to query suggestions");

        assert_eq!(suggestions.repo_id, api.id);

        // Downstream: editing `api` should surface `web`.
        assert_eq!(suggestions.dependents.len(), 1);
        assert_eq!(suggestions.dependents[0].repo_id, web.id);
        assert_eq!(suggestions.dependents[0].repo_name, "web");
        assert_eq!(suggestions.dependents[0].depends_on_repo_id, api.id);

        // Upstream: `api` itself is declared to consume `schema`.
        assert_eq!(suggestions.dependencies.len(), 1);
        assert_eq!(suggestions.dependencies[0].depends_on_repo_id, schema.id);
        assert_eq!(suggestions.dependencies[0].depends_on_repo_name, "schema");

        // A leaf on one side answers empty rather than erroring.
        let leaf = store
            .repo_dependency_suggestions(&schema.id)
            .await
            .expect("failed to query suggestions for leaf");
        assert_eq!(leaf.dependents.len(), 1);
        assert!(leaf.dependencies.is_empty());
    }

    /// A declaration pointing at a repo that no longer exists is invisible in
    /// the project view (the name JOIN drops it) and confusing everywhere else,
    /// so removing a repo must take its declarations with it — including the
    /// ones that merely *point at* it, which is the half that is easy to miss.
    #[tokio::test]
    async fn deleting_a_repo_removes_declarations_in_both_directions() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-delete").await;
        let api = repo(&store, &project_id, "api").await;
        let web = repo(&store, &project_id, "web").await;
        let docs = repo(&store, &project_id, "docs").await;

        // web depends on api (api is the target), and api depends on docs
        // (api is the source).
        declare(&store, &project_id, &web, &api, Some("generated_from"))
            .await
            .expect("web -> api");
        declare(&store, &project_id, &api, &docs, Some("consumes"))
            .await
            .expect("api -> docs");
        assert_eq!(
            store
                .list_repo_dependencies(&project_id)
                .await
                .expect("failed to list")
                .len(),
            2
        );

        store
            .delete_repo(&api.id)
            .await
            .expect("failed to delete repo");

        let remaining = store
            .list_repo_dependencies(&project_id)
            .await
            .expect("failed to list");
        assert!(
            remaining.is_empty(),
            "both the incoming and the outgoing declaration should be gone, got {remaining:?}"
        );

        // And nothing dangling is reachable through the suggestion query either.
        let suggestions = store
            .repo_dependency_suggestions(&web.id)
            .await
            .expect("failed to query suggestions");
        assert!(suggestions.dependencies.is_empty());
        assert!(suggestions.dependents.is_empty());
    }

    /// Withdrawing a declaration is how a stale one gets fixed, and editing the
    /// label is how a mislabelled one does. Both are the cheap correction that
    /// keeps a suggestion-only table honest.
    #[tokio::test]
    async fn declaration_can_be_relabelled_and_withdrawn() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-update").await;
        let api = repo(&store, &project_id, "api").await;
        let web = repo(&store, &project_id, "web").await;

        declare(&store, &project_id, &web, &api, Some("consumes"))
            .await
            .expect("declaration should be accepted");

        let updated = store
            .update_repo_dependency(
                &project_id,
                &web.id,
                &api.id,
                Some("generated_from"),
                Some("openapi codegen reads api/openapi.json"),
            )
            .await
            .expect("failed to update")
            .expect("declaration should exist");
        assert_eq!(updated.kind.as_deref(), Some("generated_from"));
        assert_eq!(
            updated.note.as_deref(),
            Some("openapi codegen reads api/openapi.json")
        );

        // Updating something that was never declared is a miss, not a create.
        let missing = store
            .update_repo_dependency(&project_id, &api.id, &web.id, Some("consumes"), None)
            .await
            .expect("failed to update");
        assert!(missing.is_none());

        assert!(
            store
                .delete_repo_dependency(&project_id, &web.id, &api.id)
                .await
                .expect("failed to delete")
        );
        assert!(
            store
                .list_repo_dependencies(&project_id)
                .await
                .expect("failed to list")
                .is_empty()
        );
        // Withdrawing twice reports that there was nothing to withdraw.
        assert!(
            !store
                .delete_repo_dependency(&project_id, &web.id, &api.id)
                .await
                .expect("failed to delete")
        );
    }

    /// A declaration naming a repo that does not exist would be a dangling
    /// arrow from the moment it was written, so it is refused at declare time
    /// rather than filtered out at read time.
    #[tokio::test]
    async fn dependency_on_an_unknown_repo_is_refused() {
        let store = setup_store().await;
        let project_id = project(&store, "/tmp/rd-unknown").await;
        let web = repo(&store, &project_id, "web").await;

        let error = store
            .declare_repo_dependency(DeclareRepoDependencyInput {
                project_id: project_id.clone(),
                repo_id: web.id.clone(),
                depends_on_repo_id: "no-such-repo".into(),
                kind: None,
                note: None,
            })
            .await
            .expect_err("an unknown repo must be refused");

        assert!(
            matches!(error, RepoDependencyError::UnknownRepo { ref repo_id } if repo_id == "no-such-repo"),
            "expected UnknownRepo, got {error:?}"
        );
    }
}
