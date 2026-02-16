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

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: String,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    client: Client,
    base_url: String,
    timeout: Duration,
}

impl OllamaClient {
    pub fn new(base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());
        
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            base_url,
            timeout: Duration::from_secs(60),
        }
    }

    /// Get the base URL for the Ollama server
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn generate_documentation(&self, model: &str, prompt: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.base_url);
        
        let request = OllamaRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
            stream: false,
        };

        tracing::info!("📝 Sending documentation request to Ollama model: {}", model);
        tracing::debug!("Prompt length: {} characters", prompt.len());
        
        // Log a summary of the prompt for debugging
        if tracing::level_filters::LevelFilter::DEBUG == tracing::level_filters::LevelFilter::current() {
            let preview = if prompt.len() > 500 {
                format!("{}...", &prompt[..500])
            } else {
                prompt.to_string()
            };
            tracing::debug!("Prompt preview:\n{}", preview);
        }

        let start_time = std::time::Instant::now();
        
        let response = self.client
            .post(&url)
            .json(&request)
            .timeout(self.timeout)
            .send()
            .await
            .with_context(|| format!("Failed to send request to Ollama at {}", url))?;

        let duration = start_time.elapsed();
        tracing::debug!("Ollama API response received in {:?}", duration);

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            tracing::error!("Ollama request failed with status {}: {}", status, error_body);
            anyhow::bail!("Ollama request failed with status {}: {}", status, error_body);
        }

        let ollama_response: OllamaResponse = response
            .json()
            .await
            .with_context(|| "Failed to parse Ollama response")?;

        if let Some(error) = ollama_response.error {
            tracing::error!("Ollama returned error: {}", error);
            anyhow::bail!("Ollama error: {}", error);
        }

        if !ollama_response.done {
            tracing::warn!("Ollama response not marked as done, but continuing anyway");
        }

        let doc_length = ollama_response.response.len();
        tracing::info!("✅ Generated documentation: {} characters", doc_length);
        
        // Log the generated documentation for debugging
        if tracing::level_filters::LevelFilter::DEBUG == tracing::level_filters::LevelFilter::current() {
            tracing::debug!("Generated documentation:\n{}", ollama_response.response);
        }

        Ok(ollama_response.response)
    }

    /// Check if Ollama server is available
    pub async fn check_availability(&self) -> Result<bool> {
        let url = format!("{}/api/tags", self.base_url);
        
        match self.client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocumentationPromptBuilder;

impl DocumentationPromptBuilder {
    pub fn build_operation_prompt(
        operation_name: &str,
        operation_description: &str,
        parameters: &[(String, String, String)], // (name, type, description)
        response_types: &[(String, String, String)], // (status, type, description)
        examples: &str,
        openapi_spec: &str,
        rust_function_signature: &str, // Add the generated Rust function signature
        sdk_crate_name: &str, // Add the SDK crate name
        client_pattern: &str, // Add the client usage pattern
    ) -> String {
        let parameters_section = if parameters.is_empty() {
            String::new()
        } else {
            let params = parameters
                .iter()
                .map(|(name, typ, desc)| format!("  - {} ({}): {}", name, typ, desc))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\nParameters:\n{}", params)
        };

        let responses_section = if response_types.is_empty() {
            String::new()
        } else {
            let responses = response_types
                .iter()
                .map(|(status, typ, desc)| format!("  - {}: {} - {}", status, typ, desc))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\nResponses:\n{}", responses)
        };

        let examples_section = if examples.is_empty() {
            String::new()
        } else {
            format!("\nExamples:\n{}", examples)
        };

        let openapi_section = if openapi_spec.is_empty() {
            String::new()
        } else {
            format!("\n\nOpenAPI Specification:\n```yaml\n{}\n```", openapi_spec)
        };

        let rust_signature_section = if rust_function_signature.is_empty() {
            String::new()
        } else {
            format!("\n\nGenerated Rust Function Signature:\n```rust\n{}\n```", rust_function_signature)
        };

        let sdk_info_section = format!("\n\nSDK Information:\n- Crate Name: {}", sdk_crate_name);
        
        let client_context_section = format!("\n\nClient Usage Pattern:\n{}", client_pattern);

        format!(
            "You are an expert technical writer creating documentation for a Rust SDK generated from OpenAPI specifications.

Generate comprehensive, professional documentation for the following API operation:

Operation: {}
Description: {}{}{}{}{}{}{}{}

IMPORTANT CONTEXT:
The SDK uses a builder pattern where operations are called on a client instance
and executed with .send().await. Here's the client pattern:
{}

Please provide:
1. A detailed Rustdoc comment for THIS operation
2. Usage examples showing client.operation(...).send().await pattern
3. Response handling for THIS operation's specific response variants
4. Error handling specific to THIS operation
5. Best practices for THIS specific operation

DO NOT document client construction - that's in client.rs.
Assume the client already exists and is properly configured.
Focus ONLY on using THIS specific operation.

Format as Rustdoc (///) for direct insertion.
Use crate name '{}' in imports.
Show: client.operation_name(...).send().await pattern.
Handle the specific response enum variants shown.
Assume reader knows client setup and tokio basics.",
            operation_name, operation_description, parameters_section, responses_section, examples_section, openapi_section, rust_signature_section, sdk_info_section, client_context_section, sdk_crate_name, sdk_crate_name
        )
    }
}