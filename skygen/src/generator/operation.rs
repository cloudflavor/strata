// Copyright 2025 Cloudflavor GmbH
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::generator::model::{
    group_dep_imports, model_type_to_rust, sanitize_field_name, sanitize_module_name,
    sanitize_type_name, type_signature, DepImport, ModelDef, ModelGenerator, ModelRegistry,
    ModelType,
};
use std::collections::BTreeSet;
use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use openapiv3::{
    MediaType, OpenAPI, Operation, Parameter, ParameterSchemaOrContent, PathItem, ReferenceOr,
    RequestBody, Response, StatusCode,
};
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct OperationGenerator;

impl OperationGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn collect_operations(
        &self,
        doc: &OpenAPI,
        models: &mut ModelRegistry,
    ) -> Result<OperationRegistry> {
        let mut registry = OperationRegistry::default();
        let mut seen_ids = indexmap::IndexSet::new();
        let mut seen_short_names: std::collections::HashMap<String, indexmap::IndexSet<String>> =
            std::collections::HashMap::new();
        let mut seen_model_prefixes: HashSet<String> = HashSet::new();

        for (path, entry) in doc.paths.paths.iter() {
            let item = match entry {
                ReferenceOr::Item(item) => item,
                ReferenceOr::Reference { reference } => {
                    return Err(anyhow!("unexpected path reference {reference}"));
                }
            };

            self.collect_from_item(
                path,
                item,
                models,
                &mut registry,
                &mut seen_ids,
                &mut seen_short_names,
                &mut seen_model_prefixes,
            )?;
        }

        Ok(registry)
    }

    fn collect_from_item(
        &self,
        path: &str,
        item: &PathItem,
        models: &mut ModelRegistry,
        registry: &mut OperationRegistry,
        seen_ids: &mut indexmap::IndexSet<String>,
        seen_short_names: &mut std::collections::HashMap<String, indexmap::IndexSet<String>>,
        seen_model_prefixes: &mut HashSet<String>,
    ) -> Result<()> {
        if let Some(op) = item.get.as_ref() {
            self.push_op(
                "get",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.put.as_ref() {
            self.push_op(
                "put",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.post.as_ref() {
            self.push_op(
                "post",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.delete.as_ref() {
            self.push_op(
                "delete",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.options.as_ref() {
            self.push_op(
                "options",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.head.as_ref() {
            self.push_op(
                "head",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.patch.as_ref() {
            self.push_op(
                "patch",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }
        if let Some(op) = item.trace.as_ref() {
            self.push_op(
                "trace",
                path,
                item,
                op,
                models,
                registry,
                seen_ids,
                seen_short_names,
                seen_model_prefixes,
            );
        }

        Ok(())
    }

    fn push_op(
        &self,
        method: &str,
        path: &str,
        item: &PathItem,
        op: &Operation,
        models: &mut ModelRegistry,
        registry: &mut OperationRegistry,
        seen_ids: &mut indexmap::IndexSet<String>,
        seen_short_names: &mut std::collections::HashMap<String, indexmap::IndexSet<String>>,
        seen_model_prefixes: &mut HashSet<String>,
    ) {
        let base_id = op
            .operation_id
            .clone()
            .unwrap_or_else(|| format!("{method}_{path}"));
        let id = unique_operation_id(&sanitize_module_name(&base_id), seen_ids);
        let mut def = OperationDef {
            id,
            method: method.to_string(),
            path: path.to_string(),
            tags: op.tags.clone(),
            request_body: None,
            responses: Vec::new(),
            response_enum: OperationResponseEnum {
                name: String::new(),
                variants: Vec::new(),
                has_default: false,
            },
            deps: Vec::new(),
            dep_imports: Vec::new(),
            group: sanitize_module_name(op.tags.first().map(|s| s.as_str()).unwrap_or("default")),
            builder_name: String::new(),
            short_name: String::new(),
            params: Vec::new(),
            required_params: Vec::new(),
            optional_params: Vec::new(),
            has_path_params: false,
            has_query_params: false,
            has_header_params: false,
            has_cookie_params: false,
        };

        let model_gen = ModelGenerator::with_module_name_map(models.name_map.clone());

        def.params =
            collect_parameters(&model_gen, models, item.parameters.iter().chain(op.parameters.iter()));
        for param in &def.params {
            match param.location {
                OperationParamLocation::Path => def.has_path_params = true,
                OperationParamLocation::Query => def.has_query_params = true,
                OperationParamLocation::Header => def.has_header_params = true,
                OperationParamLocation::Cookie => def.has_cookie_params = true,
            }
        }

        let base_short = op_short_name(&sanitize_module_name(&base_id), &def.group);
        def.short_name = unique_short_name(
            &base_short,
            &def.id,
            &def.method,
            &def.path,
            &def.group,
            seen_short_names,
        );
        def.builder_name = sanitize_type_name(&def.short_name);
        let model_prefix = unique_model_prefix(
            &def.short_name,
            &def.group,
            &def.method,
            &def.path,
            seen_model_prefixes,
        );

        if let Some(body) = op.request_body.as_ref() {
            if let Ok(Some(body)) =
                request_body_to_type(&model_gen, models, &model_prefix, body)
            {
                def.request_body = Some(body);
            }
        }

        def.responses = responses_to_types(&model_gen, models, &model_prefix, &op.responses);
        def.response_enum = build_response_enum(&def);
        def.deps = collect_operation_deps(&def);
        def.dep_imports = group_dep_imports(&def.deps, &models.type_map);
        def.required_params = def.params.iter().filter(|p| p.required).cloned().collect();
        def.optional_params = def.params.iter().filter(|p| !p.required).cloned().collect();

        registry.ops.insert(def.id.clone(), def);
    }
}

#[derive(Debug, Default)]
pub struct OperationRegistry {
    pub ops: IndexMap<String, OperationDef>,
}

impl OperationRegistry {
    pub fn get(&self, id: &str) -> Option<&OperationDef> {
        self.ops.get(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &OperationDef> {
        self.ops.values()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationDef {
    pub id: String,
    pub method: String,
    pub path: String,
    pub tags: Vec<String>,
    pub request_body: Option<OperationBody>,
    pub responses: Vec<OperationResponse>,
    pub response_enum: OperationResponseEnum,
    pub deps: Vec<String>,
    pub dep_imports: Vec<DepImport>,
    pub group: String,
    pub builder_name: String,
    pub short_name: String,
    pub params: Vec<OperationParam>,
    pub required_params: Vec<OperationParam>,
    pub optional_params: Vec<OperationParam>,
    pub has_path_params: bool,
    pub has_query_params: bool,
    pub has_header_params: bool,
    pub has_cookie_params: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationParam {
    pub name: String,
    pub rust_name: String,
    pub location: OperationParamLocation,
    pub required: bool,
    pub typ: ModelType,
    pub render_type: String,
    pub is_array: bool,
    pub array_item_is_primitive: bool,
    pub is_display: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
pub enum OperationParamLocation {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationBody {
    pub content_type: String,
    pub typ: ModelType,
    pub render_type: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ResponseParseMode {
    Json,
    Bytes,
    Empty,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationResponse {
    pub status: Option<StatusCode>,
    pub content_type: Option<String>,
    pub typ: ModelType,
    pub render_type: String,
    pub parse_mode: ResponseParseMode,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationResponseEnum {
    pub name: String,
    pub variants: Vec<OperationResponseVariant>,
    pub has_default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationResponseVariant {
    pub name: String,
    pub status: Option<StatusCode>,
    pub render_type: String,
    pub status_match: String,
    pub is_default: bool,
    pub parse_mode: ResponseParseMode,
}

fn unique_operation_id(base: &str, existing: &mut indexmap::IndexSet<String>) -> String {
    if !existing.contains(base) {
        existing.insert(base.to_string());
        return base.to_string();
    }
    let mut idx = 2;
    loop {
        let candidate = format!("{base}_{idx}");
        if !existing.contains(&candidate) {
            existing.insert(candidate.clone());
            return candidate;
        }
        idx += 1;
    }
}

fn request_body_to_type(
    generator: &ModelGenerator,
    models: &mut ModelRegistry,
    op_name: &str,
    body: &ReferenceOr<RequestBody>,
) -> Result<Option<OperationBody>> {
    let body = match body {
        ReferenceOr::Item(body) => body,
        ReferenceOr::Reference { reference } => {
            return Err(anyhow!("unexpected request body reference {reference}"));
        }
    };

    let Some((content_type, media)) = select_media_type(&body.content) else {
        return Ok(None);
    };
    let Some(schema) = media.schema.as_ref() else {
        return Ok(None);
    };

    let typ = generator.schema_ref_to_type(schema)?;
    let typ = ensure_named_type(
        generator,
        models,
        &format!("{op_name}_req"),
        typ,
    );
    let render_type = model_type_to_rust(&typ);
    Ok(Some(OperationBody {
        content_type,
        typ,
        render_type,
        required: body.required,
    }))
}

fn responses_to_types(
    generator: &ModelGenerator,
    models: &mut ModelRegistry,
    op_name: &str,
    responses: &openapiv3::Responses,
) -> Vec<OperationResponse> {
    let mut out = Vec::new();

    if let Some(default) = responses.default.as_ref() {
        if let Some(resp) = response_to_type(generator, models, op_name, None, default) {
            out.push(resp);
        }
    }

    for (status, response) in responses.responses.iter() {
        if let Some(resp) = response_to_type(
            generator,
            models,
            op_name,
            Some(status.clone()),
            response,
        ) {
            out.push(resp);
        }
    }

    out
}

fn response_to_type(
    generator: &ModelGenerator,
    models: &mut ModelRegistry,
    op_name: &str,
    status: Option<StatusCode>,
    response: &ReferenceOr<Response>,
) -> Option<OperationResponse> {
    let response = match response {
        ReferenceOr::Item(resp) => resp,
        ReferenceOr::Reference { .. } => {
            return None;
        }
    };

    if response.content.is_empty() {
        return Some(OperationResponse {
            status,
            content_type: None,
            typ: ModelType::Opaque,
            render_type: "()".into(),
            parse_mode: ResponseParseMode::Empty,
        });
    }

    let (content_type, media) = select_media_type(&response.content)?;
    let Some(schema) = media.schema.as_ref() else {
        return Some(OperationResponse {
            status,
            content_type: Some(content_type),
            typ: ModelType::Opaque,
            render_type: "Vec<u8>".into(),
            parse_mode: ResponseParseMode::Bytes,
        });
    };
    let typ = generator.schema_ref_to_type(schema).ok()?;
    let typ = ensure_named_type(
        generator,
        models,
        &format!(
            "{op_name}_resp_{}",
            response_suffix(&status)
        ),
        typ,
    );
    let render_type = model_type_to_rust(&typ);

    Some(OperationResponse {
        status,
        content_type: Some(content_type),
        typ,
        render_type,
        parse_mode: ResponseParseMode::Json,
    })
}

fn select_media_type(content: &IndexMap<String, MediaType>) -> Option<(String, &MediaType)> {
    if let Some(media) = content.get("application/json") {
        return Some(("application/json".into(), media));
    }
    content
        .iter()
        .next()
        .map(|(key, value)| (key.clone(), value))
}

fn build_response_enum(def: &OperationDef) -> OperationResponseEnum {
    let name = format!("{}Response", def.builder_name);
    let mut variants = Vec::new();
    let mut used = indexmap::IndexSet::new();

    for response in &def.responses {
        let (base, status_match, is_default) = match response.status.as_ref() {
            Some(StatusCode::Code(code)) => (format!("Status{code}"), format!("{code}"), false),
            Some(StatusCode::Range(range)) => {
                let start = range * 100;
                let end = start + 99;
                (
                    format!("Status{}XX", range),
                    format!("{start}..={end}"),
                    false,
                )
            }
            None => ("Default".into(), "_".into(), true),
        };
        let variant_name = unique_variant_name(&base, &mut used);
        variants.push(OperationResponseVariant {
            name: variant_name,
            status: response.status.clone(),
            render_type: response.render_type.clone(),
            status_match,
            is_default,
            parse_mode: response.parse_mode,
        });
    }

    let has_default = variants.iter().any(|v| v.is_default);
    OperationResponseEnum {
        name,
        variants,
        has_default,
    }
}

fn unique_variant_name(base: &str, existing: &mut indexmap::IndexSet<String>) -> String {
    if !existing.contains(base) {
        existing.insert(base.to_string());
        return base.to_string();
    }
    let mut idx = 2;
    loop {
        let candidate = format!("{base}_{idx}");
        if !existing.contains(&candidate) {
            existing.insert(candidate.clone());
            return candidate;
        }
        idx += 1;
    }
}

fn collect_operation_deps(def: &OperationDef) -> Vec<String> {
    let mut deps = indexmap::IndexSet::new();
    for param in &def.params {
        collect_deps_from_type(&param.typ, &mut deps);
    }
    if let Some(body) = def.request_body.as_ref() {
        collect_deps_from_type(&body.typ, &mut deps);
    }
    for response in &def.responses {
        collect_deps_from_type(&response.typ, &mut deps);
    }
    let mut out: Vec<String> = deps.into_iter().collect();
    out.sort();
    out
}


fn collect_deps_from_type(typ: &ModelType, deps: &mut indexmap::IndexSet<String>) {
    match typ {
        ModelType::Ref(name) => {
            deps.insert(sanitize_type_name(name));
        }
        ModelType::Array(inner) => {
            if model_type_to_rust(inner) != "serde_json::Value" {
                collect_deps_from_type(inner, deps);
            }
        }
        ModelType::Struct(def) => {
            if model_type_to_rust(typ) == "serde_json::Value" {
                return;
            }
            for field in &def.fields {
                if model_type_to_rust(&field.typ) != "serde_json::Value" {
                    collect_deps_from_type(&field.typ, deps);
                }
            }
        }
        ModelType::Composite(comp) => {
            if model_type_to_rust(typ) == "serde_json::Value" {
                return;
            }
            for variant in &comp.variants {
                if model_type_to_rust(variant) != "serde_json::Value" {
                    collect_deps_from_type(variant, deps);
                }
            }
        }
        ModelType::Primitive(_) | ModelType::Opaque => {}
    }
}

fn collect_parameters<'a>(
    generator: &ModelGenerator,
    models: &mut ModelRegistry,
    params: impl Iterator<Item = &'a ReferenceOr<Parameter>>,
) -> Vec<OperationParam> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<(OperationParamLocation, String)> = HashSet::new();

    for param in params {
        let param = match param {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { .. } => {
                continue;
            }
        };

        match param {
            Parameter::Query { parameter_data, .. } => {
                if let Some(param) =
                    parameter_to_param(generator, models, parameter_data, OperationParamLocation::Query)
                {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
            Parameter::Header { parameter_data, .. } => {
                if let Some(param) =
                    parameter_to_param(generator, models, parameter_data, OperationParamLocation::Header)
                {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
            Parameter::Path { parameter_data, .. } => {
                if let Some(param) =
                    parameter_to_param(generator, models, parameter_data, OperationParamLocation::Path)
                {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
            Parameter::Cookie { parameter_data, .. } => {
                if let Some(param) =
                    parameter_to_param(generator, models, parameter_data, OperationParamLocation::Cookie)
                {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
        }
    }

    out
}

fn parameter_to_param(
    generator: &ModelGenerator,
    models: &mut ModelRegistry,
    data: &openapiv3::ParameterData,
    location: OperationParamLocation,
) -> Option<OperationParam> {
    let (typ, render_type) = match &data.format {
        ParameterSchemaOrContent::Schema(schema) => {
            let typ = generator.schema_ref_to_type(schema).ok()?;
            let render_type = param_render_type(&typ, models);
            (typ, render_type)
        }
        ParameterSchemaOrContent::Content(content) => {
            let (content_type, media) = select_media_type(content)?;
            let schema = media.schema.as_ref()?;
            let typ = generator.schema_ref_to_type(schema).ok()?;
            let render_type = param_render_type(&typ, models);
            let _ = content_type;
            (typ, render_type)
        }
    };

    let (is_array, array_item_is_primitive) = param_type_meta(&typ, models);
    let is_display = is_display_type(&typ, models, &render_type);

    Some(OperationParam {
        name: data.name.clone(),
        rust_name: sanitize_field_name(&data.name),
        location,
        required: data.required || matches!(location, OperationParamLocation::Path),
        typ,
        render_type,
        is_array,
        array_item_is_primitive,
        is_display,
    })
}

fn param_type_meta(typ: &ModelType, models: &ModelRegistry) -> (bool, bool) {
    match typ {
        ModelType::Array(inner) => {
            let inner_render = param_render_type(inner, models);
            (true, is_display_type(inner, models, &inner_render))
        }
        _ => (false, false),
    }
}

fn is_display_type(typ: &ModelType, models: &ModelRegistry, render_type: &str) -> bool {
    if matches!(render_type, "String" | "i64" | "f64" | "bool") {
        return true;
    }
    match typ {
        ModelType::Primitive(_) | ModelType::Opaque => true,
        ModelType::Ref(name) => models
            .get(name)
            .map(|model| matches!(model.kind, ModelType::Primitive(_) | ModelType::Opaque))
            .unwrap_or(false),
        _ => false,
    }
}

fn param_render_type(typ: &ModelType, models: &ModelRegistry) -> String {
    match typ {
        ModelType::Primitive(_) | ModelType::Opaque => model_type_to_rust(typ),
        ModelType::Array(inner) => format!("Vec<{}>", param_render_type(inner, models)),
        ModelType::Ref(name) => {
            if let Some(model) = models.get(name) {
                return param_render_type(&model.kind, models);
            }
            model_type_to_rust(typ)
        }
        ModelType::Composite(comp) => {
            let mut prim: Option<crate::generator::model::PrimitiveType> = None;
            for variant in &comp.variants {
                let variant_prim = primitive_from_type(variant, models);
                let Some(variant_prim) = variant_prim else {
                    return model_type_to_rust(typ);
                };
                if let Some(existing) = prim {
                    if existing != variant_prim {
                        return model_type_to_rust(typ);
                    }
                } else {
                    prim = Some(variant_prim);
                }
            }
            match prim {
                Some(crate::generator::model::PrimitiveType::String) => "String".into(),
                Some(crate::generator::model::PrimitiveType::Integer) => "i64".into(),
                Some(crate::generator::model::PrimitiveType::Number) => "f64".into(),
                Some(crate::generator::model::PrimitiveType::Boolean) => "bool".into(),
                None => model_type_to_rust(typ),
            }
        }
        ModelType::Struct(_) => model_type_to_rust(typ),
    }
}

fn primitive_from_type(
    typ: &ModelType,
    models: &ModelRegistry,
) -> Option<crate::generator::model::PrimitiveType> {
    match typ {
        ModelType::Primitive(p) => Some(*p),
        ModelType::Ref(name) => models.get(name).and_then(|model| primitive_from_type(&model.kind, models)),
        _ => None,
    }
}

fn ensure_named_type(
    _generator: &ModelGenerator,
    models: &mut ModelRegistry,
    name_hint: &str,
    typ: ModelType,
) -> ModelType {
    if !needs_named_model(&typ) {
        return typ;
    }
    let base = sanitize_module_name(name_hint);
    if let Some(existing) = models.models.get(&base) {
        let existing_sig = type_signature(&existing.kind);
        let current_sig = type_signature(&typ);
        if existing_sig == current_sig {
            return ModelType::Ref(existing.name.clone());
        }
        let existing_fields = struct_field_set(&existing.kind);
        let current_fields = struct_field_set(&typ);
        if let (Some(existing_fields), Some(current_fields)) = (existing_fields, current_fields) {
            if let Some(suffix) = unique_field_suffix(&existing_fields, &current_fields) {
                let candidate = format!("{base}_{suffix}");
                if !models.models.contains_key(&candidate) {
                    let rust_name = sanitize_type_name(&candidate);
                    let def = ModelDef {
                        name: candidate.clone(),
                        rust_name: rust_name.clone(),
                        render_type: String::new(),
                        deps: Vec::new(),
                        dep_imports: Vec::new(),
                        kind: typ,
                    };
                    models.models.insert(candidate.clone(), def);
                    models.type_map.insert(rust_name, candidate.clone());
                    return ModelType::Ref(candidate);
                }
            }
        }
        tracing::debug!(
            name = %base,
            existing_sig = %existing_sig,
            current_sig = %current_sig,
            "operation model name collision"
        );
        panic!("operation model name collision for '{base}'");
    }
    let name = base.clone();
    let rust_name = sanitize_type_name(&name);
    let def = ModelDef {
        name: name.clone(),
        rust_name: rust_name.clone(),
        render_type: String::new(),
        deps: Vec::new(),
        dep_imports: Vec::new(),
        kind: typ,
    };
    models.models.insert(name.clone(), def);
    models.type_map.insert(rust_name, name.clone());
    ModelType::Ref(name)
}

fn needs_named_model(typ: &ModelType) -> bool {
    match typ {
        ModelType::Struct(_) | ModelType::Composite(_) => true,
        ModelType::Array(inner) => needs_named_model(inner),
        _ => false,
    }
}


fn response_suffix(status: &Option<StatusCode>) -> String {
    match status {
        Some(StatusCode::Code(code)) => format!("{code}"),
        Some(StatusCode::Range(range)) => format!("{range}xx"),
        None => "default".into(),
    }
}

fn op_short_name(op_id: &str, group: &str) -> String {
    let tokens = split_tokens(op_id);
    if tokens.is_empty() {
        return "op".into();
    }

    let group_tokens = split_tokens(group);
    let mut trimmed = tokens.as_slice();
    if !group_tokens.is_empty() && tokens.starts_with(group_tokens.as_slice()) {
        trimmed = &tokens[group_tokens.len()..];
    }
    if trimmed.len() < 2 {
        trimmed = tokens.as_slice();
    }

    let verb = trimmed.first().copied().unwrap_or("op");
    let resource = find_resource_token(trimmed).unwrap_or(verb);
    if verb == resource {
        verb.to_string()
    } else {
        format!("{verb}_{resource}")
    }
}

fn split_tokens(input: &str) -> Vec<&str> {
    input
        .split('_')
        .filter(|part| !part.is_empty())
        .collect()
}

fn find_resource_token<'a>(tokens: &'a [&'a str]) -> Option<&'a str> {
    const STOP: &[&str] = &["id", "ids", "by", "for", "with", "in", "on", "at", "of", "to", "from"];
    for token in tokens.iter().rev() {
        if !STOP.contains(token) {
            return Some(*token);
        }
    }
    tokens.last().copied()
}

fn unique_short_name(
    base: &str,
    op_id: &str,
    method: &str,
    path: &str,
    group: &str,
    seen: &mut std::collections::HashMap<String, indexmap::IndexSet<String>>,
) -> String {
    let entry = seen.entry(group.to_string()).or_default();
    let base = sanitize_module_name(base);
    let method = sanitize_module_name(method);
    let action_resource = action_resource_short(&method, path);
    let mut candidates = Vec::new();

    if !base.is_empty() {
        candidates.push(base.clone());
    }
    if !action_resource.is_empty() {
        candidates.push(action_resource.clone());
    }

    let path_tokens = path_literal_tokens(path);
    if !path_tokens.is_empty() {
        let len = path_tokens.len();
        let start = if len >= 2 { 2 } else { 1 };
        for suffix_len in start..=len {
            let suffix = path_tokens[len - suffix_len..].join("_");
            if !suffix.is_empty() {
                candidates.push(format!("{method}_{suffix}"));
            }
        }
    }

    if !base.is_empty() {
        candidates.push(format!("{method}_{base}"));
        candidates.push(format!("{base}_{method}"));
    }
    if !action_resource.is_empty() {
        candidates.push(format!("{method}_{action_resource}"));
    }

    let mut seen_local = std::collections::HashSet::new();
    for candidate in candidates.iter().cloned() {
        if candidate.is_empty() || !seen_local.insert(candidate.clone()) {
            continue;
        }
        if entry.insert(candidate.clone()) {
            return candidate;
        }
    }

    tracing::debug!(
        group = %group,
        op_id = %op_id,
        method = %method,
        path = %path,
        base = %base,
        candidates = ?candidates,
        "operation short name collision could not be resolved without suffixes"
    );
    panic!(
        "operation name collision in group '{group}' for op '{op_id}' (base '{base}')"
    );
}

fn unique_model_prefix(
    short_name: &str,
    group: &str,
    method: &str,
    path: &str,
    seen: &mut HashSet<String>,
) -> String {
    let short_name = sanitize_module_name(short_name);
    let group = sanitize_module_name(group);
    let method = sanitize_module_name(method);
    let mut candidates = Vec::new();

    if !short_name.is_empty() {
        candidates.push(short_name.clone());
    }
    if !group.is_empty() && !short_name.is_empty() {
        candidates.push(format!("{group}_{short_name}"));
    }

    let path_tokens = path_literal_tokens(path);
    if !path_tokens.is_empty() {
        let len = path_tokens.len();
        let start = if len >= 2 { 2 } else { 1 };
        for suffix_len in start..=len {
            let suffix = path_tokens[len - suffix_len..].join("_");
            if suffix.is_empty() {
                continue;
            }
            if !group.is_empty() {
                candidates.push(format!("{group}_{method}_{suffix}"));
            } else {
                candidates.push(format!("{method}_{suffix}"));
            }
        }
    }

    if !group.is_empty() && !short_name.is_empty() {
        candidates.push(format!("{group}_{method}_{short_name}"));
    }

    let mut seen_local = std::collections::HashSet::new();
    for candidate in candidates.iter().cloned() {
        if candidate.is_empty() || !seen_local.insert(candidate.clone()) {
            continue;
        }
        if seen.insert(candidate.clone()) {
            return candidate;
        }
    }

    tracing::debug!(
        short_name = %short_name,
        group = %group,
        method = %method,
        path = %path,
        candidates = ?candidates,
        "operation model prefix collision could not be resolved without suffixes"
    );
    panic!("operation model name collision for short name '{short_name}'");
}

fn action_resource_short(method: &str, path: &str) -> String {
    let method = sanitize_module_name(method);
    let (resource, ends_with_param) = path_resource_token(path);
    let action = match method.as_str() {
        "get" => {
            if ends_with_param {
                "get"
            } else {
                "list"
            }
        }
        "post" => {
            if ends_with_param {
                "post"
            } else {
                "create"
            }
        }
        "put" | "patch" => "update",
        "delete" => "delete",
        _ => method.as_str(),
    };
    if resource.is_empty() {
        action.to_string()
    } else {
        format!("{action}_{resource}")
    }
}

fn path_literal_tokens(path: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if is_param_segment(seg) {
            continue;
        }
        let sanitized = sanitize_module_name(seg);
        if !sanitized.is_empty() {
            tokens.push(sanitized);
        }
    }
    tokens
}

fn path_resource_token(path: &str) -> (String, bool) {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let ends_with_param = segments
        .last()
        .map(|seg| is_param_segment(seg))
        .unwrap_or(false);
    let mut last_literal: Option<&str> = None;
    for seg in segments.iter().rev() {
        if is_param_segment(seg) {
            continue;
        }
        last_literal = Some(*seg);
        break;
    }
    let resource = last_literal
        .map(|seg| singularize(&sanitize_module_name(seg)))
        .unwrap_or_else(|| "resource".to_string());
    (resource, ends_with_param)
}

fn is_param_segment(seg: &str) -> bool {
    seg.starts_with('{') && seg.ends_with('}')
}

fn singularize(value: &str) -> String {
    if value.ends_with('s') && value.len() > 1 {
        value[..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn struct_field_set(typ: &ModelType) -> Option<BTreeSet<String>> {
    match typ {
        ModelType::Struct(def) => {
            let mut fields = BTreeSet::new();
            for field in &def.fields {
                let name = sanitize_module_name(&field.name);
                if !name.is_empty() {
                    fields.insert(name);
                }
            }
            Some(fields)
        }
        _ => None,
    }
}

fn unique_field_suffix(
    existing_fields: &BTreeSet<String>,
    current_fields: &BTreeSet<String>,
) -> Option<String> {
    let unique: BTreeSet<String> = current_fields
        .difference(existing_fields)
        .cloned()
        .collect();
    if unique.is_empty() {
        return None;
    }
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    for field in unique {
        let sanitized = sanitize_module_name(&field);
        if sanitized.is_empty() {
            continue;
        }
        for token in sanitized.split('_') {
            if !token.is_empty() {
                candidates.insert(token.to_string());
            }
        }
        candidates.insert(sanitized);
    }
    let mut out: Vec<String> = candidates.into_iter().collect();
    out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    out.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use openapiv3::{
        MediaType, Parameter, ParameterData, ParameterSchemaOrContent, Paths, ReferenceOr,
        RequestBody, Response, Schema, StatusCode, Type,
    };

    fn make_doc() -> OpenAPI {
        OpenAPI {
            paths: Paths::default(),
            ..OpenAPI::default()
        }
    }

    #[test]
    fn collects_operations_with_ids() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();
        op.operation_id = Some("get_user".into());
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/users/{id}".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("get_user").expect("operation");
        assert_eq!(op.method, "get");
        assert_eq!(op.path, "/users/{id}");
    }

    #[test]
    fn falls_back_to_method_path_id() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        item.post = Some(Operation::default());
        doc.paths
            .paths
            .insert("/widgets/{id}".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("post_widgets_id").expect("operation");
        assert_eq!(op.method, "post");
        assert_eq!(op.path, "/widgets/{id}");
    }

    #[test]
    fn disambiguates_duplicate_ids() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item_a = PathItem::default();
        let mut op_a = Operation::default();
        op_a.operation_id = Some("list".into());
        item_a.get = Some(op_a);
        doc.paths
            .paths
            .insert("/a".into(), ReferenceOr::Item(item_a));

        let mut item_b = PathItem::default();
        let mut op_b = Operation::default();
        op_b.operation_id = Some("list".into());
        item_b.get = Some(op_b);
        doc.paths
            .paths
            .insert("/b".into(), ReferenceOr::Item(item_b));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        assert!(registry.get("list").is_some());
        assert!(registry.get("list_2").is_some());
    }

    #[test]
    fn assigns_group_from_tag() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();
        op.tags = vec!["Audit Logs".into()];
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/audit".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("get_audit").expect("operation");
        assert_eq!(op.group, "audit_logs");
    }

    #[test]
    fn collects_parameters_from_path_and_operation() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        let path_param = Parameter::Path {
            parameter_data: ParameterData {
                name: "id".into(),
                description: None,
                required: true,
                deprecated: None,
                format: ParameterSchemaOrContent::Schema(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::String(Default::default())),
                })),
                example: None,
                examples: IndexMap::new(),
                explode: None,
                extensions: IndexMap::new(),
            },
            style: Default::default(),
        };
        item.parameters.push(ReferenceOr::Item(path_param));

        let query_param = Parameter::Query {
            parameter_data: ParameterData {
                name: "limit".into(),
                description: None,
                required: false,
                deprecated: None,
                format: ParameterSchemaOrContent::Schema(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::Integer(Default::default())),
                })),
                example: None,
                examples: IndexMap::new(),
                explode: None,
                extensions: IndexMap::new(),
            },
            allow_reserved: false,
            style: Default::default(),
            allow_empty_value: None,
        };
        op.parameters.push(ReferenceOr::Item(query_param));

        item.get = Some(op);
        doc.paths
            .paths
            .insert("/items/{id}".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("get_items_id").expect("operation");
        assert_eq!(op.params.len(), 2);
        let path = op
            .params
            .iter()
            .find(|param| param.location == OperationParamLocation::Path)
            .unwrap();
        assert_eq!(path.name, "id");
        assert!(path.required);
        assert_eq!(path.render_type, "String");

        let query = op
            .params
            .iter()
            .find(|param| param.location == OperationParamLocation::Query)
            .unwrap();
        assert_eq!(query.name, "limit");
        assert!(!query.required);
        assert_eq!(query.render_type, "i64");
    }

    #[test]
    fn selects_json_request_body_type() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();
        let mut body = RequestBody::default();
        body.required = true;

        let plain_schema = Schema {
            schema_data: Default::default(),
            schema_kind: openapiv3::SchemaKind::Type(Type::String(Default::default())),
        };
        let json_schema = Schema {
            schema_data: Default::default(),
            schema_kind: openapiv3::SchemaKind::Type(Type::Integer(Default::default())),
        };

        body.content.insert(
            "text/plain".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(plain_schema)),
                ..Default::default()
            },
        );
        body.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(json_schema)),
                ..Default::default()
            },
        );
        op.request_body = Some(ReferenceOr::Item(body));
        item.post = Some(op);
        doc.paths
            .paths
            .insert("/widgets".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("post_widgets").expect("operation");
        let body = op.request_body.as_ref().expect("body");
        assert_eq!(body.content_type, "application/json");
        assert_eq!(body.render_type, "i64");
        assert!(body.required);
    }

    #[test]
    fn collects_response_types() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        let mut response = Response::default();
        response.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::Boolean(Default::default())),
                })),
                ..Default::default()
            },
        );
        op.responses
            .responses
            .insert(StatusCode::Code(200), ReferenceOr::Item(response));
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/status".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("get_status").expect("operation");
        assert_eq!(op.responses.len(), 1);
        let resp = &op.responses[0];
        assert_eq!(resp.status, Some(StatusCode::Code(200)));
        assert_eq!(resp.content_type.as_deref(), Some("application/json"));
        assert_eq!(resp.render_type, "bool");
    }

    #[test]
    fn builds_response_enum_variants() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        let mut response = Response::default();
        response.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::Boolean(Default::default())),
                })),
                ..Default::default()
            },
        );
        op.responses
            .responses
            .insert(StatusCode::Code(200), ReferenceOr::Item(response));
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/status".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("get_status").expect("operation");
        let response_enum = &op.response_enum;
        assert_eq!(response_enum.name, "GetStatusResponse");
        assert_eq!(response_enum.variants.len(), 1);
        assert_eq!(response_enum.variants[0].name, "Status200");
        assert_eq!(response_enum.variants[0].render_type, "bool");
        assert_eq!(response_enum.variants[0].status_match, "200");
        assert!(!response_enum.variants[0].is_default);
        assert!(!response_enum.has_default);
    }

    #[test]
    fn collects_operation_deps() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        let mut response = Response::default();
        response.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Reference {
                    reference: "#/components/schemas/Widget".into(),
                }),
                ..Default::default()
            },
        );
        op.responses
            .responses
            .insert(StatusCode::Code(200), ReferenceOr::Item(response));
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/widgets".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        let op = registry.get("get_widgets").expect("operation");
        assert_eq!(op.deps, vec!["Widget"]);
    }
}
