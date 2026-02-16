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

    pub async fn generate_documentation(&self, model: &str, prompt: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.base_url);
        
        let request = OllamaRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
            stream: false,
        };

        tracing::debug!("Sending prompt to Ollama model: {}", model);
        
        let response = self.client
            .post(&url)
            .json(&request)
            .timeout(self.timeout)
            .send()
            .await
            .with_context(|| format!("Failed to send request to Ollama at {}", url))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("Ollama request failed with status {}: {}", status, error_body);
        }

        let ollama_response: OllamaResponse = response
            .json()
            .await
            .with_context(|| "Failed to parse Ollama response")?;

        if let Some(error) = ollama_response.error {
            anyhow::bail!("Ollama error: {}", error);
        }

        if !ollama_response.done {
            tracing::warn!("Ollama response not marked as done, but continuing anyway");
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
        parameters: &[(String, String)],
        response_types: &[(String, String)],
        examples: &str,
    ) -> String {
        let parameters_section = if parameters.is_empty() {
            String::new()
        } else {
            let params = parameters
                .iter()
                .map(|(name, desc)| format!("  - {}: {}", name, desc))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\nParameters:\n{}", params)
        };

        let responses_section = if response_types.is_empty() {
            String::new()
        } else {
            let responses = response_types
                .iter()
                .map(|(status, desc)| format!("  - {}: {}", status, desc))
                .collect::<Vec<_>>()
                .join("\n");
            format!("\nResponses:\n{}", responses)
        };

        let examples_section = if examples.is_empty() {
            String::new()
        } else {
            format!("\nExamples:\n{}", examples)
        };

        format!(
            "You are an expert technical writer creating documentation for a Rust SDK generated from OpenAPI specifications.

Generate comprehensive, professional documentation for the following API operation:

Operation: {}
Description: {}{}{}{}

Please provide:
1. A detailed Rustdoc comment suitable for the function
2. Usage examples in Rust
3. Error handling guidance
4. Best practices and recommendations

Format the response as a Rustdoc comment (///) that can be directly inserted into the generated code.",
            operation_name, operation_description, parameters_section, responses_section, examples_section
        )
    }
}