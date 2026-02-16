// Copyright 2025 Cloudflavor GmbH

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at

// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{bail, Context};
use skygen::generator::model::ModelGenerator;
use skygen::generator::operation::{OperationGenerator, OperationDef};
use skygen::generator::project::{bootstrap_lib, format_crate};
use skygen::ollama::{DocumentationPromptBuilder, OllamaClient};
use skygen::resolver::resolve::Resolver;
use openapiv3::{Operation, PathItem, ReferenceOr};
use structopt::StructOpt;
use tokio::fs;
use tracing_subscriber::EnvFilter;

/// Find the original OpenAPI operation that corresponds to a generated operation ID
fn find_original_operation(resolved: &openapiv3::OpenAPI, operation_id: &str) -> Option<&Operation> {
    // Search through all paths and methods to find the matching operation
    for (path, path_item) in &resolved.paths.paths {
        let path_item = match path_item {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { .. } => continue,
        };

        // Check each HTTP method
        if let Some(op) = path_item.get.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.put.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.post.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.delete.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.patch.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.head.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.options.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
        if let Some(op) = path_item.trace.as_ref() {
            if op.operation_id.as_deref() == Some(operation_id) {
                return Some(op);
            }
        }
    }
    None
}

async fn generate_operation_documentation(
    ollama_client: &OllamaClient,
    model: &str,
    op: &mut OperationDef,
    original_op: &openapiv3::Operation,
) -> anyhow::Result<()> {
    tracing::info!("🔍 Generating documentation for operation: {} ({})", op.name, op.id);
    
    // Collect operation details for the prompt
    let parameters: Vec<(String, String, String)> = op.params
        .iter()
        .map(|param| {
            let desc = param.description.clone().unwrap_or_else(|| "No description available".to_string());
            tracing::debug!("  Parameter: {} ({}) - {}", param.name, param.render_type, desc);
            (param.name.clone(), param.render_type.clone(), desc)
        })
        .collect();

    let response_types: Vec<(String, String, String)> = op.responses
        .iter()
        .map(|resp| {
            let status = match resp.status {
                Some(ref status) => format!("{}", status),
                None => "default".to_string(),
            };
            let desc = format!("Response type for status {}", status);
            tracing::debug!("  Response: {} - {}", status, resp.render_type);
            (status, resp.render_type.clone(), desc)
        })
        .collect();

    let examples = format!(
        "```rust\n// Example usage:\nlet client = Client::new();\nlet response = client.{}({});\n```",
        op.name,
        if parameters.is_empty() { "" } else { "..." }
    );

    // Include relevant OpenAPI spec information
    let openapi_spec = format!(
        "path: {}\nmethod: {}\noperationId: {}\ntags: {:?}\ndeprecated: {:?}\nsummary: {:?}\ndescription: {:?}",
        op.path,
        op.method,
        original_op.operation_id.as_deref().unwrap_or("none"),
        original_op.tags,
        original_op.deprecated,
        original_op.summary,
        original_op.description
    );

    tracing::debug!("OpenAPI spec for this operation:\n{}", openapi_spec);

    // Build the prompt with comprehensive OpenAPI information
    let prompt = DocumentationPromptBuilder::build_operation_prompt(
        &op.name,
        &op.description.unwrap_or_else(|| "No description available".to_string()),
        &parameters,
        &response_types,
        &examples,
        &openapi_spec,
    );

    tracing::info!("📖 Sending prompt to Ollama...");
    
    // Generate documentation using Ollama
    let documentation = ollama_client
        .generate_documentation(model, &prompt)
        .await
        .with_context(|| format!("Failed to generate documentation for operation {}", op.name))?;

    // Store the generated documentation
    op.documentation = Some(documentation.clone());

    tracing::info!("✅ Successfully generated documentation for operation: {}", op.name);
    tracing::debug!("Documentation preview (first 200 chars):\n{}", 
        if documentation.len() > 200 {
            format!("{}...", &documentation[..200])
        } else {
            documentation
        }
    );
    
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = skygen::Cli::from_args();

    let opts_level = opts.log_level;
    let env_filter = EnvFilter::new(opts_level.as_str());

    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_ansi(true)
        .with_env_filter(env_filter)
        .with_writer(std::io::stdout)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    match opts.commands {
        skygen::Commands::Generate(args) => {
            let extension = &args
                .schema
                .extension()
                .and_then(|s| s.to_str())
                .with_context(|| "failed to parse schema file extension")?;

            let c = fs::read_to_string(&args.config).await?;
            let config: skygen::Config = toml::from_str(c.as_str())?;
            let d = fs::read_to_string(&args.schema).await?;
            // NOTE: This surfaces in the DigitalOcean OpenAPI spec where the assigned int
            // overflows what the deserializer can handle so we clamp it to u64::MAX;
            // If sanitization becomes a habbit in the other specs, move this to a fn
            let safe_data = d.replace("18446744073709552000", "18446744073709551615");

            let schema_json: serde_json::Value = match extension.to_lowercase().as_str() {
                "yaml" | "yml" => serde_yaml::from_str(safe_data.as_str())
                    .with_context(|| "failed to parse YAML")?,
                "json" => serde_json::from_str(safe_data.as_str())
                    .with_context(|| "failed to parse json")?,
                _ => bail!("unsupported file extension: {extension}"),
            };

            let spec: openapiv3::OpenAPI = serde_json::from_value(schema_json)
                .with_context(|| "failed to conver into OpenAPIv3 spec")?;

            let resolved = Resolver::new(spec)
                .resolve()
                .with_context(|| "failed to resolve schema")?;

            let mut generator = ModelGenerator::new();
            let mut registry = generator
                .collect_models(&resolved)
                .with_context(|| "failed to collect models")?;

            let op_generator = OperationGenerator::new();
            let mut ops = op_generator
                .collect_operations(&resolved, &mut registry)
                .with_context(|| "failed to collect operations")?;

            // Generate documentation using Ollama if model is specified
            if let Some(ollama_model) = &args.ollama_model {
                tracing::info!("🤖 Generating documentation using Ollama model: {}", ollama_model);
                
                let ollama_client = OllamaClient::new(None);
                
                // Check if Ollama is available
                if ollama_client.check_availability().await.unwrap_or(false) {
                    tracing::info!("✅ Ollama server is available, starting documentation generation...");
                    
                    let total_ops = ops.len();
                    tracing::info!("📚 Processing {} operations for documentation...", total_ops);
                    
                    let mut success_count = 0;
                    let mut fail_count = 0;
                    
                    // We need to reconstruct the mapping from operation IDs to original OpenAPI operations
                    // This requires accessing the resolved spec
                    for (index, op) in ops.iter_mut().enumerate() {
                        tracing::info!("📝 Processing operation {}/{}: {}", index + 1, total_ops, op.name);
                        
                        // Find the original operation in the resolved spec
                        if let Some(original_op) = find_original_operation(&resolved, &op.id) {
                            if let Err(e) = generate_operation_documentation(&ollama_client, ollama_model, op, original_op).await {
                                tracing::error!("❌ Failed to generate documentation for operation {}: {}", op.name, e);
                                fail_count += 1;
                            } else {
                                success_count += 1;
                            }
                        } else {
                            tracing::warn!("⚠️  Could not find original operation for {}, using fallback", op.id);
                            // Still try to generate documentation with limited info
                            if let Err(e) = generate_operation_documentation(&ollama_client, ollama_model, op, &openapiv3::Operation::default()).await {
                                tracing::error!("❌ Failed to generate documentation for operation {}: {}", op.name, e);
                                fail_count += 1;
                            } else {
                                success_count += 1;
                            }
                        }
                    }
                    
                    tracing::info!("📊 Documentation generation complete: {} successful, {} failed out of {} total operations",
                        success_count, fail_count, total_ops);
                    
                } else {
                    tracing::error!("❌ Ollama server is not available at {}, skipping documentation generation", ollama_client.base_url);
                    tracing::info!("💡 To use Ollama documentation, ensure the Ollama server is running and accessible");
                }
            }

            generator
                .finalize_registry(&mut registry)
                .with_context(|| "failed to finalize models")?;

            bootstrap_lib(&config, registry, ops, &args.output)
                .await
                .with_context(|| "failed to bootstrap library")?;

            format_crate(&args.output.as_path())
                .with_context(|| "failed to format the generated crate")?;
        }
    }

    Ok(())
}
