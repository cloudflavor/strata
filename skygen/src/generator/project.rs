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

async fn render_templates(
    tera: &tera::Tera,
    root: &Path,
    base: &mut TeraContext,
    plans: &[RenderPlan],
) -> Result<()> {
    for p in plans {
        if let Some(extra_ctx) = p.extra_ctx.to_owned() {
            base.extend(extra_ctx);
        }
        let mut data = tera.render(p.template, &base)?;
        // NOTE: avoid using the taplo CLI for formatting the Cargo.toml file after tera rendering
        if p.out_rel == "Cargo.toml" {
            data = formatter::format(&data, formatter::Options::default());
        }
        let out = root.join(&p.out_rel);

        fs::write(out, data).await?;
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

    let mut tera = Tera::default();
    for name in [
        "templates/cargo.toml.tera",
        "templates/operation.rs.tera",
        "templates/model.rs.tera",
        "templates/lib.rs.tera",
        "templates/mod.rs.tera",
    ] {
        let f = crate::ASSETS
            .get_file(name)
            .with_context(|| "failed to fetch template")?;

        tera.add_raw_template(
            name,
            f.contents_utf8()
                .with_context(|| "failed to fetch utf8 contents from template")?,
        )?;
    }

    let mut base_ctx = TeraContext::new();
    base_ctx.insert("config", config);

    let plans = [
        RenderPlan {
            template: "templates/cargo.toml.tera",
            out_rel: String::from("Cargo.toml"),
            extra_ctx: None,
        },
        RenderPlan {
            template: "templates/lib.rs.tera",
            out_rel: String::from("src/lib.rs"),
            extra_ctx: None,
        },
        RenderPlan {
            template: "templates/mod.rs.tera",
            out_rel: String::from("src/apis/mod.rs"),
            extra_ctx: None,
        },
        RenderPlan {
            template: "templates/mod.rs.tera",
            out_rel: String::from("src/models/mod.rs"),
            extra_ctx: None,
        },
    ];
    render_templates(&tera, out_dir.as_ref(), &mut base_ctx, &plans).await?;

    let rs_files = [
        RenderPlan {
            template: "lib/client.rs",
            out_rel: String::from("src/client.rs"),
            extra_ctx: None,
        },
        RenderPlan {
            template: "lib/errors.rs",
            out_rel: String::from("src/errors.rs"),
            extra_ctx: None,
        },
    ];

    write_rs_files(out_dir.as_ref(), &rs_files).await?;

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
    let mut mods_ctx = TeraContext::new();
    mods_ctx.insert("modules", &mods);

    models.push(RenderPlan {
        template: "templates/mod.rs.tera",
        out_rel: String::from("src/models/mod.rs"),
        extra_ctx: Some(mods_ctx),
    });

    let mut models_ctx = TeraContext::new();
    render_templates(&tera, out_dir.as_ref(), &mut models_ctx, &models).await?;

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

async fn write_rs_files(root: &Path, plans: &[RenderPlan]) -> Result<()> {
    for p in plans {
        let f = crate::ASSETS
            .get_file(p.template)
            .with_context(|| "failed to fetch static rust file")?;
        let data = f
            .contents_utf8()
            .with_context(|| "failed to fetch utf8 contents from rust file")?;
        let out = root.join(&p.out_rel);

        fs::write(&out, data).await?
    }

    Ok(())
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
