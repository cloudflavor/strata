// Copyright 2026 Cloudflavor GmbH

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at

// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use openapiv3::{
    AdditionalProperties, AnySchema, Callback, Example, Header, Link, LinkOperation, MediaType,
    OpenAPI, Parameter, ParameterSchemaOrContent, PathItem, ReferenceOr, RequestBody, Response,
    Responses, Schema, SchemaKind, SecurityScheme, Type,
};
use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};

#[derive(Debug)]
pub(crate) struct ResolvedCache<'a> {
    paths: HashMap<&'a str, &'a PathItem>,
    params: HashMap<&'a str, &'a Parameter>,
    schemas: HashMap<&'a str, &'a Schema>,
    responses: HashMap<String, &'a Response>,
    headers: HashMap<&'a str, &'a Header>,
    callbacks: HashMap<&'a str, &'a Callback>,
    req_bodies: HashMap<RequestBodyKey<'a>, &'a RequestBody>,
    examples: HashMap<&'a str, &'a Example>,
    links: HashMap<&'a str, &'a Link>,
    security_schemes: HashMap<&'a str, &'a SecurityScheme>,
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Patch,
    Options,
    Head,
    Trace,
}

impl HttpMethod {
    fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "get" => Some(Self::Get),
            "put" => Some(Self::Put),
            "post" => Some(Self::Post),
            "delete" => Some(Self::Delete),
            "patch" => Some(Self::Patch),
            "options" => Some(Self::Options),
            "head" => Some(Self::Head),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq)]
enum RequestBodyKey<'a> {
    Component(&'a str),
    Operation { path: &'a str, method: HttpMethod },
}

#[derive(Clone, Copy)]
struct OperationLocator<'a> {
    path: &'a str,
    method: HttpMethod,
}

pub struct EagerResolver<'a> {
    spec: &'a OpenAPI,
    // resolved: Option<ResolvedSpec>,
    cache: ResolvedCache<'a>,
    visited_refs: HashSet<&'a str>,
    operation_ids: HashMap<&'a str, OperationLocator<'a>>,
}

impl<'a> EagerResolver<'a> {
    pub fn new(spec: &'a OpenAPI) -> Self {
        let paths_cap = spec.paths.paths.len();
        let (
            param_cap,
            schema_cap,
            response_cap,
            header_cap,
            callback_cap,
            rbody_cap,
            example_cap,
            link_cap,
            security_cap,
        ) = spec
            .components
            .as_ref()
            .map(|c| {
                (
                    c.parameters.len(),
                    c.schemas.len(),
                    c.responses.len(),
                    c.headers.len(),
                    c.callbacks.len(),
                    c.request_bodies.len(),
                    c.examples.len(),
                    c.links.len(),
                    c.security_schemes.len(),
                )
            })
            .unwrap_or_default();

        Self {
            spec,
            // resolved: None,
            cache: ResolvedCache {
                req_bodies: HashMap::with_capacity(rbody_cap),
                paths: HashMap::with_capacity(paths_cap),
                params: HashMap::with_capacity(param_cap),
                schemas: HashMap::with_capacity(schema_cap),
                responses: HashMap::with_capacity(response_cap),
                headers: HashMap::with_capacity(header_cap),
                callbacks: HashMap::with_capacity(callback_cap),
                examples: HashMap::with_capacity(example_cap),
                links: HashMap::with_capacity(link_cap),
                security_schemes: HashMap::with_capacity(security_cap),
            },
            visited_refs: HashSet::new(),
            operation_ids: HashMap::new(),
        }
    }

    pub fn resolve(&mut self) -> Result<()> {
        self.resolve_paths()
            .with_context(|| "failed to resolve paths")?;
        debug!("{:#?}", self.cache.req_bodies);

        Ok(())
    }

    fn resolve_paths(&mut self) -> Result<()> {
        for (name, path_item) in self.spec.paths.iter() {
            match path_item {
                ReferenceOr::Item(item) => {
                    self.record_operation_ids(name.as_str(), item);
                    self.resolve_operation(name.as_str(), item)?;
                    self.cache.paths.insert(name, item);
                }
                ReferenceOr::Reference { reference } => {
                    self.resolve_ref(reference)?;
                }
            }
        }
        Ok(())
    }

    fn resolve_operation(&mut self, path: &'a str, path_item: &'a PathItem) -> Result<()> {
        for (method, op) in iterate_operations(path_item) {
            for param in &op.parameters {
                self.resolve_parameter_or(param)?;
            }
            self.resolve_responses(&op.responses)?;
            self.resolve_req_body(path, method, op.request_body.as_ref())?;
            for callback in op.callbacks.values() {
                self.resolve_callback(callback)?;
            }
        }

        Ok(())
    }

    fn record_operation_ids(&mut self, path: &'a str, path_item: &'a PathItem) {
        for (method, operation) in iterate_operations(path_item) {
            if let Some(operation_id) = operation.operation_id.as_deref() {
                self.operation_ids
                    .insert(operation_id, OperationLocator { path, method });
            }
        }
    }

    fn resolve_responses(&mut self, responses: &'a Responses) -> Result<()> {
        if let Some(default_resp) = responses.default.as_ref() {
            match default_resp {
                ReferenceOr::Item(item) => {
                    self.resolve_response(item)?;
                    self.cache.responses.insert("default".to_string(), item);
                }
                ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref())?,
            }
        }

        for (status, resp) in responses.responses.iter() {
            match resp {
                ReferenceOr::Item(item) => {
                    self.resolve_response(item)?;
                    self.cache.responses.insert(status.to_string(), item);
                }
                ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref())?,
            }
        }

        Ok(())
    }

    fn resolve_response(&mut self, response: &'a Response) -> Result<()> {
        self.resolve_headers(response.headers.iter())?;
        for media_type in response.content.values() {
            self.resolve_media_type(media_type)?;
        }
        for link in response.links.values() {
            self.resolve_link_or(link)?;
        }
        Ok(())
    }

    fn resolve_headers<I>(&mut self, headers: I) -> Result<()>
    where
        I: Iterator<Item = (&'a String, &'a ReferenceOr<Header>)>,
    {
        for (name, header) in headers {
            match header {
                ReferenceOr::Item(item) => {
                    self.cache.headers.insert(name, item);
                    self.resolve_header(item)?;
                }
                ReferenceOr::Reference { reference } => self.resolve_ref(reference)?,
            }
        }
        Ok(())
    }

    fn resolve_header(&mut self, header: &'a Header) -> Result<()> {
        match &header.format {
            ParameterSchemaOrContent::Schema(schema) => self.resolve_schema_ref(schema),
            ParameterSchemaOrContent::Content(content) => {
                for media in content.values() {
                    self.resolve_media_type(media)?;
                }
                Ok(())
            }
        }?;
        for example in header.examples.values() {
            self.resolve_example_or(example)?;
        }
        Ok(())
    }

    fn resolve_parameter_or(&mut self, parameter: &'a ReferenceOr<Parameter>) -> Result<()> {
        match parameter {
            ReferenceOr::Item(item) => self.resolve_parameter(item),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_req_body(
        &mut self,
        path: &'a str,
        method: HttpMethod,
        req_body: Option<&'a ReferenceOr<RequestBody>>,
    ) -> Result<()> {
        let Some(req_body) = req_body else {
            return Ok(());
        };

        match req_body {
            ReferenceOr::Item(item) => {
                self.cache.req_bodies.insert(
                    RequestBodyKey::Operation { path, method },
                    item,
                );
                self.resolve_request_body(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_parameter(&mut self, parameter: &'a Parameter) -> Result<()> {
        let data = parameter.parameter_data_ref();
        match &data.format {
            ParameterSchemaOrContent::Schema(schema) => self.resolve_schema_ref(schema),
            ParameterSchemaOrContent::Content(content) => {
                for media in content.values() {
                    self.resolve_media_type(media)?;
                }
                Ok(())
            }
        }?;
        for example in data.examples.values() {
            self.resolve_example_or(example)?;
        }
        Ok(())
    }

    fn resolve_media_type(&mut self, media_type: &'a MediaType) -> Result<()> {
        if let Some(schema) = media_type.schema.as_ref() {
            self.resolve_schema_ref(schema)?;
        }
        for encoding in media_type.encoding.values() {
            self.resolve_headers(encoding.headers.iter())?;
        }
        for example in media_type.examples.values() {
            self.resolve_example_or(example)?;
        }
        Ok(())
    }

    fn resolve_request_body(&mut self, body: &'a RequestBody) -> Result<()> {
        for media_type in body.content.values() {
            self.resolve_media_type(media_type)?;
        }
        Ok(())
    }

    fn resolve_callback(&mut self, callback: &'a Callback) -> Result<()> {
        for (expression, path_item) in callback.iter() {
            self.resolve_operation(expression.as_str(), path_item)?;
        }
        Ok(())
    }

    fn resolve_example_or(&mut self, example: &'a ReferenceOr<Example>) -> Result<()> {
        match example {
            ReferenceOr::Item(_) => Ok(()),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_link_or(&mut self, link: &'a ReferenceOr<Link>) -> Result<()> {
        match link {
            ReferenceOr::Item(item) => self.resolve_link(item),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_link(&mut self, link: &'a Link) -> Result<()> {
        match &link.operation {
            LinkOperation::OperationRef(reference) => {
                self.resolve_operation_reference(reference.as_str())
            }
            LinkOperation::OperationId(operation_id) => {
                self.resolve_operation_id(operation_id.as_str())
            }
        }
    }

    fn resolve_schema_ref(&mut self, schema: &'a ReferenceOr<Schema>) -> Result<()> {
        match schema {
            ReferenceOr::Item(item) => self.resolve_schema(item),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_boxed_schema_ref(
        &mut self,
        schema: &'a ReferenceOr<Box<Schema>>,
    ) -> Result<()> {
        match schema {
            ReferenceOr::Item(item) => self.resolve_schema(item.as_ref()),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_schema(&mut self, schema: &'a Schema) -> Result<()> {
        match &schema.schema_kind {
            SchemaKind::Type(t) => self.resolve_type(t),
            SchemaKind::OneOf { one_of } => {
                for s in one_of {
                    self.resolve_schema_ref(s)?;
                }
                Ok(())
            }
            SchemaKind::AllOf { all_of } => {
                for s in all_of {
                    self.resolve_schema_ref(s)?;
                }
                Ok(())
            }
            SchemaKind::AnyOf { any_of } => {
                for s in any_of {
                    self.resolve_schema_ref(s)?;
                }
                Ok(())
            }
            SchemaKind::Not { not } => self.resolve_schema_ref(not.as_ref()),
            SchemaKind::Any(any) => self.resolve_any_schema(any),
        }
    }

    fn resolve_type(&mut self, schema_type: &'a Type) -> Result<()> {
        match schema_type {
            Type::Array(array) => {
                if let Some(items) = &array.items {
                    self.resolve_boxed_schema_ref(items)?;
                }
                Ok(())
            }
            Type::Object(object) => {
                for property in object.properties.values() {
                    self.resolve_boxed_schema_ref(property)?;
                }
                if let Some(additional) = &object.additional_properties {
                    if let AdditionalProperties::Schema(schema) = additional {
                        self.resolve_schema_ref(schema.as_ref())?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn resolve_any_schema(&mut self, schema: &'a AnySchema) -> Result<()> {
        for property in schema.properties.values() {
            self.resolve_boxed_schema_ref(property)?;
        }
        if let Some(additional) = &schema.additional_properties {
            if let AdditionalProperties::Schema(schema) = additional {
                self.resolve_schema_ref(schema.as_ref())?;
            }
        }
        if let Some(items) = &schema.items {
            self.resolve_boxed_schema_ref(items)?;
        }
        for s in &schema.one_of {
            self.resolve_schema_ref(s)?;
        }
        for s in &schema.all_of {
            self.resolve_schema_ref(s)?;
        }
        for s in &schema.any_of {
            self.resolve_schema_ref(s)?;
        }
        if let Some(not) = &schema.not {
            self.resolve_schema_ref(not.as_ref())?;
        }
        Ok(())
    }

    fn resolve_parameter_ref(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.parameters[name] {
            ReferenceOr::Item(item) => {
                self.cache.params.insert(name, item);
                self.resolve_parameter(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_ref(&mut self, reference: &'a str) -> Result<()> {
        if !self.visited_refs.insert(reference) {
            return Ok(());
        }

        let result = match ComponentRef::parse(reference) {
            Some(ComponentRef::Parameter(name)) => self.resolve_parameter_ref(name),
            Some(ComponentRef::Response(name)) => self.resolve_response_ref(name),
            Some(ComponentRef::Header(name)) => self.resolve_header_ref(name),
            Some(ComponentRef::Schema(name)) => self.resolve_schema_component(name),
            Some(ComponentRef::Callback(name)) => self.resolve_callback_ref(name),
            Some(ComponentRef::RequestBody(name)) => self.resolve_request_body_component(name),
            Some(ComponentRef::Example(name)) => self.resolve_example_component(name),
            Some(ComponentRef::Link(name)) => self.resolve_link_component(name),
            Some(ComponentRef::SecurityScheme(name)) => {
                self.resolve_security_scheme_component(name)
            }
            None => {
                warn!("unresolved component reference: {reference}");
                Err(anyhow!("unresolved component reference: {reference} "))
            }
        };

        if result.is_err() {
            self.visited_refs.remove(reference);
        }

        result
    }

    fn resolve_response_ref(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.responses[name] {
            ReferenceOr::Item(item) => {
                self.cache.responses.insert(name.to_string(), &item);
                self.resolve_response(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_header_ref(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.headers[name] {
            ReferenceOr::Item(item) => {
                self.cache.headers.insert(name, &item);
                self.resolve_header(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_schema_component(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.schemas[name] {
            ReferenceOr::Item(item) => {
                self.cache.schemas.insert(name, &item);
                self.resolve_schema(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_callback_ref(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.callbacks[name] {
            ReferenceOr::Item(item) => {
                self.cache.callbacks.insert(name, &item);
                self.resolve_callback(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_request_body_component(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.request_bodies[name] {
            ReferenceOr::Item(item) => {
                self.cache
                    .req_bodies
                    .insert(RequestBodyKey::Component(name), item);
                self.resolve_request_body(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_example_component(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.examples[name] {
            ReferenceOr::Item(item) => {
                self.cache.examples.insert(name, item);
                Ok(())
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_link_component(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.links[name] {
            ReferenceOr::Item(item) => {
                self.cache.links.insert(name, item);
                self.resolve_link(item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_security_scheme_component(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.security_schemes[name] {
            ReferenceOr::Item(item) => {
                self.cache.security_schemes.insert(name, item);
                Ok(())
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_operation_id(&mut self, operation_id: &'a str) -> Result<()> {
        let locator = self.operation_ids.get(operation_id).ok_or_else(|| {
            anyhow!("link references unknown operationId: {operation_id}")
        })?;

        let path_entry = self
            .spec
            .paths
            .paths
            .get(locator.path)
            .ok_or_else(|| anyhow!("operationId points to missing path: {}", locator.path))?;

        match path_entry {
            ReferenceOr::Item(item) => {
                self.resolve_single_operation(locator.method, locator.path, item)
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_operation_reference(&mut self, reference: &'a str) -> Result<()> {
        let pointer = match reference.strip_prefix('#') {
            Some(pointer) => pointer,
            None => return Ok(()),
        };

        self.resolve_operation_pointer(pointer)
    }

    fn resolve_operation_pointer(&mut self, pointer: &'a str) -> Result<()> {
        let mut segments = pointer.split('/').filter(|segment| !segment.is_empty());

        match segments.next() {
            Some("paths") => {}
            _ => {
                warn!("link operationRef points outside /paths: #{pointer}");
                return Ok(());
            }
        }

        let path_segment = segments
            .next()
            .ok_or_else(|| anyhow!("link operationRef missing path segment: #{pointer}"))?;
        let method_segment = segments
            .next()
            .ok_or_else(|| anyhow!("link operationRef missing method segment: #{pointer}"))?;

        if segments.next().is_some() {
            warn!("link operationRef has extra pointer segments: #{pointer}");
        }

        let decoded_path = decode_pointer_segment(path_segment);
        let (path_name, path_item) = self
            .spec
            .paths
            .paths
            .get_key_value(decoded_path.as_str())
            .ok_or_else(|| anyhow!("link references unknown path: {decoded_path}"))?;
        let method = HttpMethod::from_str(method_segment)
            .ok_or_else(|| anyhow!("link references unsupported HTTP method: {method_segment}"))?;

        match path_item {
            ReferenceOr::Item(item) => self.resolve_single_operation(method, path_name.as_str(), item),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_single_operation(
        &mut self,
        method: HttpMethod,
        path: &'a str,
        path_item: &'a PathItem,
    ) -> Result<()> {
        if let Some(operation) = find_operation_by_method(path_item, method) {
            for param in &operation.parameters {
                self.resolve_parameter_or(param)?;
            }
            self.resolve_responses(&operation.responses)?;
            self.resolve_req_body(path, method, operation.request_body.as_ref())?;
            for callback in operation.callbacks.values() {
                self.resolve_callback(callback)?;
            }
        }

        Ok(())
    }
}

fn decode_pointer_segment(segment: &str) -> String {
    let mut result = String::with_capacity(segment.len());
    let mut chars = segment.chars();

    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') => result.push('~'),
                Some('1') => result.push('/'),
                Some(other) => {
                    result.push('~');
                    result.push(other);
                }
                None => result.push('~'),
            }
        } else {
            result.push(ch);
        }
    }

    result
}

fn iterate_operations<'a>(
    path_item: &'a PathItem,
) -> impl Iterator<Item = (HttpMethod, &'a openapiv3::Operation)> {
    [
        (HttpMethod::Get, path_item.get.as_ref()),
        (HttpMethod::Put, path_item.put.as_ref()),
        (HttpMethod::Post, path_item.post.as_ref()),
        (HttpMethod::Delete, path_item.delete.as_ref()),
        (HttpMethod::Patch, path_item.patch.as_ref()),
        (HttpMethod::Options, path_item.options.as_ref()),
        (HttpMethod::Head, path_item.head.as_ref()),
        (HttpMethod::Trace, path_item.trace.as_ref()),
    ]
    .into_iter()
    .filter_map(|(method, op)| op.map(|operation| (method, operation)))
}

fn find_operation_by_method<'a>(
    path_item: &'a PathItem,
    method: HttpMethod,
) -> Option<&'a openapiv3::Operation> {
    match method {
        HttpMethod::Get => path_item.get.as_ref(),
        HttpMethod::Put => path_item.put.as_ref(),
        HttpMethod::Post => path_item.post.as_ref(),
        HttpMethod::Delete => path_item.delete.as_ref(),
        HttpMethod::Patch => path_item.patch.as_ref(),
        HttpMethod::Options => path_item.options.as_ref(),
        HttpMethod::Head => path_item.head.as_ref(),
        HttpMethod::Trace => path_item.trace.as_ref(),
    }
}

enum ComponentRef<'a> {
    Parameter(&'a str),
    Response(&'a str),
    Header(&'a str),
    Schema(&'a str),
    Callback(&'a str),
    RequestBody(&'a str),
    Example(&'a str),
    Link(&'a str),
    SecurityScheme(&'a str),
}

impl<'a> ComponentRef<'a> {
    fn parse(reference: &'a str) -> Option<Self> {
        let mut pieces = reference
            .trim_start_matches('#')
            .split('/')
            .filter(|s| !s.is_empty());

        match (pieces.next(), pieces.next(), pieces.next(), pieces.next()) {
            (Some("components"), Some(kind), Some(name), None) => match kind {
                "parameters" => Some(Self::Parameter(name)),
                "responses" => Some(Self::Response(name)),
                "schemas" => Some(Self::Schema(name)),
                "headers" => Some(Self::Header(name)),
                "callbacks" => Some(Self::Callback(name)),
                "requestBodies" => Some(Self::RequestBody(name)),
                "examples" => Some(Self::Example(name)),
                "links" => Some(Self::Link(name)),
                "securitySchemes" => Some(Self::SecurityScheme(name)),
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod test {
    use openapiv3::{Operation as OAPIOperation, PathItem};

    #[test]
    fn test_resolve_operation() {
        let op = OAPIOperation {
            ..Default::default()
        };
        let _param = PathItem {
            get: Some(op),
            ..Default::default()
        };
    }
}
