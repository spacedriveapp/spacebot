# src/config/

Configuration subsystem. Loads, validates, and hot-reloads all runtime config.

## Files

**types.rs** — All config structs. `Config` is the root; subsystems get their own typed structs (`AgentConfig`, `LlmConfig`, `RoutingConfig`, `CompactionConfig`, `MemoryPersistenceConfig`, `SandboxConfig`, `ProviderConfig`, etc.). ~50 structs, each with `impl` blocks containing `resolve()` methods. Critical hotspot.

**load.rs** — Config loading pipeline: read `config.toml`, apply env var overrides, resolve secret references, validate. Secret references use the `secret:NAME` pattern and are resolved against the secret store at load time. Critical hotspot.

**toml_schema.rs** — TOML schema definitions used during validation. Separate from `types.rs` so the wire format can diverge from the runtime types.

**runtime.rs** — `RuntimeConfig`: the live snapshot all processes read. Wrapped in `Arc<ArcSwap<RuntimeConfig>>` for lock-free access. Readers call `.load()` to get the current snapshot; no locks, no blocking.

**watcher.rs** — File watcher. Detects changes to `config.toml` and triggers a reload cycle: re-parse, re-validate, swap the `ArcSwap` pointer. Failed reloads leave the previous config in place.

**permissions.rs** — Per-adapter, per-channel access control structs (`DiscordPermissions`, `TelegramPermissions`, `SlackPermissions`, `TwitchPermissions`). Hot-reloadable alongside the rest of config.

## Resolution Order

```
env vars  >  config.toml  >  compiled defaults
```

Each subsystem's `resolve()` method applies this order. Env vars win unconditionally. Missing TOML keys fall back to defaults baked into the `Default` impls.

## Hot-Reload Pattern

```rust
// At startup
let runtime_config: Arc<ArcSwap<RuntimeConfig>> = Arc::new(ArcSwap::new(Arc::new(initial)));

// Every reader (channel, worker, cortex)
let config = runtime_config.load();

// On file change (watcher.rs)
runtime_config.store(Arc::new(new_config));
```

Readers always see a consistent snapshot. The store is atomic. No reader ever blocks on a reload.

## Secret References

Config values can reference the secret store:

```toml
api_key = "secret:OPENAI_KEY"
```

Resolved at load time in `load.rs`. The resolved value is a `DecryptedSecret` wrapper that redacts in `Debug`/`Display`. Never logged, never serialized back to disk.

## Adding Config

1. Add struct + fields to `types.rs` with a `Default` impl.
2. Add TOML schema entry in `toml_schema.rs`.
3. Add env var handling in `load.rs`.
4. Expose via `RuntimeConfig` in `runtime.rs` if processes need live access.
