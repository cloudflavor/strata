# Skygen – Rust OpenAPI v3 SDK Generator

Skygen is a Rust command line tool that turns an OpenAPI v3 specification into an idiomatic Rust SDK. It can generate SDKs for Cloudflare, DigitalOcean, Exoscale, Hetzner, Scaleway and any provider that publishes an OpenAPI spec.

## Installation

```bash
cargo install --path ./skygen
```

## Usage

```bash
skygen generate --schema <schema-file> --output-dir <output-directory> --config <config-file>
```

The CLI supports different log levels for debugging:

```bash
skygen --log-level debug generate --schema openapi.yaml --config config.toml --output-dir ./output
```

Supported log levels: `trace`, `debug`, `info`, `warn`, `error` (default: `info`)

## Configuration

Skygen uses a TOML configuration file to customize the generated SDK. Here's an example configuration:

```toml
crate_name = "cloudflare"
version = "0.1.0-alpha"
edition = "2021"  # Optional, defaults to "2021"
description = "Unofficial Rust SDK for Cloudflare"
lib_status = "experimental"  # Can be: active, deprecated, experimental
keywords = ["cloudflare", "api", "sdk"]
api_url = "https://api.cloudflare.com/client/v4"
authors = ["Cloudflavor GmbH <foss@cloudflavor.io>"]

# Optional filtering
include_only = ["User", "Account"]  # Only include specified models
exclude = ["InternalModel"]         # Exclude specific models
```

### Configuration Options

- `crate_name`: The name of the generated Rust crate
- `version`: The version of the generated crate
- `edition`: The Rust edition to use (defaults to "2021")
- `description`: A brief description of the crate
- `lib_status`: The maintenance status of the library (e.g., "active", "deprecated", "experimental")
- `keywords`: A list of keywords for the crate
- `api_url`: The base URL for the API
- `authors`: A list of authors for the crate
- `include_only`: (Optional) A list of models to include (if not specified, all models are included)
- `exclude`: (Optional) A list of models to exclude

## Generating the SDK

```bash
skygen generate \
  --schema path/to/openapi.yaml \
  --config config.toml \
  --output-dir ./output
```

The output will contain a Cargo crate with a typical `src/` layout and a `Cargo.toml` that exposes optional features like `rustls`, `native-tls` and `http2`.

## Generated Structure

The generated SDK includes:
- `src/lib.rs`: Main library entry point with prelude
- `src/client.rs`: HTTP client implementation with request/response handling
- `src/errors.rs`: Comprehensive error types and handling
- `src/models/`: Generated model types with proper module organization
- `src/models/mod.rs`: Models module with organized re-exports
- `src/apis/`: Generated API operation functions grouped by resource
- `Cargo.toml`: Crate metadata with feature flags

## Project Structure

```
src/
├─ lib.rs              # Main entry point
├─ client.rs           # HTTP client
├─ errors.rs           # Error types
├─ models/
│  ├─ mod.rs           # Models module
│  ├─ model1.rs        # Individual models
│  └─ model2.rs
└─ apis/
   ├─ resource1.rs     # API operations
   └─ resource2.rs
```

## Features

- **Full OpenAPI v3 specification support** with comprehensive schema handling
- **Advanced model generation** with support for complex nested structures
- **Composite type handling** with intelligent variant naming for oneOf/anyOf/allOf
- **Improved operation generation** with better naming and organization
- **Enhanced error handling** with comprehensive error types
- **Intelligent name collision resolution** using field-based disambiguation
- **Modular code organization** with dedicated models module
- **HTTP client** with flexible request/response handling
- **Feature flags** for different TLS backends (rustls, native-tls) and HTTP/2
- **Automatic code formatting** with `cargo fmt`
- **Debug logging** for troubleshooting complex API specifications

## Advanced Features

- **Inline model hoisting**: Automatic extraction of inline schemas into proper model definitions
- **Field-based disambiguation**: Intelligent naming of similar models based on their field differences
- **Operation grouping**: Logical organization of API operations by resource type
- **Response enum generation**: Proper Rust enums for different API response types
- **Parameter handling**: Comprehensive support for path, query, header, and body parameters

## Contributing

See `CONTRIBUTING.md` for details.

## Troubleshooting

### Name Collision Errors

For complex APIs with many similar models (like Cloudflare), you might encounter name collision errors:

```
thread 'main' panicked at 'inline model name collision for 'model_name': no unique fields to disambiguate; rename schema'
```

**Solutions:**

1. **Use `--log-level debug`** to see detailed information about the collision:
   ```bash
   skygen --log-level debug generate --schema openapi.json --config config.toml --output-dir ./output
   ```

2. **Simplify the API spec** by removing unused or duplicate models

3. **Use `include_only`** in config to generate only specific models:
   ```toml
   include_only = ["User", "Account", "Product"]
   ```

4. **Use `exclude`** to skip problematic models:
   ```toml
   exclude = ["ProblemModel1", "ProblemModel2"]
   ```

### Large API Specifications

For very large specs (10MB+), generation may take significant time and memory. Consider:
- Using `include_only` to generate subsets of the API
- Breaking the API into multiple smaller SDKs
- Increasing system memory limits

## License

Apache‑2.0 – see the `LICENSE` file.