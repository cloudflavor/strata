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
use crate::generator::operation::OperationRegistry;
use crate::Config;
use anyhow::{Context, Result};
use indexmap::{IndexMap, IndexSet};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use taplo::formatter;
use tera::{Context as TeraContext, Tera, Value};
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
pub async fn write_project_files(
    root: &Path,
    plans: &[RenderPlan],
    rendered_data: &[String],
) -> Result<()> {
    for (p, data) in plans.iter().zip(rendered_data.iter()) {
        // NOTE: avoid using the taplo CLI for formatting the Cargo.toml file after tera rendering
        let out = root.join(&p.out_rel);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).await?;
        }
        if p.out_rel == "Cargo.toml" {
            let formatted = formatter::format(data, formatter::Options::default());
            fs::write(out, formatted).await?;
        } else {
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
    ops: OperationRegistry,
    out_dir: impl AsRef<Path>,
) -> Result<()> {
    create_dirs(out_dir.as_ref())
        .await
        .with_context(|| "failed to create project directories")?;

    // Load templates
    let template_names = [
        "templates/cargo.toml.tera",
        "templates/operations.rs.tera",
        "templates/model.rs.tera",
        "templates/aliases.rs.tera",
        "templates/lib.rs.tera",
        "templates/mod.rs.tera",
    ];
    let tera = load_templates(&template_names)?;

    // Set up base context
    let mut base_ctx = TeraContext::new();
    base_ctx.insert("config", config);

    // Render main project files
    let mut models_mod_ctx = TeraContext::new();
    models_mod_ctx.insert("is_models_mod", &true);

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
            extra_ctx: Some(models_mod_ctx),
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
    let mut mods: Vec<String> = Vec::new();
    let mut type_map = registry.type_map.clone();
    let mut alias_models = Vec::new();

    let mut alias_rust_names: Vec<String> = Vec::new();
    for model in registry.models.values() {
        if !matches!(
            model.kind,
            crate::generator::model::ModelType::Struct(_)
                | crate::generator::model::ModelType::Composite(_)
        ) {
            alias_rust_names.push(model.rust_name.clone());
        }
    }

    let alias_module = if !alias_rust_names.is_empty() {
        let mut candidate = "aliases".to_string();
        if registry.models.contains_key(&candidate) {
            candidate = "model_aliases".to_string();
        }
        if registry.models.contains_key(&candidate) {
            candidate = "type_aliases".to_string();
        }
        Some(candidate)
    } else {
        None
    };

    if let Some(module_name) = alias_module.as_ref() {
        for rust_name in &alias_rust_names {
            type_map.insert(rust_name.clone(), module_name.clone());
        }
    }

    for (name, mut model) in registry.models.into_iter() {
        model.dep_imports =
            crate::generator::model::group_dep_imports(&model.deps, &type_map);
        match model.kind {
            crate::generator::model::ModelType::Struct(_)
            | crate::generator::model::ModelType::Composite(_) => {
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
            _ => {
                alias_models.push(model);
            }
        }
    }

    if let (Some(alias_module), true) = (alias_module, !alias_models.is_empty()) {

        let mut alias_ctx = TeraContext::new();
        alias_ctx.insert("aliases", &alias_models);

        let mut alias_deps = indexmap::IndexSet::new();
        for model in &alias_models {
            for dep in &model.deps {
                alias_deps.insert(dep.clone());
            }
        }
        let mut alias_deps: Vec<String> = alias_deps.into_iter().collect();
        alias_deps.sort();
        let dep_imports =
            crate::generator::model::group_dep_imports(&alias_deps, &type_map);
        alias_ctx.insert("dep_imports", &dep_imports);

        let out_file = format!("src/models/{alias_module}.rs");
        models.push(RenderPlan {
            template: "templates/aliases.rs.tera",
            out_rel: out_file,
            extra_ctx: Some(alias_ctx),
        });
        mods.push(alias_module);
    }

    // Add module context for models
    let mut mods_ctx = TeraContext::new();
    mods_ctx.insert("modules", &mods);
    mods_ctx.insert("is_models_mod", &true);

    models.push(RenderPlan {
        template: "templates/mod.rs.tera",
        out_rel: "src/models/mod.rs".to_string(),
        extra_ctx: Some(mods_ctx),
    });

    let models_ctx = TeraContext::new();
    let rendered_model_data = render_project_templates(&tera, &models_ctx, &models)?;
    clean_stale_model_files(out_dir.as_ref(), &models).await?;
    write_project_files(out_dir.as_ref(), &models, &rendered_model_data).await?;

    // Generate grouped operation files
    let mut ops_plans: Vec<RenderPlan> = Vec::new();
    let mut groups: IndexMap<String, Vec<crate::generator::operation::OperationDef>> =
        IndexMap::new();

    for op in ops.ops.values() {
        groups.entry(op.group.clone()).or_default().push(op.clone());
    }

    let mut top_modules: Vec<String> = Vec::new();
    for (group, group_ops) in groups.iter() {
        top_modules.push(group.clone());

        let mut deps: IndexSet<String> = IndexSet::new();
        let mut uses_header_value = false;
        let mut uses_cookie_header = false;
        let mut uses_content_type = false;
        let mut uses_error_types = false;

        for op in group_ops {
            for dep in &op.deps {
                deps.insert(dep.clone());
            }
            if op.has_header_params || op.has_cookie_params || op.request_body.is_some() {
                uses_header_value = true;
            }
            if op.has_cookie_params {
                uses_cookie_header = true;
            }
            if op.request_body.is_some() {
                uses_content_type = true;
            }
            if !op.response_enum.has_default {
                uses_error_types = true;
            }
        }

        let mut deps_vec: Vec<String> = deps.into_iter().collect();
        deps_vec.sort();
        let dep_imports = crate::generator::model::group_dep_imports(&deps_vec, &type_map);

        let mut group_ctx = TeraContext::new();
        group_ctx.insert("operations", group_ops);
        group_ctx.insert("dep_imports", &dep_imports);
        group_ctx.insert("uses_header_value", &uses_header_value);
        group_ctx.insert("uses_cookie_header", &uses_cookie_header);
        group_ctx.insert("uses_content_type", &uses_content_type);
        group_ctx.insert("uses_error_types", &uses_error_types);

        ops_plans.push(RenderPlan {
            template: "templates/operations.rs.tera",
            out_rel: format!("src/apis/{group}.rs"),
            extra_ctx: Some(group_ctx),
        });
    }

    let mut api_mods_ctx = TeraContext::new();
    api_mods_ctx.insert("modules", &top_modules);
    ops_plans.push(RenderPlan {
        template: "templates/mod.rs.tera",
        out_rel: "src/apis/mod.rs".to_string(),
        extra_ctx: Some(api_mods_ctx),
    });

    let ops_ctx = TeraContext::new();
    let rendered_ops_data = render_project_templates(&tera, &ops_ctx, &ops_plans)?;
    clean_stale_api_files(out_dir.as_ref(), &ops_plans).await?;
    write_project_files(out_dir.as_ref(), &ops_plans, &rendered_ops_data).await?;

    Ok(())
}

/// Load a Tera instance populated with embedded template assets.
pub fn load_templates(names: &[&str]) -> Result<Tera> {
    let mut tera = Tera::default();

    // Register custom filters
    tera.register_filter("sanitize_module_name", sanitize_module_name_filter);

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

/// Filter to sanitize module names for use in Rust imports
fn sanitize_module_name_filter(
    value: &Value,
    _args: &std::collections::HashMap<String, Value>,
) -> tera::Result<Value> {
    if let Some(s) = value.as_str() {
        // This is a simplified version of the sanitize_module_name function
        // from the model.rs file
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push('_');
            }
        }
        if out.is_empty() {
            out = "_".to_string();
        }
        Ok(Value::String(out))
    } else {
        // If value is not a string, return it as is
        Ok(value.clone())
    }
}

async fn create_dirs(root_dir: &Path) -> Result<()> {
    let src_dir = root_dir.join("src");

    for path in [&src_dir, &src_dir.join("apis"), &src_dir.join("models")] {
        fs::create_dir_all(path).await?;
    }

    Ok(())
}

async fn clean_stale_model_files(root: &Path, plans: &[RenderPlan]) -> Result<()> {
    let models_dir = root.join("src/models");
    if !models_dir.exists() {
        return Ok(());
    }

    let mut desired: HashSet<std::path::PathBuf> = HashSet::new();
    for plan in plans {
        desired.insert(root.join(&plan.out_rel));
    }

    let mut entries = fs::read_dir(&models_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        if !desired.contains(&path) {
            fs::remove_file(path).await?;
        }
    }

    Ok(())
}

async fn clean_stale_api_files(root: &Path, plans: &[RenderPlan]) -> Result<()> {
    let apis_dir = root.join("src/apis");
    if !apis_dir.exists() {
        return Ok(());
    }

    let mut desired: HashSet<std::path::PathBuf> = HashSet::new();
    for plan in plans {
        desired.insert(root.join(&plan.out_rel));
    }

    let mut stack = vec![apis_dir.clone()];
    while let Some(dir) = stack.pop() {
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            if !desired.contains(&path) {
                fs::remove_file(path).await?;
            }
        }
    }

    remove_empty_api_dirs(&apis_dir).await?;

    Ok(())
}

async fn remove_empty_api_dirs(root: &Path) -> Result<()> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                stack.push(entry.path());
            }
        }
    }

    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
    for dir in dirs {
        if dir == root {
            continue;
        }
        let mut entries = fs::read_dir(&dir).await?;
        if entries.next_entry().await?.is_none() {
            fs::remove_dir(&dir).await?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::model::{ModelType, PrimitiveType};
    use crate::generator::operation::{
        OperationDef, OperationParam, OperationParamLocation, OperationResponse,
        OperationResponseEnum, OperationResponseVariant,
    };
    use openapiv3::StatusCode;
    use tera::Context as TeraContext;

    #[test]
    fn renders_operation_template_with_client() {
        let tera = load_templates(&["templates/operations.rs.tera"]).expect("templates");
        let mut ctx = TeraContext::new();
        let operation = OperationDef {
            id: "get_test".into(),
            method: "get".into(),
            path: "/test/{id}".into(),
            tags: vec![],
            request_body: None,
            responses: vec![OperationResponse {
                status: Some(StatusCode::Code(200)),
                content_type: Some("application/json".into()),
                typ: ModelType::Primitive(PrimitiveType::String),
                render_type: "String".into(),
            }],
            response_enum: OperationResponseEnum {
                name: "GetTestResponse".into(),
                variants: vec![OperationResponseVariant {
                    name: "Status200".into(),
                    status: Some(StatusCode::Code(200)),
                    render_type: "String".into(),
                    status_match: "200".into(),
                    is_default: false,
                }],
                has_default: false,
            },
            deps: Vec::new(),
            dep_imports: Vec::new(),
            group: "default".into(),
            params: vec![OperationParam {
                name: "id".into(),
                rust_name: "id".into(),
                location: OperationParamLocation::Path,
                required: true,
                typ: ModelType::Primitive(PrimitiveType::String),
                render_type: "String".into(),
                is_array: false,
                array_item_is_primitive: false,
            }],
            has_path_params: true,
            has_query_params: false,
            has_header_params: false,
            has_cookie_params: false,
        };
        ctx.insert("operations", &vec![operation]);
        ctx.insert("dep_imports", &Vec::<crate::generator::model::DepImport>::new());
        ctx.insert("uses_header_value", &false);
        ctx.insert("uses_cookie_header", &false);
        ctx.insert("uses_content_type", &false);
        ctx.insert("uses_error_types", &true);
        let data = tera
            .render("templates/operations.rs.tera", &ctx)
            .expect("render");
        assert!(data.contains("client: &Client"));
        assert!(data.contains("parse_response"));
        assert!(data.contains("reqwest::Request::new"));
    }
}
