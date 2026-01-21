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

use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use openapiv3::{
    AdditionalProperties, Callback, Components, Example, Header, Link, MediaType, OpenAPI,
    Operation, Parameter, ParameterSchemaOrContent, PathItem, Paths, ReferenceOr, RequestBody,
    Response, Responses, Schema, SchemaKind, SecurityRequirement, SecurityScheme, Type,
};
use std::collections::{HashMap, HashSet};
use std::mem;

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
            let mut paths = mem::take(&mut self.doc.paths);
            self.resolve_paths(&mut paths, &mut components)?;
            self.doc.paths = paths;
            if let Some(mut security) = self.doc.security.take() {
                self.resolve_security_requirements(&mut security, &mut components)?;
                self.doc.security = Some(security);
            }
            self.doc.components = Some(components);
        }
        Ok(self.doc)
    }

    /// Walk the components object and resolve each supported entry.
    fn resolve_components(&mut self, components: &mut Components) -> Result<()> {
        let mut schemas = mem::take(&mut components.schemas);
        self.resolve_schema_map(&mut schemas)?;
        components.schemas = schemas;

        let mut responses = mem::take(&mut components.responses);
        let mut response_cache = mem::take(&mut self.response_cache);
        self.resolve_response_map(&mut responses, components, &mut response_cache)?;
        self.response_cache = response_cache;
        components.responses = responses;

        let mut parameters = mem::take(&mut components.parameters);
        let mut parameter_cache = mem::take(&mut self.parameter_cache);
        self.resolve_parameter_map(&mut parameters, components, &mut parameter_cache)?;
        self.parameter_cache = parameter_cache;
        components.parameters = parameters;

        let mut examples = mem::take(&mut components.examples);
        let mut example_cache = mem::take(&mut self.example_cache);
        self.resolve_example_map(&mut examples, &mut example_cache)?;
        self.example_cache = example_cache;
        components.examples = examples;

        let mut request_bodies = mem::take(&mut components.request_bodies);
        let mut request_body_cache = mem::take(&mut self.request_body_cache);
        self.resolve_request_body_map(&mut request_bodies, components, &mut request_body_cache)?;
        self.request_body_cache = request_body_cache;
        components.request_bodies = request_bodies;

        let mut headers = mem::take(&mut components.headers);
        let mut header_cache = mem::take(&mut self.header_cache);
        self.resolve_header_map(&mut headers, components, &mut header_cache)?;
        self.header_cache = header_cache;
        components.headers = headers;

        let mut security_schemes = mem::take(&mut components.security_schemes);
        let mut security_scheme_cache = mem::take(&mut self.security_scheme_cache);
        self.resolve_security_scheme_map(&mut security_schemes, &mut security_scheme_cache)?;
        self.security_scheme_cache = security_scheme_cache;
        components.security_schemes = security_schemes;

        let mut links = mem::take(&mut components.links);
        let mut link_cache = mem::take(&mut self.link_cache);
        self.resolve_link_map(&mut links, &mut link_cache)?;
        self.link_cache = link_cache;
        components.links = links;

        let mut callbacks = mem::take(&mut components.callbacks);
        let mut callback_cache = mem::take(&mut self.callback_cache);
        self.resolve_callback_map(&mut callbacks, components, &mut callback_cache)?;
        self.callback_cache = callback_cache;
        components.callbacks = callbacks;

        Ok(())
    }

    /// Iterate over every schema entry and resolve the referenced
    /// structure (tracking progress to avoid borrowing issues).
    fn resolve_schema_map(&mut self, schemas: &mut IndexMap<String, ReferenceOr<Schema>>) -> Result<()> {
        let names: Vec<String> = schemas.keys().cloned().collect();
        for name in names {
            let entry = schemas
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("#/components/schemas/{name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    let inserted = self.in_progress.insert(name.clone());
                    let result = self.walk_schema(&mut item, schemas);
                    if inserted {
                        self.in_progress.remove(&name);
                    }
                    result?;
                    item
                }
                ReferenceOr::Reference { reference } => {
                    self.resolve_schema_reference(&reference, schemas)?
                }
            };

            self.schema_cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = schemas.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    /// Iterate over reusable responses and resolve each entry.
    fn resolve_response_map(
        &mut self,
        responses: &mut IndexMap<String, ReferenceOr<Response>>,
        components: &mut Components,
        cache: &mut HashMap<String, Response>,
    ) -> Result<()> {
        self.resolve_component_map(
            responses,
            cache,
            "#/components/responses/",
            |resolver, response| resolver.walk_response(response, components),
        )
    }

    fn resolve_parameter_map(
        &mut self,
        parameters: &mut IndexMap<String, ReferenceOr<Parameter>>,
        components: &mut Components,
        cache: &mut HashMap<String, Parameter>,
    ) -> Result<()> {
        self.resolve_component_map(
            parameters,
            cache,
            "#/components/parameters/",
            |resolver, parameter| resolver.walk_parameter(parameter, components),
        )
    }

    fn resolve_example_map(
        &mut self,
        examples: &mut IndexMap<String, ReferenceOr<Example>>,
        cache: &mut HashMap<String, Example>,
    ) -> Result<()> {
        self.resolve_component_map(
            examples,
            cache,
            "#/components/examples/",
            |_, example| Self::noop_walk(example),
        )
    }

    fn resolve_request_body_map(
        &mut self,
        bodies: &mut IndexMap<String, ReferenceOr<RequestBody>>,
        components: &mut Components,
        cache: &mut HashMap<String, RequestBody>,
    ) -> Result<()> {
        self.resolve_component_map(
            bodies,
            cache,
            "#/components/requestBodies/",
            |resolver, body| resolver.walk_request_body(body, components),
        )
    }

    fn resolve_header_map(
        &mut self,
        headers: &mut IndexMap<String, ReferenceOr<Header>>,
        components: &mut Components,
        cache: &mut HashMap<String, Header>,
    ) -> Result<()> {
        self.resolve_component_map(
            headers,
            cache,
            "#/components/headers/",
            |resolver, header| resolver.walk_header(header, components),
        )
    }

    fn resolve_security_scheme_map(
        &mut self,
        schemes: &mut IndexMap<String, ReferenceOr<SecurityScheme>>,
        cache: &mut HashMap<String, SecurityScheme>,
    ) -> Result<()> {
        self.resolve_component_map(
            schemes,
            cache,
            "#/components/securitySchemes/",
            |_, scheme| Self::noop_walk(scheme),
        )
    }

    fn resolve_link_map(
        &mut self,
        links: &mut IndexMap<String, ReferenceOr<Link>>,
        cache: &mut HashMap<String, Link>,
    ) -> Result<()> {
        self.resolve_component_map(
            links,
            cache,
            "#/components/links/",
            |_, link| Self::noop_walk(link),
        )
    }

    fn resolve_callback_map(
        &mut self,
        callbacks: &mut IndexMap<String, ReferenceOr<Callback>>,
        components: &mut Components,
        cache: &mut HashMap<String, Callback>,
    ) -> Result<()> {
        self.resolve_component_map(
            callbacks,
            cache,
            "#/components/callbacks/",
            |resolver, callback| resolver.walk_callback(callback, components),
        )
    }

    fn resolve_component_map<T, F>(
        &mut self,
        map: &mut IndexMap<String, ReferenceOr<T>>,
        cache: &mut HashMap<String, T>,
        prefix: &str,
        mut walk_inline: F,
    ) -> Result<()>
    where
        T: Clone,
        F: FnMut(&mut Resolver, &mut T) -> Result<()>,
    {
        let names: Vec<String> = map.keys().cloned().collect();
        for name in names {
            let entry = map
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("{prefix}{name} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    walk_inline(self, &mut item)?;
                    item
                }
                ReferenceOr::Reference { reference } => self.resolve_component_reference(
                    map,
                    cache,
                    prefix,
                    &reference,
                    &mut walk_inline,
                )?,
            };

            cache.insert(name.clone(), resolved.clone());
            if let Some(slot) = map.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    fn resolve_component_reference<T>(
        &mut self,
        map: &mut IndexMap<String, ReferenceOr<T>>,
        cache: &mut HashMap<String, T>,
        prefix: &str,
        reference: &str,
        walk_inline: &mut dyn FnMut(&mut Resolver, &mut T) -> Result<()>,
    ) -> Result<T>
    where
        T: Clone,
    {
        let Some(target) = reference.strip_prefix(prefix) else {
            return Err(anyhow!("unsupported reference {reference}"));
        };

        if let Some(cached) = cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = map
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    walk_inline(self, &mut item)?;
                    item
                }
                ReferenceOr::Reference { reference: nested } => self.resolve_component_reference(
                    map,
                    cache,
                    prefix,
                    &nested,
                    walk_inline,
                )?,
            };

            cache.insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_paths(&mut self, paths: &mut Paths, components: &mut Components) -> Result<()> {
        let names: Vec<String> = paths.paths.keys().cloned().collect();
        for name in names {
            let entry = paths
                .paths
                .get(&name)
                .cloned()
                .ok_or_else(|| anyhow!("#/paths/{name} missing from paths"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    self.resolve_path_item(&mut item, components)?;
                    item
                }
                ReferenceOr::Reference { reference } => {
                    self.resolve_path_item_reference(&reference, paths, components)?
                }
            };

            if let Some(slot) = paths.paths.get_mut(&name) {
                *slot = ReferenceOr::Item(resolved);
            }
        }
        Ok(())
    }

    fn resolve_path_item_reference(
        &mut self,
        reference: &str,
        paths: &mut Paths,
        components: &mut Components,
    ) -> Result<PathItem> {
        let Some(pointer) = reference.strip_prefix("#/paths/") else {
            return Err(anyhow!("unsupported path item reference {reference}"));
        };

        if pointer.contains('/') {
            return Err(anyhow!("unsupported path item reference {reference}"));
        }

        let path_key = Self::decode_pointer_segment(pointer);
        let entry = paths
            .paths
            .get(&path_key)
            .cloned()
            .ok_or_else(|| anyhow!("{reference} missing from paths"))?;

        let resolved = match entry {
            ReferenceOr::Item(mut item) => {
                self.resolve_path_item(&mut item, components)?;
                item
            }
            ReferenceOr::Reference { reference: nested } => {
                self.resolve_path_item_reference(&nested, paths, components)?
            }
        };

        Ok(resolved)
    }

    fn resolve_path_item(&mut self, item: &mut PathItem, components: &mut Components) -> Result<()> {
        self.resolve_parameters(&mut item.parameters, components)?;
        if let Some(operation) = item.get.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.put.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.post.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.delete.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.options.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.head.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.patch.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        if let Some(operation) = item.trace.as_mut() {
            self.resolve_operation(operation, components)?;
        }
        Ok(())
    }

    fn resolve_operation(
        &mut self,
        operation: &mut Operation,
        components: &mut Components,
    ) -> Result<()> {
        self.resolve_parameters(&mut operation.parameters, components)?;
        if let Some(body) = operation.request_body.as_mut() {
            self.resolve_request_body_ref(body, components)?;
        }
        self.resolve_responses(&mut operation.responses, components)?;
        if let Some(security) = operation.security.as_mut() {
            self.resolve_security_requirements(security, components)?;
        }
        for callback in operation.callbacks.values_mut() {
            self.walk_callback(callback, components)?;
        }
        Ok(())
    }

    fn resolve_responses(
        &mut self,
        responses: &mut Responses,
        components: &mut Components,
    ) -> Result<()> {
        if let Some(default) = responses.default.as_mut() {
            self.resolve_response_ref(default, components)?;
        }
        for response in responses.responses.values_mut() {
            self.resolve_response_ref(response, components)?;
        }
        Ok(())
    }

    fn resolve_parameters(
        &mut self,
        parameters: &mut Vec<ReferenceOr<Parameter>>,
        components: &mut Components,
    ) -> Result<()> {
        for parameter in parameters.iter_mut() {
            self.resolve_parameter_ref(parameter, components)?;
        }
        Ok(())
    }

    fn resolve_parameter_ref(
        &mut self,
        parameter: &mut ReferenceOr<Parameter>,
        components: &mut Components,
    ) -> Result<()> {
        let resolved = match parameter.clone() {
            ReferenceOr::Item(mut item) => {
                self.walk_parameter(&mut item, components)?;
                item
            }
            ReferenceOr::Reference { reference } => {
                self.resolve_parameter_reference(&reference, components)?
            }
        };
        *parameter = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_parameter_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<Parameter> {
        let Some(target) = reference.strip_prefix("#/components/parameters/") else {
            return Err(anyhow!("unsupported parameter reference {reference}"));
        };

        if let Some(cached) = self.parameter_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .parameters
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    self.walk_parameter(&mut item, components)?;
                    item
                }
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_parameter_reference(&nested, components)?
                }
            };

            self.parameter_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_request_body_ref(
        &mut self,
        body: &mut ReferenceOr<RequestBody>,
        components: &mut Components,
    ) -> Result<()> {
        let resolved = match body.clone() {
            ReferenceOr::Item(mut item) => {
                self.walk_request_body(&mut item, components)?;
                item
            }
            ReferenceOr::Reference { reference } => {
                self.resolve_request_body_reference(&reference, components)?
            }
        };
        *body = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_request_body_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<RequestBody> {
        let Some(target) = reference.strip_prefix("#/components/requestBodies/") else {
            return Err(anyhow!("unsupported request body reference {reference}"));
        };

        if let Some(cached) = self.request_body_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .request_bodies
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    self.walk_request_body(&mut item, components)?;
                    item
                }
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_request_body_reference(&nested, components)?
                }
            };

            self.request_body_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_response_ref(
        &mut self,
        response: &mut ReferenceOr<Response>,
        components: &mut Components,
    ) -> Result<()> {
        let resolved = match response.clone() {
            ReferenceOr::Item(mut item) => {
                self.walk_response(&mut item, components)?;
                item
            }
            ReferenceOr::Reference { reference } => {
                self.resolve_response_reference(&reference, components)?
            }
        };
        *response = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_response_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<Response> {
        let Some(target) = reference.strip_prefix("#/components/responses/") else {
            return Err(anyhow!("unsupported response reference {reference}"));
        };

        if let Some(cached) = self.response_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .responses
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    self.walk_response(&mut item, components)?;
                    item
                }
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_response_reference(&nested, components)?
                }
            };

            self.response_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_header_ref(
        &mut self,
        header: &mut ReferenceOr<Header>,
        components: &mut Components,
    ) -> Result<()> {
        let resolved = match header.clone() {
            ReferenceOr::Item(mut item) => {
                self.walk_header(&mut item, components)?;
                item
            }
            ReferenceOr::Reference { reference } => {
                self.resolve_header_reference(&reference, components)?
            }
        };
        *header = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_header_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<Header> {
        let Some(target) = reference.strip_prefix("#/components/headers/") else {
            return Err(anyhow!("unsupported header reference {reference}"));
        };

        if let Some(cached) = self.header_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .headers
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut item) => {
                    self.walk_header(&mut item, components)?;
                    item
                }
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_header_reference(&nested, components)?
                }
            };

            self.header_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_link_ref(
        &mut self,
        link: &mut ReferenceOr<Link>,
        components: &mut Components,
    ) -> Result<()> {
        let resolved = match link.clone() {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { reference } => {
                self.resolve_link_reference(&reference, components)?
            }
        };
        *link = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_link_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<Link> {
        let Some(target) = reference.strip_prefix("#/components/links/") else {
            return Err(anyhow!("unsupported link reference {reference}"));
        };

        if let Some(cached) = self.link_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .links
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(item) => item,
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_link_reference(&nested, components)?
                }
            };

            self.link_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_example_ref(
        &mut self,
        example: &mut ReferenceOr<Example>,
        components: &mut Components,
    ) -> Result<()> {
        let resolved = match example.clone() {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { reference } => {
                self.resolve_example_reference(&reference, components)?
            }
        };
        *example = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_example_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<Example> {
        let Some(target) = reference.strip_prefix("#/components/examples/") else {
            return Err(anyhow!("unsupported example reference {reference}"));
        };

        if let Some(cached) = self.example_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .examples
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(item) => item,
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_example_reference(&nested, components)?
                }
            };

            self.example_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_schema_reference(
        &mut self,
        reference: &str,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<Schema> {
        let Some(target) = reference.strip_prefix("#/components/schemas/") else {
            return Err(anyhow!("unsupported schema reference {reference}"));
        };

        if let Some(cached) = self.schema_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = schemas
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(mut schema) => {
                    self.walk_schema(&mut schema, schemas)?;
                    schema
                }
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_schema_reference(&nested, schemas)?
                }
            };

            self.schema_cache.insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_security_requirements(
        &mut self,
        requirements: &mut Vec<SecurityRequirement>,
        components: &mut Components,
    ) -> Result<()> {
        for requirement in requirements.iter() {
            for scheme_name in requirement.keys() {
                let Some(entry) = components.security_schemes.get(scheme_name).cloned() else {
                    // Leave unknown schemes untouched to avoid failing on vendor quirks.
                    continue;
                };

                let resolved = match entry {
                    ReferenceOr::Item(item) => item,
                    ReferenceOr::Reference { reference } => {
                        self.resolve_security_scheme_reference(&reference, components)?
                    }
                };

                if let Some(slot) = components.security_schemes.get_mut(scheme_name) {
                    *slot = ReferenceOr::Item(resolved);
                }
            }
        }
        Ok(())
    }

    fn resolve_security_scheme_reference(
        &mut self,
        reference: &str,
        components: &mut Components,
    ) -> Result<SecurityScheme> {
        let Some(target) = reference.strip_prefix("#/components/securitySchemes/") else {
            return Err(anyhow!("unsupported security scheme reference {reference}"));
        };

        if let Some(cached) = self.security_scheme_cache.get(target) {
            return Ok(cached.clone());
        }

        if !self.in_progress.insert(target.to_string()) {
            return Err(anyhow!("cycle detected while resolving {reference}"));
        }

        let result = (|| {
            let entry = components
                .security_schemes
                .get(target)
                .cloned()
                .ok_or_else(|| anyhow!("{reference} missing from components"))?;

            let resolved = match entry {
                ReferenceOr::Item(item) => item,
                ReferenceOr::Reference { reference: nested } => {
                    self.resolve_security_scheme_reference(&nested, components)?
                }
            };

            self.security_scheme_cache
                .insert(target.to_string(), resolved.clone());
            Ok(resolved)
        })();

        self.in_progress.remove(target);
        result
    }

    fn resolve_schema_ref(
        &mut self,
        schema_ref: &mut ReferenceOr<Schema>,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<()> {
        let resolved = match schema_ref.clone() {
            ReferenceOr::Item(mut schema) => {
                self.walk_schema(&mut schema, schemas)?;
                schema
            }
            ReferenceOr::Reference { reference } => {
                let Some(target) = reference.strip_prefix("#/components/schemas/") else {
                    return Err(anyhow!("unsupported schema reference {reference}"));
                };
                if self.in_progress.contains(target) {
                    // Preserve recursive references as-is.
                    return Ok(());
                }
                self.resolve_schema_reference(&reference, schemas)?
            }
        };
        *schema_ref = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn resolve_schema_ref_boxed(
        &mut self,
        schema_ref: &mut ReferenceOr<Box<Schema>>,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<()> {
        let resolved = match schema_ref.clone() {
            ReferenceOr::Item(mut schema) => {
                self.walk_schema(&mut schema, schemas)?;
                schema
            }
            ReferenceOr::Reference { reference } => {
                let Some(target) = reference.strip_prefix("#/components/schemas/") else {
                    return Err(anyhow!("unsupported schema reference {reference}"));
                };
                if self.in_progress.contains(target) {
                    // Preserve recursive references as-is.
                    return Ok(());
                }
                Box::new(self.resolve_schema_reference(&reference, schemas)?)
            }
        };
        *schema_ref = ReferenceOr::Item(resolved);
        Ok(())
    }

    fn walk_schema(
        &mut self,
        schema: &mut Schema,
        schemas: &mut IndexMap<String, ReferenceOr<Schema>>,
    ) -> Result<()> {
        match &mut schema.schema_kind {
            SchemaKind::Type(Type::Object(obj)) => {
                for prop in obj.properties.values_mut() {
                    self.resolve_schema_ref_boxed(prop, schemas)?;
                }
                if let Some(additional) = obj.additional_properties.as_mut() {
                    if let AdditionalProperties::Schema(schema_ref) = additional {
                        self.resolve_schema_ref(schema_ref.as_mut(), schemas)?;
                    }
                }
            }
            SchemaKind::Type(Type::Array(array)) => {
                if let Some(items) = array.items.as_mut() {
                    self.resolve_schema_ref_boxed(items, schemas)?;
                }
            }
            SchemaKind::OneOf { one_of } => {
                for item in one_of.iter_mut() {
                    self.resolve_schema_ref(item, schemas)?;
                }
            }
            SchemaKind::AllOf { all_of } => {
                for item in all_of.iter_mut() {
                    self.resolve_schema_ref(item, schemas)?;
                }
            }
            SchemaKind::AnyOf { any_of } => {
                for item in any_of.iter_mut() {
                    self.resolve_schema_ref(item, schemas)?;
                }
            }
            SchemaKind::Not { not } => {
                self.resolve_schema_ref(not.as_mut(), schemas)?;
            }
            SchemaKind::Any(any) => {
                for prop in any.properties.values_mut() {
                    self.resolve_schema_ref_boxed(prop, schemas)?;
                }
                if let Some(items) = any.items.as_mut() {
                    self.resolve_schema_ref_boxed(items, schemas)?;
                }
                if let Some(additional) = any.additional_properties.as_mut() {
                    if let AdditionalProperties::Schema(schema_ref) = additional {
                        self.resolve_schema_ref(schema_ref.as_mut(), schemas)?;
                    }
                }
                for item in any.one_of.iter_mut() {
                    self.resolve_schema_ref(item, schemas)?;
                }
                for item in any.all_of.iter_mut() {
                    self.resolve_schema_ref(item, schemas)?;
                }
                for item in any.any_of.iter_mut() {
                    self.resolve_schema_ref(item, schemas)?;
                }
                if let Some(not) = any.not.as_mut() {
                    self.resolve_schema_ref(not.as_mut(), schemas)?;
                }
            }
            SchemaKind::Type(Type::String(_))
            | SchemaKind::Type(Type::Number(_))
            | SchemaKind::Type(Type::Integer(_))
            | SchemaKind::Type(Type::Boolean(_)) => {}
        }
        Ok(())
    }

    fn walk_parameter(&mut self, parameter: &mut Parameter, components: &mut Components) -> Result<()> {
        let data = match parameter {
            Parameter::Query { parameter_data, .. } => parameter_data,
            Parameter::Header { parameter_data, .. } => parameter_data,
            Parameter::Path { parameter_data, .. } => parameter_data,
            Parameter::Cookie { parameter_data, .. } => parameter_data,
        };
        self.walk_parameter_data(data, components)
    }

    fn walk_parameter_data(
        &mut self,
        data: &mut openapiv3::ParameterData,
        components: &mut Components,
    ) -> Result<()> {
        match &mut data.format {
            ParameterSchemaOrContent::Schema(schema_ref) => {
                self.resolve_schema_ref(schema_ref, &mut components.schemas)?;
            }
            ParameterSchemaOrContent::Content(content) => {
                for media in content.values_mut() {
                    self.walk_media_type(media, components)?;
                }
            }
        }
        for example in data.examples.values_mut() {
            self.resolve_example_ref(example, components)?;
        }
        Ok(())
    }

    fn walk_header(&mut self, header: &mut Header, components: &mut Components) -> Result<()> {
        match &mut header.format {
            ParameterSchemaOrContent::Schema(schema_ref) => {
                self.resolve_schema_ref(schema_ref, &mut components.schemas)?;
            }
            ParameterSchemaOrContent::Content(content) => {
                for media in content.values_mut() {
                    self.walk_media_type(media, components)?;
                }
            }
        }
        for example in header.examples.values_mut() {
            self.resolve_example_ref(example, components)?;
        }
        Ok(())
    }

    fn walk_request_body(
        &mut self,
        body: &mut RequestBody,
        components: &mut Components,
    ) -> Result<()> {
        for media in body.content.values_mut() {
            self.walk_media_type(media, components)?;
        }
        Ok(())
    }

    fn walk_response(&mut self, response: &mut Response, components: &mut Components) -> Result<()> {
        for header in response.headers.values_mut() {
            self.resolve_header_ref(header, components)?;
        }
        for media in response.content.values_mut() {
            self.walk_media_type(media, components)?;
        }
        for link in response.links.values_mut() {
            self.resolve_link_ref(link, components)?;
        }
        Ok(())
    }

    fn walk_media_type(&mut self, media: &mut MediaType, components: &mut Components) -> Result<()> {
        if let Some(schema_ref) = media.schema.as_mut() {
            self.resolve_schema_ref(schema_ref, &mut components.schemas)?;
        }
        for example in media.examples.values_mut() {
            self.resolve_example_ref(example, components)?;
        }
        for encoding in media.encoding.values_mut() {
            for header in encoding.headers.values_mut() {
                self.resolve_header_ref(header, components)?;
            }
        }
        Ok(())
    }

    fn walk_callback(&mut self, callback: &mut Callback, components: &mut Components) -> Result<()> {
        for item in callback.values_mut() {
            self.resolve_path_item(item, components)?;
        }
        Ok(())
    }

    fn decode_pointer_segment(segment: &str) -> String {
        segment.replace("~1", "/").replace("~0", "~")
    }


    fn noop_walk<T>(_: &mut T) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use openapiv3::{
        Callback, Example, Header, HeaderStyle, Link, LinkOperation, MediaType, Operation,
        Parameter, ParameterData, ParameterSchemaOrContent, PathItem, QueryStyle, RequestBody,
        SchemaKind, SecurityRequirement, SecurityScheme, StatusCode, Type,
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
        components.security_schemes.insert(
            "Bearer".into(),
            ReferenceOr::Item(SecurityScheme::HTTP {
                scheme: "bearer".into(),
                bearer_format: Some("JWT".into()),
                description: None,
                extensions: IndexMap::new(),
            }),
        );
        components.security_schemes.insert(
            "Alias".into(),
            ReferenceOr::Reference {
                reference: "#/components/securitySchemes/Bearer".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.security_schemes.get("Alias").unwrap() {
            ReferenceOr::Item(SecurityScheme::HTTP { scheme, .. }) => {
                assert_eq!(scheme, "bearer");
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
                description: Some("link to logs".into()),
                operation: LinkOperation::OperationId("get_log".into()),
                request_body: None,
                parameters: IndexMap::new(),
                server: None,
                extensions: IndexMap::new(),
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
            ReferenceOr::Item(link) => match &link.operation {
                LinkOperation::OperationId(id) => assert_eq!(id, "get_log"),
                other => panic!("unexpected link operation {other:?}"),
            },
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved link, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_callback_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        let mut callback: Callback = IndexMap::new();
        callback.insert(
            "{$request.body#/callback}".into(),
            PathItem::default(),
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
                assert!(callback.iter().any(|(expr, _)| expr.contains("callback")));
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved callback, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_operation_references() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();

        components.schemas.insert(
            "Payload".into(),
            ReferenceOr::Item(simple_schema(Type::String(Default::default()))),
        );

        let mut body = RequestBody::default();
        let mut body_media = MediaType::default();
        body_media.schema = Some(ReferenceOr::Reference {
            reference: "#/components/schemas/Payload".into(),
        });
        body.content.insert("application/json".into(), body_media);
        components
            .request_bodies
            .insert("Create".into(), ReferenceOr::Item(body));

        let mut response = Response {
            description: "ok".into(),
            ..Response::default()
        };
        let mut response_media = MediaType::default();
        response_media.schema = Some(ReferenceOr::Reference {
            reference: "#/components/schemas/Payload".into(),
        });
        response
            .content
            .insert("application/json".into(), response_media);
        components
            .responses
            .insert("Ok".into(), ReferenceOr::Item(response));

        let mut operation = Operation::default();
        operation.request_body = Some(ReferenceOr::Reference {
            reference: "#/components/requestBodies/Create".into(),
        });
        operation.responses.responses.insert(
            StatusCode::Code(200),
            ReferenceOr::Reference {
                reference: "#/components/responses/Ok".into(),
            },
        );

        let mut item = PathItem::default();
        item.post = Some(operation);
        doc.paths
            .paths
            .insert("/test".into(), ReferenceOr::Item(item));

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let path_item = match resolved.paths.paths.get("/test").unwrap() {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved path item, found reference {reference}")
            }
        };
        let operation = path_item.post.as_ref().expect("missing post operation");
        let body = match operation.request_body.as_ref().unwrap() {
            ReferenceOr::Item(body) => body,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved request body, found reference {reference}")
            }
        };
        let media = body.content.get("application/json").unwrap();
        match media.schema.as_ref().unwrap() {
            ReferenceOr::Item(schema) => match &schema.schema_kind {
                SchemaKind::Type(Type::String(_)) => {}
                other => panic!("unexpected schema kind {other:?}"),
            },
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved schema, found reference {reference}")
            }
        }

        let response = match operation.responses.responses.get(&StatusCode::Code(200)).unwrap() {
            ReferenceOr::Item(response) => response,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved response, found reference {reference}")
            }
        };
        let media = response.content.get("application/json").unwrap();
        match media.schema.as_ref().unwrap() {
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
    fn resolves_security_requirements() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.security_schemes.insert(
            "ApiKey".into(),
            ReferenceOr::Item(SecurityScheme::APIKey {
                location: openapiv3::APIKeyLocation::Header,
                name: "X-API-Key".into(),
                description: None,
                extensions: IndexMap::new(),
            }),
        );

        let mut requirement = SecurityRequirement::new();
        requirement.insert("ApiKey".into(), Vec::new());
        doc.security = Some(vec![requirement.clone()]);

        let mut operation = Operation::default();
        operation.security = Some(vec![requirement]);
        let mut item = PathItem::default();
        item.get = Some(operation);
        doc.paths
            .paths
            .insert("/secure".into(), ReferenceOr::Item(item));

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        match components.security_schemes.get("ApiKey").unwrap() {
            ReferenceOr::Item(SecurityScheme::APIKey { name, .. }) => {
                assert_eq!(name, "X-API-Key");
            }
            other => panic!("unexpected security scheme {other:?}"),
        }
    }

    #[test]
    fn preserves_recursive_schema_refs() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();

        let mut node = simple_schema(Type::Object(Default::default()));
        if let SchemaKind::Type(Type::Object(obj)) = &mut node.schema_kind {
            obj.properties.insert(
                "child".into(),
                ReferenceOr::Reference {
                    reference: "#/components/schemas/Node".into(),
                },
            );
        }

        components
            .schemas
            .insert("Node".into(), ReferenceOr::Item(node));

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let components = resolved.components.unwrap();
        let schema = match components.schemas.get("Node").unwrap() {
            ReferenceOr::Item(schema) => schema,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved schema, found reference {reference}")
            }
        };
        let SchemaKind::Type(Type::Object(obj)) = &schema.schema_kind else {
            panic!("expected object schema");
        };
        match obj.properties.get("child").unwrap() {
            ReferenceOr::Reference { reference } => {
                assert_eq!(reference, "#/components/schemas/Node");
            }
            ReferenceOr::Item(_) => {
                panic!("expected recursive reference to remain");
            }
        }
    }

    #[test]
    fn resolves_path_item_references() {
        let mut doc = base_openapi();
        let mut item = PathItem::default();
        item.get = Some(Operation::default());
        doc.paths
            .paths
            .insert("/base".into(), ReferenceOr::Item(item));
        doc.paths.paths.insert(
            "/alias".into(),
            ReferenceOr::Reference {
                reference: "#/paths/~1base".into(),
            },
        );

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        match resolved.paths.paths.get("/alias").unwrap() {
            ReferenceOr::Item(item) => {
                assert!(item.get.is_some());
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved path item, found reference {reference}")
            }
        }
    }

    #[test]
    fn resolves_parameter_content_media_refs() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "Payload".into(),
            ReferenceOr::Item(simple_schema(Type::String(Default::default()))),
        );

        let mut media = MediaType::default();
        media.schema = Some(ReferenceOr::Reference {
            reference: "#/components/schemas/Payload".into(),
        });

        let mut content = IndexMap::new();
        content.insert("application/json".into(), media);

        let param = Parameter::Query {
            parameter_data: ParameterData {
                name: "filter".into(),
                description: None,
                required: true,
                deprecated: None,
                format: ParameterSchemaOrContent::Content(content),
                example: None,
                examples: IndexMap::new(),
                explode: None,
                extensions: IndexMap::new(),
            },
            allow_reserved: false,
            style: QueryStyle::default(),
            allow_empty_value: None,
        };

        components
            .parameters
            .insert("Filter".into(), ReferenceOr::Item(param));

        let mut op = Operation::default();
        op.parameters.push(ReferenceOr::Reference {
            reference: "#/components/parameters/Filter".into(),
        });
        let mut item = PathItem::default();
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/items".into(), ReferenceOr::Item(item));

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let path_item = match resolved.paths.paths.get("/items").unwrap() {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved path item, found reference {reference}")
            }
        };
        let op = path_item.get.as_ref().unwrap();
        let param = match op.parameters.first().unwrap() {
            ReferenceOr::Item(param) => param,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved parameter, found reference {reference}")
            }
        };
        let Parameter::Query { parameter_data, .. } = param else {
            panic!("expected query parameter");
        };
        let ParameterSchemaOrContent::Content(content) = &parameter_data.format else {
            panic!("expected content parameter");
        };
        let media = content.get("application/json").unwrap();
        match media.schema.as_ref().unwrap() {
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
    fn resolves_response_headers_and_encoding_refs() {
        let mut doc = base_openapi();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "Payload".into(),
            ReferenceOr::Item(simple_schema(Type::String(Default::default()))),
        );
        components.headers.insert(
            "RateLimit".into(),
            ReferenceOr::Item(Header {
                description: Some("rate limit".into()),
                style: HeaderStyle::default(),
                required: false,
                deprecated: None,
                format: ParameterSchemaOrContent::Schema(ReferenceOr::Reference {
                    reference: "#/components/schemas/Payload".into(),
                }),
                example: None,
                examples: IndexMap::new(),
                extensions: IndexMap::new(),
            }),
        );

        let mut response = Response {
            description: "ok".into(),
            ..Response::default()
        };
        response.headers.insert(
            "X-RateLimit".into(),
            ReferenceOr::Reference {
                reference: "#/components/headers/RateLimit".into(),
            },
        );

        let mut media = MediaType::default();
        media.schema = Some(ReferenceOr::Reference {
            reference: "#/components/schemas/Payload".into(),
        });
        let mut encoding = openapiv3::Encoding::default();
        encoding.headers.insert(
            "X-Enc".into(),
            ReferenceOr::Reference {
                reference: "#/components/headers/RateLimit".into(),
            },
        );
        media.encoding.insert("payload".into(), encoding);
        response.content.insert("application/json".into(), media);

        let mut op = Operation::default();
        op.responses.responses.insert(
            StatusCode::Code(200),
            ReferenceOr::Item(response),
        );

        let mut item = PathItem::default();
        item.get = Some(op);
        doc.paths
            .paths
            .insert("/resp".into(), ReferenceOr::Item(item));

        let resolved = Resolver::new(doc).resolve().expect("resolve succeeds");
        let path_item = match resolved.paths.paths.get("/resp").unwrap() {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved path item, found reference {reference}")
            }
        };
        let op = path_item.get.as_ref().unwrap();
        let response = match op.responses.responses.get(&StatusCode::Code(200)).unwrap() {
            ReferenceOr::Item(response) => response,
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved response, found reference {reference}")
            }
        };
        match response.headers.get("X-RateLimit").unwrap() {
            ReferenceOr::Item(Header { description, .. }) => {
                assert_eq!(description.as_deref(), Some("rate limit"));
            }
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved header, found reference {reference}")
            }
        }
        let media = response.content.get("application/json").unwrap();
        let encoding = media.encoding.get("payload").unwrap();
        match encoding.headers.get("X-Enc").unwrap() {
            ReferenceOr::Item(_) => {}
            ReferenceOr::Reference { reference } => {
                panic!("expected resolved encoding header, found reference {reference}")
            }
        }
    }
}
