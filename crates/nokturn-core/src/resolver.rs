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
use openapiv3::RequestBody;
use openapiv3::{
    Callback, Header, OpenAPI, Parameter, PathItem, ReferenceOr, Response, Responses, Schema,
};
use std::collections::HashMap;
use tracing::{debug, warn};

#[derive(Debug)]
pub(crate) struct ResolvedCache<'a> {
    paths: HashMap<&'a str, &'a PathItem>,
    params: HashMap<&'a str, &'a Parameter>,
    schemas: HashMap<&'a str, &'a Schema>,
    responses: HashMap<String, &'a Response>,
    headers: HashMap<&'a str, &'a Header>,
    callbacks: HashMap<&'a str, &'a Callback>,
    req_bodies: HashMap<&'a str, &'a RequestBody>,
}

pub struct EagerResolver<'a> {
    spec: &'a OpenAPI,
    // resolved: Option<ResolvedSpec>,
    cache: ResolvedCache<'a>,
    // visited: HashSet<String>,
}

impl<'a> EagerResolver<'a> {
    pub fn new(spec: &'a OpenAPI) -> Self {
        let paths_cap = spec.paths.paths.len();
        let (param_cap, schema_cap, response_cap, header_cap, callback_cap, rbody_cap) = spec
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
            },
            // visited: HashSet::new(),
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
                    self.resolve_operation(item)?;
                    self.cache.paths.insert(name, item);
                }
                ReferenceOr::Reference { reference } => {
                    self.resolve_ref(reference)?;
                }
            }
        }
        Ok(())
    }

    fn resolve_operation(&mut self, path_item: &'a PathItem) -> Result<()> {
        for op in [
            path_item.get.as_ref(),
            path_item.put.as_ref(),
            path_item.post.as_ref(),
            path_item.delete.as_ref(),
            path_item.patch.as_ref(),
            path_item.options.as_ref(),
            path_item.head.as_ref(),
            path_item.trace.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.resolve_responses(&op.responses)?;
            self.resolve_req_body(op.request_body)?;
        }

        Ok(())
    }

    fn resolve_responses(&mut self, responses: &'a Responses) -> Result<()> {
        for (status, resp) in responses.responses.iter() {
            match resp {
                ReferenceOr::Item(item) => {
                    self.resolve_headers(item.headers.iter())?;
                    self.cache.responses.insert(status.to_string(), item);
                }
                ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref())?,
            }
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
                }
                ReferenceOr::Reference { reference } => self.resolve_ref(reference)?,
            }
        }
        Ok(())
    }

    fn resolve_req_body_ref(&mut self, req_body: &'a RequestBody) -> Result<()> {
        match req_body {
            ReferenceOr::Item(item) => Ok(()),
            ReferenceOr::Reference { reference } => self.resolve_ref(reference),
        }
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
                Ok(())
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_ref(&mut self, reference: &'a str) -> Result<()> {
        match ComponentRef::parse(reference) {
            Some(ComponentRef::Parameter(name)) => self.resolve_parameter_ref(name),
            Some(ComponentRef::Response(name)) => self.resolve_response_ref(name),
            Some(ComponentRef::Header(name)) => self.resolve_header_ref(name),
            Some(ComponentRef::Schema(name)) => self.resolve_schema_ref(name),
            Some(ComponentRef::Callback(name)) => self.resolve_callback_ref(name),
            None => {
                warn!("unresolved component reference: {reference}");
                Err(anyhow!("unresolved component reference: {reference} "))
            }
        }
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
                Ok(())
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
                Ok(())
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }

    fn resolve_schema_ref(&mut self, name: &'a str) -> Result<()> {
        let comps = self
            .spec
            .components
            .as_ref()
            .ok_or_else(|| anyhow!("spec has no components"))?;

        match &comps.schemas[name] {
            ReferenceOr::Item(item) => {
                self.cache.schemas.insert(name, &item);
                Ok(())
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
                Ok(())
            }
            ReferenceOr::Reference { reference } => self.resolve_ref(reference.as_ref()),
        }
    }
}

enum ComponentRef<'a> {
    Parameter(&'a str),
    Response(&'a str),
    Header(&'a str),
    Schema(&'a str),
    Callback(&'a str),
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
