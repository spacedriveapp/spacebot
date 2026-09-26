# src/secrets/

Encrypted credential storage and output scrubbing.

## Files

- `store.rs` (1645 lines) — `SecretsStore`: encrypted redb-backed credential store
- `keystore.rs` (~500 lines) — `KeyStore` trait + OS keyring implementations
- `scrub.rs` — output scrubbing for worker streams

## SecretsStore (`store.rs`)

Wraps redb. All values encrypted at rest with AES-256-GCM. Master key derived via Argon2id.

**Two categories:**
- `system` — never exposed to subprocesses (LLM API keys, webhook tokens)
- `tool` — injected as env vars into worker shell environments

## KeyStore (`keystore.rs`)

Trait with 3 implementations:

- `MacOSKeyStore` — macOS Keychain via Security framework
- `LinuxKeyStore` — kernel keyring via libsecret FFI
- `NoopKeyStore` — fallback (dev/test only, warns loudly)

Master key lives in the OS credential store. Never written to disk. `keystore.rs` contains 4+ `unsafe` blocks — all justified by libsecret FFI requirements for Linux keyring access.

## Scrubber (`scrub.rs`)

Redacts secret values from worker output before it reaches channels or LLM context.

- Rolling buffer handles secrets split across stream chunk boundaries
- Applied to all worker stdout/stderr streams
- Replacement: `[REDACTED]`

## SystemSecrets Trait

Marker trait for config sections that contain credentials: `LlmConfig`, `DiscordConfig`, `TelegramConfig`, etc. Used to enforce scrubbing at config load time.
