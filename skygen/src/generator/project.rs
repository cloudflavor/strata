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

use crate::generator::model::ModelRegistry;
use crate::Config;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use taplo::formatter;
use tera::{Context as TeraContext, Tera};
use tokio::fs;

#[derive(Debug)]
pub struct RenderPlan {
    template: &'static str,
    out_rel: String,
    extra_ctx: Option<TeraContext>,
}

// Separated rendering logic for easier testing
pub fn render_project_templates(
    tera: &Tera,
    base_ctx: &TeraContext,
    plans: &[RenderPlan],
) -> Result<Vec<String>> {
    let mut rendered_data = Vec::new();
    let mut ctx = base_ctx.clone();

    for p in plans {
        if let Some(extra_ctx) = &p.extra_ctx {
            ctx.extend(extra_ctx.clone());
        }
        let data = tera.render(p.template, &ctx)?;
        rendered_data.push(data);
    }
    Ok(rendered_data)
}

// Separated file writing logic for easier testing
pub async fn write_project_files(root: &Path, plans: &[RenderPlan], rendered_data: &[String]) -> Result<()> {
    for (p, data) in plans.iter().zip(rendered_data.iter()) {
        // NOTE: avoid using the taplo CLI for formatting the Cargo.toml file after tera rendering
        if p.out_rel == "Cargo.toml" {
            let formatted = formatter::format(data, formatter::Options::default());
            let out = root.join(&p.out_rel);
            fs::write(out, formatted).await?;
        } else {
            let out = root.join(&p.out_rel);
            fs::write(out, data).await?;
        }
    }
    Ok(())
}

// Separated file writing logic for easier testing
pub async fn write_static_files(root: &Path, plans: &[RenderPlan]) -> Result<()> {
    for p in plans {
        let f = crate::ASSETS
            .get_file(p.template)
            .with_context(|| "failed to fetch static rust file")?;
        let data = f
            .contents_utf8()
            .with_context(|| "failed to fetch utf8 contents from rust file")?;
        let out = root.join(&p.out_rel);
        fs::write(&out, data).await?;
    }
    Ok(())
}

pub async fn bootstrap_lib(
    config: &Config,
    registry: ModelRegistry,
    out_dir: impl AsRef<Path>,
) -> Result<()> {
    create_dirs(out_dir.as_ref())
        .await
        .with_context(|| "failed to create project directories")?;

    // Load templates
    let template_names = [
        "templates/cargo.toml.tera",
        "templates/operation.rs.tera",
        "templates/model.rs.tera",
        "templates/lib.rs.tera",
        "templates/mod.rs.tera",
    ];
    let tera = load_templates(&template_names)?;

    // Set up base context
    let mut base_ctx = TeraContext::new();
    base_ctx.insert("config", config);

    // Render main project files
    let main_plans = [
        RenderPlan {
            template: "templates/cargo.toml.tera",
            out_rel: "Cargo.toml".to_string(),
            extra_ctx: None,
        },
        RenderPlan {
            template: "templates/lib.rs.tera",
            out_rel: "src/lib.rs".to_string(),
            extra_ctx: None,
        },
        RenderPlan {
            template: "templates/mod.rs.tera",
            out_rel: "src/apis/mod.rs".to_string(),
            extra_ctx: None,
        },
        RenderPlan {
            template: "templates/mod.rs.tera",
            out_rel: "src/models/mod.rs".to_string(),
            extra_ctx: None,
        },
    ];

    let rendered_data = render_project_templates(&tera, &base_ctx, &main_plans)?;
    write_project_files(out_dir.as_ref(), &main_plans, &rendered_data).await?;

    // Write additional Rust files
    let rs_files = [
        RenderPlan {
            template: "lib/client.rs",
            out_rel: "src/client.rs".to_string(),
            extra_ctx: None,
        },
        RenderPlan {
            template: "lib/errors.rs",
            out_rel: "src/errors.rs".to_string(),
            extra_ctx: None,
        },
    ];

    write_static_files(out_dir.as_ref(), &rs_files).await?;

    // Generate model files
    let mut models: Vec<RenderPlan> = Vec::new();
    let mut mods = Vec::new();

    for (name, model) in registry.models.into_iter() {
        mods.push(name.clone());
        let mut model_ctx = TeraContext::new();
        let out_file = format!("src/models/{name}.rs");
        model_ctx.insert("model", &model);

        models.push(RenderPlan {
            template: "templates/model.rs.tera",
            out_rel: out_file,
            extra_ctx: Some(model_ctx),
        });
    }

    // Add module context for models
    let mut mods_ctx = TeraContext::new();
    mods_ctx.insert("modules", &mods);

    models.push(RenderPlan {
        template: "templates/mod.rs.tera",
        out_rel: "src/models/mod.rs".to_string(),
        extra_ctx: Some(mods_ctx),
    });

    let models_ctx = TeraContext::new();
    let rendered_model_data = render_project_templates(&tera, &models_ctx, &models)?;
    write_project_files(out_dir.as_ref(), &models, &rendered_model_data).await?;

    Ok(())
}

/// Load a Tera instance populated with embedded template assets.
pub fn load_templates(names: &[&str]) -> Result<Tera> {
    let mut tera = Tera::default();
    for name in names {
        let f = crate::ASSETS
            .get_file(*name)
            .with_context(|| "failed to fetch template")?;

        tera.add_raw_template(
            name,
            f.contents_utf8()
                .with_context(|| "failed to fetch utf8 contents from template")?,
        )?;
    }
    Ok(tera)
}

async fn create_dirs(root_dir: &Path) -> Result<()> {
    let src_dir = root_dir.join("src");

    for path in [&src_dir, &src_dir.join("apis"), &src_dir.join("models")] {
        fs::create_dir_all(path).await?;
    }

    Ok(())
}

pub fn format_crate(path: &Path) -> Result<()> {
    Command::new("cargo")
        .arg("fmt")
        .current_dir(path)
        .status()?;

    Ok(())
}
