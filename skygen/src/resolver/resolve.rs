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

use anyhow::{anyhow, Context, Result};
use indexmap::IndexMap;
use openapiv3::{
    Callback, Components, Example, Header, Link, OpenAPI, Parameter, ReferenceOr, RequestBody,
    Response, Schema, SecurityScheme,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct Resolver {
    doc: OpenAPI,

    schema_cache: HashMap<String, Schema>,
    response_cache: HashMap<String, Response>,
    parameter_cache: HashMap<String, Parameter>,
    example_cache: HashMap<String, Example>,
    request_body_cache: HashMap<String, RequestBody>,
    header_cache: HashMap<String, Header>,
    security_scheme_cache: HashMap<String, SecurityScheme>,
    link_cache: HashMap<String, Link>,
    callback_cache: HashMap<String, Callback>,

    in_progress: HashSet<String>,
}

impl Resolver {
    /// Create a new resolver backed by the parsed OpenAPI document and
    /// empty caches for each component type.
    pub fn new(inner: OpenAPI) -> Self {
        Self {
            doc: inner,
            schema_cache: HashMap::new(),
            response_cache: HashMap::new(),
            parameter_cache: HashMap::new(),
            example_cache: HashMap::new(),
            request_body_cache: HashMap::new(),
            header_cache: HashMap::new(),
            security_scheme_cache: HashMap::new(),
            link_cache: HashMap::new(),
            callback_cache: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    /// Resolve all supported references and return the fully inlined
    /// OpenAPI document.
    pub fn resolve(mut self) -> Result<OpenAPI> {
        if let Some(mut components) = self.doc.components.take() {
            self.resolve_components(&mut components)?;
            self.doc.components = Some(components);
        }
        Ok(self.doc)
    }

    /// Walk the components object and resolve each supported entry.
    fn resolve_components(&mut self, components: &mut Components) -> Result<()> {
        self.resolve_schema_map(&mut components.schemas)?;
        self.resolve_response_map(&mut components.responses)?;
        self.resolve_parameter_map(&mut components.parameters)?;
        self.resolve_example_map(&mut components.examples)?;
        self.resolve_request_body_map(&mut components.request_bodies)?;
        self.resolve_header_map(&mut components.headers)?;
        self.resolve_security_scheme_map(&mut components.security_schemes)?;
        self.resolve_link_map(&mut components.links)?;
        self.resolve_callback_map(&mut components.callbacks)?;
        Ok(())
    }

    /// Iterate over every schema entry and resolve the referenced
    /// structure (tracking progress to avoid borrowing issues).
    fn resolve_schema_map(
        &mut self,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<()> {
        let schema_names: Vec<String> = schemas.keys().cloned().collect();
        for name in schema_names {
            self.resolve_schema_entry(&name, schemas)
                .with_context(|| format!("failed to resolve schema {name}"))?;
        }
        Ok(())
    }

    /// Resolve a single schema (either inline or via `$ref`) and cache
    /// the result under the schema name.
    fn resolve_schema_entry(
        &mut self,
        name: &str,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<Schema> {
        if let Some(cached) = self.schema_cache.get(name) {
            return Ok(cached.clone());
        }

        let entry = schemas
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("schema {name} missing from components"))?;

        let resolved = match entry {
            ReferenceOr::Item(mut schema) => {
                self.walk_schema(&mut schema)?;
                schema
            }
            ReferenceOr::Reference { reference } => {
                self.resolve_schema_reference(&reference, schemas)?
            }
        };

        self.schema_cache.insert(name.to_string(), resolved.clone());
        if let Some(slot) = schemas.get_mut(name) {
            *slot = ReferenceOr::Item(resolved.clone());
        }

        Ok(resolved)
    }

    /// Follow a `#/components/schemas/...` reference, detect cycles, and
    /// return the resolved schema.
    fn resolve_schema_reference(
        &mut self,
        reference: &str,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<Schema> {
        if let Some(cached) = self.schema_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target_name) = reference.strip_prefix("#/components/schemas/") else {
            return Err(anyhow!("unsupported schema reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let schema = self.resolve_schema_entry(target_name, schemas)?;

        self.in_progress.remove(reference);
        self.schema_cache
            .insert(reference.to_string(), schema.clone());

        Ok(schema)
    }

    /// Iterate over reusable responses and resolve each entry.
    fn resolve_response_map(
        &mut self,
        responses: &mut IndexMap<String, ReferenceOr<Response>>,
    ) -> Result<()> {
        let response_names: Vec<String> = responses.keys().cloned().collect();
        for name in response_names {
            let entry = responses
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("response {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(resp) => resp,
                ReferenceOr::Reference { reference } => {
                    self.resolve_response_reference(&reference, responses)?
                }
            };

            self.response_cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = responses.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/responses/...` reference with cycle checks.
    fn resolve_response_reference(
        &mut self,
        reference: &str,
        responses: &mut IndexMap<String, ReferenceOr<Response>>,
    ) -> Result<Response> {
        if let Some(cached) = self.response_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/responses/") else {
            return Err(anyhow!("unsupported response reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let resolved = responses
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("response {target} missing from components"))?;

        let response = match resolved {
            ReferenceOr::Item(resp) => resp,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_response_reference(&nested, responses)?
            }
        };

        self.in_progress.remove(reference);
        self.response_cache
            .insert(reference.to_string(), response.clone());

        Ok(response)
    }

    /// Iterate over reusable parameters and resolve each entry.
    fn resolve_parameter_map(
        &mut self,
        parameters: &mut IndexMap<String, ReferenceOr<Parameter>>,
    ) -> Result<()> {
        let names: Vec<String> = parameters.keys().cloned().collect();
        for name in names {
            let entry = parameters
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("parameter {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(param) => param,
                ReferenceOr::Reference { reference } => {
                    self.resolve_parameter_reference(&reference, parameters)?
                }
            };

            self.parameter_cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = parameters.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/parameters/...` reference with cycle checks.
    fn resolve_parameter_reference(
        &mut self,
        reference: &str,
        parameters: &mut IndexMap<String, ReferenceOr<Parameter>>,
    ) -> Result<Parameter> {
        if let Some(cached) = self.parameter_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/parameters/") else {
            return Err(anyhow!("unsupported parameter reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = parameters
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("parameter {target} missing from components"))?;

        let parameter = match entry {
            ReferenceOr::Item(param) => param,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_parameter_reference(&nested, parameters)?
            }
        };

        self.in_progress.remove(reference);
        self.parameter_cache
            .insert(reference.to_string(), parameter.clone());

        Ok(parameter)
    }

    /// Iterate over reusable examples and resolve each entry.
    fn resolve_example_map(
        &mut self,
        examples: &mut IndexMap<String, ReferenceOr<Example>>,
    ) -> Result<()> {
        let names: Vec<String> = examples.keys().cloned().collect();
        for name in names {
            let entry = examples
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("example {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(example) => example,
                ReferenceOr::Reference { reference } => {
                    self.resolve_example_reference(&reference, examples)?
                }
            };

            self.example_cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = examples.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/examples/...` reference with cycle checks.
    fn resolve_example_reference(
        &mut self,
        reference: &str,
        examples: &mut IndexMap<String, ReferenceOr<Example>>,
    ) -> Result<Example> {
        if let Some(cached) = self.example_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/examples/") else {
            return Err(anyhow!("unsupported example reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = examples
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("example {target} missing from components"))?;

        let example = match entry {
            ReferenceOr::Item(ex) => ex,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_example_reference(&nested, examples)?
            }
        };

        self.in_progress.remove(reference);
        self.example_cache
            .insert(reference.to_string(), example.clone());

        Ok(example)
    }

    /// Iterate over reusable request bodies and resolve each entry.
    fn resolve_request_body_map(
        &mut self,
        bodies: &mut IndexMap<String, ReferenceOr<RequestBody>>,
    ) -> Result<()> {
        let names: Vec<String> = bodies.keys().cloned().collect();
        for name in names {
            let entry = bodies
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("request body {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(body) => body,
                ReferenceOr::Reference { reference } => {
                    self.resolve_request_body_reference(&reference, bodies)?
                }
            };

            self.request_body_cache
                .insert(name.clone(), resolved.clone());
            if let Some(slot) = bodies.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/requestBodies/...` reference with cycle checks.
    fn resolve_request_body_reference(
        &mut self,
        reference: &str,
        bodies: &mut IndexMap<String, ReferenceOr<RequestBody>>,
    ) -> Result<RequestBody> {
        if let Some(cached) = self.request_body_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/requestBodies/") else {
            return Err(anyhow!("unsupported request body reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = bodies
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("request body {target} missing from components"))?;

        let body = match entry {
            ReferenceOr::Item(body) => body,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_request_body_reference(&nested, bodies)?
            }
        };

        self.in_progress.remove(reference);
        self.request_body_cache
            .insert(reference.to_string(), body.clone());

        Ok(body)
    }

    /// Iterate over reusable headers and resolve each entry.
    fn resolve_header_map(
        &mut self,
        headers: &mut IndexMap<String, ReferenceOr<Header>>,
    ) -> Result<()> {
        let names: Vec<String> = headers.keys().cloned().collect();
        for name in names {
            let entry = headers
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("header {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(header) => header,
                ReferenceOr::Reference { reference } => {
                    self.resolve_header_reference(&reference, headers)?
                }
            };

            self.header_cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = headers.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/headers/...` reference with cycle checks.
    fn resolve_header_reference(
        &mut self,
        reference: &str,
        headers: &mut IndexMap<String, ReferenceOr<Header>>,
    ) -> Result<Header> {
        if let Some(cached) = self.header_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/headers/") else {
            return Err(anyhow!("unsupported header reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = headers
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("header {target} missing from components"))?;

        let header = match entry {
            ReferenceOr::Item(header) => header,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_header_reference(&nested, headers)?
            }
        };

        self.in_progress.remove(reference);
        self.header_cache
            .insert(reference.to_string(), header.clone());

        Ok(header)
    }

    /// Iterate over reusable security schemes and resolve each entry.
    fn resolve_security_scheme_map(
        &mut self,
        schemes: &mut IndexMap<String, ReferenceOr<SecurityScheme>>,
    ) -> Result<()> {
        let names: Vec<String> = schemes.keys().cloned().collect();
        for name in names {
            let entry = schemes
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("security scheme {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(scheme) => scheme,
                ReferenceOr::Reference { reference } => {
                    self.resolve_security_scheme_reference(&reference, schemes)?
                }
            };

            self.security_scheme_cache
                .insert(name.clone(), resolved.clone());
            if let Some(slot) = schemes.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/securitySchemes/...` reference with cycle checks.
    fn resolve_security_scheme_reference(
        &mut self,
        reference: &str,
        schemes: &mut IndexMap<String, ReferenceOr<SecurityScheme>>,
    ) -> Result<SecurityScheme> {
        if let Some(cached) = self.security_scheme_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/securitySchemes/") else {
            return Err(anyhow!("unsupported security scheme reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = schemes
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("security scheme {target} missing from components"))?;

        let scheme = match entry {
            ReferenceOr::Item(scheme) => scheme,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_security_scheme_reference(&nested, schemes)?
            }
        };

        self.in_progress.remove(reference);
        self.security_scheme_cache
            .insert(reference.to_string(), scheme.clone());

        Ok(scheme)
    }

    /// Iterate over reusable links and resolve each entry.
    fn resolve_link_map(&mut self, links: &mut IndexMap<String, ReferenceOr<Link>>) -> Result<()> {
        let names: Vec<String> = links.keys().cloned().collect();
        for name in names {
            let entry = links
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("link {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(link) => link,
                ReferenceOr::Reference { reference } => {
                    self.resolve_link_reference(&reference, links)?
                }
            };

            self.link_cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = links.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/links/...` reference with cycle checks.
    fn resolve_link_reference(
        &mut self,
        reference: &str,
        links: &mut IndexMap<String, ReferenceOr<Link>>,
    ) -> Result<Link> {
        if let Some(cached) = self.link_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/links/") else {
            return Err(anyhow!("unsupported link reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = links
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("link {target} missing from components"))?;

        let link = match entry {
            ReferenceOr::Item(link) => link,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_link_reference(&nested, links)?
            }
        };

        self.in_progress.remove(reference);
        self.link_cache.insert(reference.to_string(), link.clone());

        Ok(link)
    }

    /// Iterate over reusable callbacks and resolve each entry.
    fn resolve_callback_map(
        &mut self,
        callbacks: &mut IndexMap<String, ReferenceOr<Callback>>,
    ) -> Result<()> {
        let names: Vec<String> = callbacks.keys().cloned().collect();
        for name in names {
            let entry = callbacks
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("callback {name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(callback) => callback,
                ReferenceOr::Reference { reference } => {
                    self.resolve_callback_reference(&reference, callbacks)?
                }
            };

            self.callback_cache
                .insert(name.clone(), resolved.clone());
            if let Some(slot) = callbacks.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Follow a `#/components/callbacks/...` reference with cycle checks.
    fn resolve_callback_reference(
        &mut self,
        reference: &str,
        callbacks: &mut IndexMap<String, ReferenceOr<Callback>>,
    ) -> Result<Callback> {
        if let Some(cached) = self.callback_cache.get(reference) {
            return Ok(cached.clone());
        }

        let Some(target) = reference.strip_prefix("#/components/callbacks/") else {
            return Err(anyhow!("unsupported callback reference {reference}"));
        };

        if !self.in_progress.insert(reference.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let entry = callbacks
            .get(target)
            .cloned()
            .ok_or_else(|| anyhow!("callback {target} missing from components"))?;

        let callback = match entry {
            ReferenceOr::Item(callback) => callback,
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_callback_reference(&nested, callbacks)?
            }
        };

        self.in_progress.remove(reference);
        self.callback_cache
            .insert(reference.to_string(), callback.clone());

        Ok(callback)
    }

    /// Placeholder for recursively resolving nested references inside a
    /// schema (properties, items, etc.).
    fn walk_schema(&mut self, _schema: &mut Schema) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use openapiv3::{
        Callback, Example, Header, HeaderStyle, HttpSecurityScheme, Link, MediaType, Parameter,
        ParameterData, ParameterSchemaOrContent, PathItem, QueryStyle, RequestBody, SchemaKind,
        SecurityScheme, Type,
    };

    fn simple_schema(kind: Type) -> Schema {
        Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(kind),
        }
    }

    fn query_parameter(name: &str) -> Parameter {
        Parameter::Query {
            parameter_data: ParameterData {
                name: name.into(),
                description: None,
                required: true,
                deprecated: None,
                format: ParameterSchemaOrContent::Schema(ReferenceOr::Item(simple_schema(
                    Type::Integer(Default::default()),
                ))),
                example: None,
                examples: IndexMap::new(),
                explode: None,
                extensions: IndexMap::new(),
            },
            allow_reserved: false,
            style: QueryStyle::default(),
            allow_empty_value: None,
        }
    }

    fn base_openapi() -> OpenAPI {
        OpenAPI {
            components: Some(Components::default()),
            ..OpenAPI::default()
        }
    }

    #[test]
    fn resolves_schema_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "Base".into(),
            ReferenceOr::Item(simple_schema(Type::String(Default::default()))),
        );
        components.schemas.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/schemas/Base".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.schemas.get("Alias").unwrap() {
            ReferenceOr::Item(schema) => match &schema.schema_kind {
                SchemaKind::Type(Type::String(_)) => {}
                other => panic!("unexpected schema kind {other:?}"),
            },
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved schema, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_response_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.responses.insert(
            "Ok".into(),
            ReferenceOr::Item(Response {
                description: "ok".into(),
                ..Response::default()
            }),
        );
        components.responses.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/responses/Ok".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.responses.get("Alias").unwrap() {
            ReferenceOr::Item(response) => {
                assert_eq!(response.description, "ok");
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved response, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_parameter_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.parameters.insert(
            "AccountId".into(),
            ReferenceOr::Item(query_parameter("account_id")),
        );
        components.parameters.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/parameters/AccountId".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.parameters.get("Alias").unwrap() {
            ReferenceOr::Item(Parameter::Query { parameter_data, .. }) => {
                assert_eq!(parameter_data.name, "account_id");
            }
            ReferenceOr::Item(other) => panic!("unexpected parameter variant {other:?}"),
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved parameter, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_example_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.examples.insert(
            "Example".into(),
            ReferenceOr::Item(Example {
                summary: Some("example".into()),
                ..Example::default()
            }),
        );
        components.examples.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/examples/Example".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.examples.get("Alias").unwrap() {
            ReferenceOr::Item(example) => {
                assert_eq!(example.summary.as_deref(), Some("example"));
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved example, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_request_body_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        let mut body = RequestBody::default();
        let mut media = MediaType::default();
        media.schema = Some(ReferenceOr::Item(simple_schema(Type::String(
            Default::default(),
        ))));
        body.content.insert("application/json".into(), media);
        components
            .request_bodies
            .insert("Create".into(), ReferenceOr::Item(body));
        components.request_bodies.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/requestBodies/Create".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.request_bodies.get("Alias").unwrap() {
            ReferenceOr::Item(body) => {
                assert!(body.content.contains_key("application/json"));
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved request body, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_header_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.headers.insert(
            "Auth".into(),
            ReferenceOr::Item(Header {
                description: Some("auth header".into()),
                style: HeaderStyle::default(),
                required: true,
                deprecated: None,
                format: ParameterSchemaOrContent::Schema(ReferenceOr::Item(simple_schema(
                    Type::String(Default::default()),
                ))),
                example: None,
                examples: IndexMap::new(),
                extensions: IndexMap::new(),
            }),
        );
        components.headers.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/headers/Auth".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.headers.get("Alias").unwrap() {
            ReferenceOr::Item(header) => {
                assert!(header.required);
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved header, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_security_scheme_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components
            .security_schemes
            .insert("Bearer".into(), ReferenceOr::Item(SecurityScheme::HTTP(HttpSecurityScheme {
                scheme: "bearer".into(),
                bearer_format: Some("JWT".into()),
                description: None,
            })));
        components.security_schemes.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/securitySchemes/Bearer".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.security_schemes.get("Alias").unwrap() {
            ReferenceOr::Item(SecurityScheme::HTTP(http)) => {
                assert_eq!(http.scheme, "bearer");
            }
            ReferenceOr::Item(other) => panic!("unexpected scheme variant {other:?}"),
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved security scheme, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_link_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.links.insert(
            "Log".into(),
            ReferenceOr::Item(Link {
                operation_id: Some("get_log".into()),
                description: Some("link to logs".into()),
                ..Link::default()
            }),
        );
        components.links.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/links/Log".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.links.get("Alias").unwrap() {
            ReferenceOr::Item(link) => {
                assert_eq!(link.operation_id.as_deref(), Some("get_log"));
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved link, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_callback_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        let mut callback = Callback::default();
        callback.add_path_item(
            "{$request.body#/callback}".into(),
            ReferenceOr::Item(PathItem::default()),
        );
        components
            .callbacks
            .insert("Webhook".into(), ReferenceOr::Item(callback));
        components.callbacks.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/callbacks/Webhook".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.callbacks.get("Alias").unwrap() {
            ReferenceOr::Item(callback) => {
                assert!(callback
                    .iter()
                    .any(|(expr, _)| expr.contains("callback")));
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved callback, found reference {reference}")
            }
        }
    }
}
