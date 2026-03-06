# Strata - Rust OpenAPI v3 tooling

Strata is a Rust workspace that hosts the Nokturn components: a core OpenAPI v3 loader/resolver and the `nokturn-gen` experimental SDK generator CLI.

## Workspace layout

- `crates/nokturn-core`: Loads OpenAPI v3 (JSON/YAML), applies a small sanitization pass, resolves component references, and defines the canonical IR for code generation.
- `crates/nokturn-gen`: CLI wrapper (binary name: `nokturn-gen`) that reads the config/spec, resolves the schema, and is the home for upcoming SDK generation.

## Status

Code generation is in progress. Today `nokturn-gen generate` validates the config and resolves the OpenAPI spec, but does not yet emit SDK source files.

## Build

```bash
cargo build --manifest-path crates/Cargo.toml -p nokturn-gen
```

## Usage

```bash
cargo run --manifest-path crates/Cargo.toml -p nokturn-gen -- \
  --log-level debug \
  generate \
  --schema open-api-specs/digitalocean/digitalocean.yaml \
  --config open-api-specs/digitalocean/config.toml \
  --output-dir generated/digitalocean
```

You can also run the binary directly after a release build:

```bash
(cd crates && cargo build --release)
./crates/target/release/nokturn-gen --log-level info generate \
  --schema open-api-specs/digitalocean/digitalocean.yaml \
  --config open-api-specs/digitalocean/config.toml \
  --output-dir generated/digitalocean
```

Supported log levels: `trace`, `debug`, `info`, `warn`, `error`.

## Configuration

`nokturn-gen` expects a TOML config with the following fields:

```toml
crate_name = "digitalocean"
version = "0.1.0-alpha"
edition = "2024" # optional
description = "Unofficial Rust SDK for DigitalOcean"
lib_status = "experimental"
keywords = ["digitalocean"]
api_url = "https://api.digitalocean.com/v2"
authors = ["Cloudflavor GmbH <foss@cloudflavor.io>"]

# Optional filtering
include_only = ["User", "Account"]
exclude = ["InternalModel"]
```

In-tree examples live under `open-api-specs/*/config.toml`.

## License

Apache-2.0 - see the `LICENSE` file.
