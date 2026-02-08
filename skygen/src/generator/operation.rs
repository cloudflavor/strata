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
    sanitize_type_name, DepImport, ModelGenerator, ModelRegistry, ModelType,
};
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
        models: &ModelRegistry,
    ) -> Result<OperationRegistry> {
        let mut registry = OperationRegistry::default();
        let mut seen_ids = indexmap::IndexSet::new();

        for (path, entry) in doc.paths.paths.iter() {
            let item = match entry {
                ReferenceOr::Item(item) => item,
                ReferenceOr::Reference { reference } => {
                    return Err(anyhow!("unexpected path reference {reference}"));
                }
            };

            self.collect_from_item(path, item, models, &mut registry, &mut seen_ids)?;
        }

        Ok(registry)
    }

    fn collect_from_item(
        &self,
        path: &str,
        item: &PathItem,
        models: &ModelRegistry,
        registry: &mut OperationRegistry,
        seen_ids: &mut indexmap::IndexSet<String>,
    ) -> Result<()> {
        if let Some(op) = item.get.as_ref() {
            self.push_op("get", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.put.as_ref() {
            self.push_op("put", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.post.as_ref() {
            self.push_op("post", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.delete.as_ref() {
            self.push_op("delete", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.options.as_ref() {
            self.push_op("options", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.head.as_ref() {
            self.push_op("head", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.patch.as_ref() {
            self.push_op("patch", path, item, op, models, registry, seen_ids);
        }
        if let Some(op) = item.trace.as_ref() {
            self.push_op("trace", path, item, op, models, registry, seen_ids);
        }

        Ok(())
    }

    fn push_op(
        &self,
        method: &str,
        path: &str,
        item: &PathItem,
        op: &Operation,
        models: &ModelRegistry,
        registry: &mut OperationRegistry,
        seen_ids: &mut indexmap::IndexSet<String>,
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
            params: Vec::new(),
            has_path_params: false,
            has_query_params: false,
            has_header_params: false,
            has_cookie_params: false,
        };

        let model_gen = ModelGenerator::with_module_name_map(models.name_map.clone());

        def.params = collect_parameters(
            &model_gen,
            models,
            item.parameters.iter().chain(op.parameters.iter()),
        );
        for param in &def.params {
            match param.location {
                OperationParamLocation::Path => def.has_path_params = true,
                OperationParamLocation::Query => def.has_query_params = true,
                OperationParamLocation::Header => def.has_header_params = true,
                OperationParamLocation::Cookie => def.has_cookie_params = true,
            }
        }

        if let Some(body) = op.request_body.as_ref() {
            if let Ok(Some(body)) = request_body_to_type(&model_gen, body) {
                def.request_body = Some(body);
            }
        }

        def.responses = responses_to_types(&model_gen, &op.responses);
        def.response_enum = build_response_enum(&def);
        def.deps = collect_operation_deps(&def);
        def.dep_imports = group_dep_imports(&def.deps, &models.type_map);

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
    pub params: Vec<OperationParam>,
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

#[derive(Debug, Clone, Serialize)]
pub struct OperationResponse {
    pub status: Option<StatusCode>,
    pub content_type: Option<String>,
    pub typ: ModelType,
    pub render_type: String,
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
    responses: &openapiv3::Responses,
) -> Vec<OperationResponse> {
    let mut out = Vec::new();

    if let Some(default) = responses.default.as_ref() {
        if let Some(resp) = response_to_type(generator, None, default) {
            out.push(resp);
        }
    }

    for (status, response) in responses.responses.iter() {
        if let Some(resp) = response_to_type(generator, Some(status.clone()), response) {
            out.push(resp);
        }
    }

    out
}

fn response_to_type(
    generator: &ModelGenerator,
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
            render_type: "serde_json::Value".into(),
        });
    }

    let (content_type, media) = select_media_type(&response.content)?;
    let schema = media.schema.as_ref()?;
    let typ = generator.schema_ref_to_type(schema).ok()?;
    let render_type = model_type_to_rust(&typ);

    Some(OperationResponse {
        status,
        content_type: Some(content_type),
        typ,
        render_type,
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
    let name = format!("{}Response", sanitize_type_name(&def.id));
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
    models: &ModelRegistry,
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
                if let Some(param) = parameter_to_param(
                    generator,
                    models,
                    parameter_data,
                    OperationParamLocation::Query,
                ) {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
            Parameter::Header { parameter_data, .. } => {
                if let Some(param) = parameter_to_param(
                    generator,
                    models,
                    parameter_data,
                    OperationParamLocation::Header,
                ) {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
            Parameter::Path { parameter_data, .. } => {
                if let Some(param) = parameter_to_param(
                    generator,
                    models,
                    parameter_data,
                    OperationParamLocation::Path,
                ) {
                    let key = (param.location, param.name.clone());
                    if seen.insert(key) {
                        out.push(param);
                    }
                }
            }
            Parameter::Cookie { parameter_data, .. } => {
                if let Some(param) = parameter_to_param(
                    generator,
                    models,
                    parameter_data,
                    OperationParamLocation::Cookie,
                ) {
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
    models: &ModelRegistry,
    data: &openapiv3::ParameterData,
    location: OperationParamLocation,
) -> Option<OperationParam> {
    let (typ, render_type) = match &data.format {
        ParameterSchemaOrContent::Schema(schema) => {
            let typ = generator.schema_ref_to_type(schema).ok()?;
            let render_type = model_type_to_rust(&typ);
            (typ, render_type)
        }
        ParameterSchemaOrContent::Content(content) => {
            let (content_type, media) = select_media_type(content)?;
            let schema = media.schema.as_ref()?;
            let typ = generator.schema_ref_to_type(schema).ok()?;
            let render_type = model_type_to_rust(&typ);
            let _ = content_type;
            (typ, render_type)
        }
    };

    let (is_array, array_item_is_primitive) = param_type_meta(&typ, models);

    Some(OperationParam {
        name: data.name.clone(),
        rust_name: sanitize_field_name(&data.name),
        location,
        required: data.required || matches!(location, OperationParamLocation::Path),
        typ,
        render_type,
        is_array,
        array_item_is_primitive,
    })
}

fn param_type_meta(typ: &ModelType, models: &ModelRegistry) -> (bool, bool) {
    match typ {
        ModelType::Array(inner) => (true, is_display_type(inner, models)),
        _ => (false, false),
    }
}

fn is_display_type(typ: &ModelType, models: &ModelRegistry) -> bool {
    match typ {
        ModelType::Primitive(_) | ModelType::Opaque => true,
        ModelType::Ref(name) => models
            .get(name)
            .map(|model| matches!(model.kind, ModelType::Primitive(_) | ModelType::Opaque))
            .unwrap_or(false),
        _ => false,
    }
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
        let models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();
        op.operation_id = Some("get_user".into());
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/users/{id}".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &models)
            .expect("collect");
        let op = registry.get("get_user").expect("operation");
        assert_eq!(op.method, "get");
        assert_eq!(op.path, "/users/{id}");
    }

    #[test]
    fn falls_back_to_method_path_id() {
        let mut doc = make_doc();
        let models = ModelRegistry::default();
        let mut item = PathItem::default();
        item.post = Some(Operation::default());
        doc.paths
            .paths
            .insert("/widgets/{id}".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &models)
            .expect("collect");
        let op = registry.get("post_widgets_id").expect("operation");
        assert_eq!(op.method, "post");
        assert_eq!(op.path, "/widgets/{id}");
    }

    #[test]
    fn disambiguates_duplicate_ids() {
        let mut doc = make_doc();
        let models = ModelRegistry::default();
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
            .collect_operations(&doc, &models)
            .expect("collect");
        assert!(registry.get("list").is_some());
        assert!(registry.get("list_2").is_some());
    }

    #[test]
    fn assigns_group_from_tag() {
        let mut doc = make_doc();
        let models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();
        op.tags = vec!["Audit Logs".into()];
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/audit".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &models)
            .expect("collect");
        let op = registry.get("get_audit").expect("operation");
        assert_eq!(op.group, "audit_logs");
    }

    #[test]
    fn collects_parameters_from_path_and_operation() {
        let mut doc = make_doc();
        let models = ModelRegistry::default();
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
            .collect_operations(&doc, &models)
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
        let models = ModelRegistry::default();
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
            .collect_operations(&doc, &models)
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
        let models = ModelRegistry::default();
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
            .collect_operations(&doc, &models)
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
        let models = ModelRegistry::default();
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
            .collect_operations(&doc, &models)
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
        let models = ModelRegistry::default();
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
            .collect_operations(&doc, &models)
            .expect("collect");
        let op = registry.get("get_widgets").expect("operation");
        assert_eq!(op.deps, vec!["Widget"]);
    }
}
