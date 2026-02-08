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
use skygen::generator::operation::OperationGenerator;
use skygen::generator::project::{bootstrap_lib, format_crate};
use skygen::resolver::resolve::Resolver;
use structopt::StructOpt;
use tokio::fs;
use tracing_subscriber::EnvFilter;

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
            let registry = generator
                .collect_models(&resolved)
                .with_context(|| "failed to collect models")?;

            let op_generator = OperationGenerator::new();
            let ops = op_generator
                .collect_operations(&resolved, &registry)
                .with_context(|| "failed to collect operations")?;

            bootstrap_lib(&config, registry, ops, &args.output)
                .await
                .with_context(|| "failed to bootstrap library")?;

            format_crate(&args.output.as_path())
                .with_context(|| "failed to format the generated crate")?;
        }
    }

    Ok(())
}
