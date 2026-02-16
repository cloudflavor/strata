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

pub mod generator;
pub mod ollama;
pub mod resolver;

use core::fmt;
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use structopt::StructOpt;

pub static ASSETS: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets");

#[derive(StructOpt)]
pub struct Cli {
    #[structopt(
        short,
        long,
        default_value = "info",
        possible_values = &["trace", "debug", "info", "warn", "error"]
    )]
    pub log_level: tracing::Level,

    #[structopt(subcommand)]
    pub commands: Commands,
}

#[derive(StructOpt)]
pub enum Commands {
    Generate(GenerateArgs),
}

#[derive(StructOpt)]
pub struct GenerateArgs {
    /// OpenAPIv3 Spec file to generate the SDK from
    #[structopt(short = "s", long = "schema")]
    pub schema: PathBuf,

    /// The output directory where the generated bindings will be placed
    #[structopt(short = "o", long = "output-dir")]
    pub output: PathBuf,

    /// Skygen config for generating the SDK.
    #[structopt(short = "c", long = "config")]
    pub config: PathBuf,

    /// Ollama model to use for generating documentation (format: model:name)
    /// Defaults to "gpt-oss:latest" if not specified
    #[structopt(long = "ollama", env = "SKYGEN_OLLAMA_MODEL")]
    pub ollama_model: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    crate_name: String,
    version: String,
    edition: Option<String>,
    description: String,
    lib_status: String,
    keywords: Vec<String>,
    api_url: String,
    authors: Vec<String>,
    include_only: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum ResolverError {
    InvalidRef(String),
    PointerEscape(String),
    MissingTarget { ref_: String, path: String },
    TypeMismatch { ref_: String, expected: String },
    CycleDetected(String),
    MaxDeptExceeded { ref_: String, depth: usize },
}

impl fmt::Display for ResolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRef(r) => write!(f, "invalid $ref: {r}"),
            Self::PointerEscape(seg) => write!(f, "invalid JSON pointer escape in segment: {seg}"),
            Self::MissingTarget { ref_, path } => {
                write!(f, "could not find $ref: {ref_} for path: {path}")
            }
            Self::TypeMismatch { ref_, expected } => {
                write!(f, "resolved $ref {ref_} is not a {expected}")
            }
            Self::CycleDetected(r) => write!(f, "cycle detected while resolving $ref: {r}"),
            Self::MaxDeptExceeded { ref_, depth } => {
                write!(f, "max resolution depth {ref_} exceeded at {depth}")
            }
        }
    }
}

impl std::error::Error for ResolverError {}
