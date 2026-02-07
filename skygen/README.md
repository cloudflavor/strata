# Skygen

Skygen is a tool that generates Rust SDKs for cloud providers from their OpenAPI v3 specifications.

## Usage

```bash
skygen generate --schema <schema-file> --output-dir <output-directory> --config <config-file>
```

## Configuration

Skygen uses a TOML configuration file to customize the generated SDK. Here's an example configuration:

```toml
crate_name = "my-cloud-sdk"
version = "0.1.0"
edition = "2021"
description = "A Rust SDK for My Cloud Provider API"
lib_status = "active"
keywords = ["cloud", "api", "sdk"]
api_url = "https://api.mycloudprovider.com"
authors = ["Cloudflavor GmbH <foss@cloudflavor.io>"]
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

## Generated Structure

The generated SDK includes:
- `src/lib.rs`: Main library entry point
- `src/client.rs`: HTTP client implementation
- `src/errors.rs`: Error types
- `src/models/`: Generated model types
- `src/apis/`: Generated API operation functions
- `Cargo.toml`: Crate metadata and dependencies

## Features

- Full OpenAPI v3 specification support
- Automatic model generation from schemas
- HTTP client with request/response handling
- Error type generation
- Support for complex schema types (arrays, objects, references, oneOf, anyOf, allOf)
- Rust code formatting with `cargo fmt`

