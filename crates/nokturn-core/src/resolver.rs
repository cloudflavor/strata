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

use std::collections::HashMap;

use crate::ResolvedSpec;
use anyhow::Result;
use openapiv3::OpenAPI;

pub struct EagerResolver {
    spec: OpenAPI,
    resolved: Option<ResolvedSpec>,
}

impl EagerResolver {
    pub fn new(spec: OpenAPI) -> Self {
        Self {
            spec,
            resolved: None,
        }
    }

    pub fn resolve(self) -> Self {
        let _paths = self.resolve_paths();
        let resolved = ResolvedSpec {
            models: HashMap::new(),
        };

        Self {
            spec: self.spec,
            resolved: Some(resolved),
        }
    }

    fn resolve_paths(&self) -> Result<()> {
        for (name, path_item) in self.spec.paths.iter() {
            println!("{name}, {:#?}", path_item);
        }
        Ok(())
    }
}
