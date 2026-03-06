# Repository Guidelines

## Project Structure & Module Organization

- `crates/` is the Rust workspace root.
  - `crates/nokturn-core/`: core OpenAPI v3 loader/resolver and IR types.
  - `crates/nokturn-gen/`: CLI binary (`nokturn-gen`) plus generator assets in `crates/nokturn-gen/assets/`.
- `open-api-specs/`: in-repo sample specs and config (e.g., `open-api-specs/digitalocean/`).
- `generated/`: output directory for SDK generation (kept empty unless explicitly asked to commit artifacts).
- `scripts/`: helper scripts (e.g., `scripts/generate.sh`).
- `config-example.toml`: example config template for generator inputs.

## Build, Test, and Development Commands

- Build the CLI:
  - `cargo build --manifest-path crates/Cargo.toml -p nokturn-gen`
- Run the CLI (example):
  - `cargo run --manifest-path crates/Cargo.toml -p nokturn-gen -- \
    --log-level debug generate --schema open-api-specs/digitalocean/digitalocean.yaml \
    --config open-api-specs/digitalocean/config.toml --output-dir generated/digitalocean`
- Run all tests:
  - `cargo test --manifest-path crates/Cargo.toml`

## Coding Style & Naming Conventions

- Rust formatting: follow `rustfmt` defaults (run `cargo fmt` if available).
- Naming: Rust modules/functions in `snake_case`, types in `PascalCase`, crates in `kebab-case` (e.g., `nokturn-gen`).
- Keep public APIs documented in-place with concise Rustdoc.

## Testing Guidelines

- Framework: Rust’s built-in test harness (`#[test]`).
- Tests are currently co-located with modules (e.g., `crates/nokturn-core/src/lib.rs`).
- Add tests near the code they exercise and run `cargo test --manifest-path crates/Cargo.toml` before opening a PR.

## Commit & Pull Request Guidelines

- Commit messages follow a concise scope prefix pattern seen in history:
  - `scope: short description` (e.g., `resolver: additional openapi v3 types`).
- PRs should include:
  - Clear description of intent and impact.
  - Linked issue or context when available.
  - Notes about schema/config changes or new generation behavior.

## Security & Configuration Tips

- Do not commit secrets or real API tokens in `open-api-specs/` or configs.
- Prefer adding new example configs under `open-api-specs/<provider>/config.toml`.
