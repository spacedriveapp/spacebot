# OpenAPI Migration

Port the API from hand-wired axum routes with manually duplicated TypeScript types to a utoipa-annotated OpenAPI spec with generated TypeScript types and a typed client. Reference implementation: `../spacedrive-data`.

## Problem

Every API type is manually maintained in both Rust (`src/api/*.rs`, ~200 structs) and TypeScript (`interface/src/api/client.ts`, 211 interfaces, 2,643 lines). The TS client has 123 hand-written `fetch()` methods. Types already drift — `BindingInfo` has fields in TS that don't exist in Rust, config sections exist in Rust but are missing from TS. There is no compile-time or CI-time check that frontend types match backend types.

## Target State

- Rust types derive `ToSchema`, handlers annotated with `#[utoipa::path]`
- `OpenApiRouter` replaces `axum::Router` for API routes
- Single `cargo run --bin openapi-spec` binary dumps the JSON spec
- `openapi-typescript` generates `api-schema.d.ts` from the spec
- `openapi-fetch` provides a fully typed HTTP client
- Swagger UI served at `/api/docs`
- `just typegen` runs the full pipeline
- `just gate-pr` validates generated types are up to date
- All 211 manually-written TS interfaces deleted

## Stack

| Layer | Crate / Package | Version |
|---|---|---|
| Rust OpenAPI macros | `utoipa` | 5 |
| Axum integration | `utoipa-axum` | 0.2 |
| Swagger UI | `utoipa-swagger-ui` | 9 (feature `axum`) |
| TS type codegen | `openapi-typescript` | ^7 (devDependency) |
| TS typed client | `openapi-fetch` | ^0.17 (dependency) |

## Execution Plan

This is structured for a fast coding agent to execute top-to-bottom in one pass. Each phase has concrete file lists, exact code patterns, and a verification command. Do not skip phases or reorder — later phases depend on earlier ones compiling.

---

### Phase 1: Dependencies and Scaffolding

**Add Rust dependencies** to `Cargo.toml` in the `# HTTP server for control UI` section:

```toml
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid"] }
utoipa-axum = "0.2"
utoipa-swagger-ui = { version = "9", features = ["axum"] }
```

**Add TS dependencies** in `interface/`:

```sh
bun add openapi-fetch@^0.17
bun add -d openapi-typescript@^7
```

**Create the spec binary** at `src/bin/openapi_spec.rs`:

```rust
fn main() {
    print!("{}", spacebot::openapi_json());
}
```

Add to `Cargo.toml`:

```toml
[[bin]]
name = "openapi-spec"
path = "src/bin/openapi_spec.rs"
```

**Add `openapi_json()` to `src/lib.rs`:**

```rust
pub fn openapi_json() -> String {
    let (_, api) = crate::api::server::api_router()
        .split_for_parts();
    api.to_json().expect("failed to serialize OpenAPI spec")
}
```

This requires `api_router()` to be extracted as a public function (done in Phase 2).

**Add typegen scripts** to `justfile`:

```just
typegen:
    cargo run --bin openapi-spec > /tmp/spacebot-openapi.json
    cd interface && bunx openapi-typescript /tmp/spacebot-openapi.json -o src/api/schema.d.ts

check-typegen:
    cargo run --bin openapi-spec > /tmp/spacebot-openapi-check.json
    cd interface && bunx openapi-typescript /tmp/spacebot-openapi-check.json -o /tmp/spacebot-schema-check.d.ts
    diff interface/src/api/schema.d.ts /tmp/spacebot-schema-check.d.ts
```

Add `check-typegen` as a step in the `gate-pr` recipe.

**Verify:** `cargo check` passes (new deps resolve, binary stub compiles — `openapi_json` won't exist yet, that's fine, it will error).

---

### Phase 2: Router Conversion

Convert `src/api/server.rs` to use `OpenApiRouter` instead of `axum::Router` for API routes.

**Extract `api_router()`** as a public function returning `OpenApiRouter<Arc<ApiState>>`. This is the function that `openapi_json()` calls.

**Key changes in `server.rs`:**

```rust
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(info(
    title = "Spacebot API",
    version = env!("CARGO_PKG_VERSION"),
    description = "Spacebot agent system API"
))]
struct ApiDoc;

pub fn api_router() -> OpenApiRouter<Arc<ApiState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(system::health))
        .routes(routes!(system::status))
        // ... all routes converted
}
```

**Route conversion pattern.** Each `.route()` call becomes `.routes(routes!(...))`:

```rust
// Before:
.route("/health", get(system::health))

// After:
.routes(routes!(system::health))
```

For routes with multiple methods on the same path:

```rust
// Before:
.route("/agents", get(agents::list_agents).post(agents::create_agent).put(agents::update_agent).delete(agents::delete_agent))

// After:
.routes(routes!(agents::list_agents, agents::create_agent, agents::update_agent, agents::delete_agent))
```

**In `start_http_server`**, split the OpenApiRouter into parts and mount Swagger UI:

```rust
let (api_routes, api) = api_router().split_for_parts();

let api_routes = api_routes
    .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
    .layer(middleware::from_fn_with_state(state.clone(), api_auth_middleware));

let app = Router::new()
    .merge(
        utoipa_swagger_ui::SwaggerUi::new("/api/docs")
            .url("/api/openapi.json", api),
    )
    .nest("/api", api_routes)
    .fallback(static_handler)
    .layer(cors)
    .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
    .with_state(state);
```

**Special cases:**

- The opencode proxy (`any()` routes) stays as a plain `.route()` on the base `Router` — it's an untyped passthrough, not part of the API spec.
- SSE endpoints (`events_sse`, `cortex_chat_send`) can be registered with `routes!()` but their streaming responses won't auto-document. Add manual `responses()` annotations describing the SSE content type.
- The `.nest("/api", ...)` call uses the plain `Router` from `split_for_parts()`, so auth middleware and CORS apply normally.

**Verify:** `cargo check` passes. At this point handlers won't have `#[utoipa::path]` annotations yet so the spec will be empty — that's expected. The important thing is the router compiles.

---

### Phase 3: Annotate Types with `ToSchema`

Add `#[derive(utoipa::ToSchema)]` to every request/response struct across all 27 handler files. This is mechanical — find every struct with `#[derive(Serialize)]` or `#[derive(Deserialize)]` in `src/api/` and add `ToSchema`.

**Pattern:**

```rust
// Before:
#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
}

// After:
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct HealthResponse {
    status: &'static str,
}
```

For `Deserialize` request bodies:

```rust
// Before:
#[derive(Deserialize)]
pub(super) struct CreateCronRequest { ... }

// After:
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateCronRequest { ... }
```

**Files to annotate** (every file in `src/api/` that defines structs):

| File | Approx types |
|---|---|
| `system.rs` | 4 |
| `agents.rs` | 8 |
| `channels.rs` | 6 |
| `workers.rs` | 4 |
| `memories.rs` | 4 |
| `cortex.rs` | 6 |
| `config.rs` | 24 |
| `cron.rs` | 5 |
| `providers.rs` | 6 |
| `models.rs` | 3 |
| `messaging.rs` | 4 |
| `bindings.rs` | 6 |
| `settings.rs` | 8 |
| `secrets.rs` | 8 |
| `tasks.rs` | 6 |
| `skills.rs` | 6 |
| `projects.rs` | 10 |
| `links.rs` | 10 |
| `webchat.rs` | 3 |
| `ssh.rs` | 2 |
| `factory.rs` | 2 |
| `tools.rs` | 2 |
| `ingest.rs` | 3 |
| `mcp.rs` | 4 |
| `state.rs` | 5 |

**Types from outside `src/api/`** (used in API responses):

Some handler responses embed types from the core domain (e.g. `Memory`, `MemoryType`, `RelationType` from `src/memory/types.rs`, `CronConfig` from `src/cron/`, `ProcessEvent` variants from `src/lib.rs`). For these, use conditional derives behind a feature flag:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Memory { ... }
```

Add the feature to `Cargo.toml`:

```toml
[features]
metrics = ["dep:prometheus"]
openapi = ["dep:utoipa"]
```

And make utoipa optional:

```toml
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid"], optional = true }
```

The `openapi-spec` binary enables the feature:

```toml
[[bin]]
name = "openapi-spec"
path = "src/bin/openapi_spec.rs"
required-features = ["openapi"]
```

Note: `utoipa-axum` and `utoipa-swagger-ui` remain non-optional since the router uses `OpenApiRouter` unconditionally. Only `utoipa` (the derive macro) is behind the feature for core types. Alternatively, keep everything non-optional — simpler and the compile cost is negligible. Choose one approach and be consistent.

**Verify:** `cargo check` (and `cargo check --features openapi` if using feature flag). All types compile with `ToSchema`.

---

### Phase 4: Annotate Handlers

Add `#[utoipa::path]` to every handler function. This is the largest phase by line count.

**Pattern for GET handlers:**

```rust
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, body = HealthResponse),
    ),
    tag = "system",
)]
pub(super) async fn health() -> Json<HealthResponse> { ... }
```

**Pattern for POST/PUT with request body:**

```rust
#[utoipa::path(
    post,
    path = "/agents",
    request_body = CreateAgentRequest,
    responses(
        (status = 200, body = AgentInfo),
        (status = 400, description = "Invalid request"),
    ),
    tag = "agents",
)]
pub(super) async fn create_agent(...) -> ... { ... }
```

**Pattern for query parameters:**

```rust
#[utoipa::path(
    get,
    path = "/channels/messages",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("channel_id" = String, Query, description = "Channel ID"),
    ),
    responses(
        (status = 200, body = MessagesResponse),
    ),
    tag = "channels",
)]
```

**Pattern for path parameters:**

```rust
#[utoipa::path(
    delete,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, description = "Deleted"),
    ),
    tag = "tasks",
)]
```

**Pattern for SSE endpoints:**

```rust
#[utoipa::path(
    get,
    path = "/events",
    responses(
        (status = 200, description = "SSE event stream", content_type = "text/event-stream"),
    ),
    tag = "system",
)]
```

**Tags** — group by file/domain: `system`, `agents`, `channels`, `workers`, `memories`, `cortex`, `config`, `cron`, `tasks`, `projects`, `providers`, `models`, `messaging`, `bindings`, `settings`, `secrets`, `skills`, `links`, `webchat`, `ssh`, `factory`, `tools`, `ingest`, `mcp`.

**Skip:** The opencode proxy handler — it's a raw passthrough, not part of the API spec.

**Verify:** `cargo run --bin openapi-spec | python3 -m json.tool > /dev/null` — the spec generates valid JSON. Spot check that the endpoint count matches (should be ~124, excluding the opencode proxy).

---

### Phase 5: Generate TypeScript and Replace Client

**Run typegen:**

```sh
just typegen
```

This produces `interface/src/api/schema.d.ts`.

**Create the typed client** at `interface/src/api/client-typed.ts`:

```typescript
import createClient from "openapi-fetch";
import type { paths } from "./schema";

let baseUrl = "";

export function setServerUrl(url: string) {
    baseUrl = url;
}

function getClient() {
    return createClient<paths>({
        baseUrl: baseUrl ? `${baseUrl}/api` : "/api",
        headers: getAuthHeaders(),
    });
}

function getAuthHeaders(): Record<string, string> {
    const token = localStorage.getItem("spacebot_auth_token");
    return token ? { Authorization: `Bearer ${token}` } : {};
}
```

**Create type re-exports** at `interface/src/api/types.ts`:

```typescript
import type { components } from "./schema";

export type HealthResponse = components["schemas"]["HealthResponse"];
export type StatusResponse = components["schemas"]["StatusResponse"];
export type AgentInfo = components["schemas"]["AgentInfo"];
// ... every schema type gets a friendly re-export
```

**Migration strategy for consumers** — do NOT rewrite all 40+ consuming files in one shot. Instead:

1. Create `client-typed.ts` alongside the existing `client.ts`
2. Re-export all existing type names from `types.ts` so imports like `import { AgentInfo } from "@/api/client"` can be redirected to `import { AgentInfo } from "@/api/types"` or kept as re-exports from the old path
3. In `client.ts`, replace the 211 manually-written interfaces with re-exports from `types.ts`:
   ```typescript
   // Replace all manual interface definitions with:
   export type { HealthResponse, StatusResponse, AgentInfo, ... } from "./types";
   ```
4. Migrate `api` object methods one by one to use `openapi-fetch` — or leave the `fetch()` wrappers in place initially and just kill the manual types. The typed client can be adopted incrementally per-route.

This way the 40+ files importing from `@/api/client` don't need to change in this PR.

**Verify:** `bunx tsc --noEmit` — no type errors from the generated types.

---

### Phase 6: CI Gate

Add `check-typegen` to the `gate-pr` script / `just gate-pr` recipe. This runs the typegen pipeline and diffs the output against the committed `schema.d.ts`. If they differ, the gate fails — meaning someone changed a Rust API type without regenerating the TS types.

**Verify:** `just gate-pr` passes.

---

## Files Changed Summary

| Area | Files | Nature |
|---|---|---|
| `Cargo.toml` | 1 | Add utoipa deps, openapi-spec binary |
| `src/bin/openapi_spec.rs` | 1 | New binary |
| `src/lib.rs` | 1 | Add `openapi_json()` |
| `src/api/server.rs` | 1 | Router → OpenApiRouter, Swagger UI mount |
| `src/api/*.rs` (27 files) | 27 | Add `ToSchema` + `#[utoipa::path]` to all types and handlers |
| Core domain types | ~5 | Add conditional `ToSchema` derives |
| `interface/package.json` | 1 | Add openapi-fetch, openapi-typescript |
| `interface/src/api/schema.d.ts` | 1 | Generated (do not edit) |
| `interface/src/api/types.ts` | 1 | Friendly re-exports |
| `interface/src/api/client.ts` | 1 | Delete 211 manual interfaces, re-export from types.ts |
| `justfile` | 1 | Add typegen + check-typegen recipes |
| `scripts/gate-pr.sh` | 1 | Add check-typegen step |

## Risks

- **SSE endpoints** don't map cleanly to OpenAPI request/response schemas. Document them with manual `content_type = "text/event-stream"` annotations and keep the SSE event type definitions as manual TS types (or use a discriminated union in the spec).
- **Multipart uploads** (webchat audio, ingest files, avatar upload) need utoipa's `MultipartForm` derive support. Test these endpoints specifically.
- **`serde_json::Value`** fields (used in config, MCP, etc.) generate `type: object` in OpenAPI which maps to `Record<string, unknown>` in TS. This is correct but less precise than the current hand-written TS types that sometimes use narrower types.
- **Response enums** — some handlers return different shapes depending on query params. These need `#[serde(untagged)]` enum wrappers with `ToSchema` derives to represent properly in the spec.
- **The opencode proxy** is excluded from the spec entirely — it's a raw HTTP passthrough.

## Not In Scope

- Migrating all 40+ consumer files to use `openapi-fetch` directly (keep existing `fetch()` wrappers, just delete the manual types)
- Request validation via OpenAPI schema (utoipa is spec generation only, not validation)
- API versioning
