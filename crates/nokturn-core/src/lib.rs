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

pub mod resolver;

use crate::resolver::EagerResolver;
use anyhow::{Result, bail};
use std::collections::HashMap;

pub struct ResolvedSchema;

pub struct ResolvedSpec {
    pub models: HashMap<String, ResolvedSchema>,
}

pub fn resolve_schema(spec: &str, ext: &str) -> Result<()> {
    let s = sanitize_spec(spec);
    let schema = load_openapiv3(s.as_str(), ext)?;
    let resolver = EagerResolver::new(schema);
    let _resolved_schema = resolver.resolve();

    Ok(())
}

pub fn load_openapiv3(spec: &str, ext: &str) -> Result<openapiv3::OpenAPI> {
    let schema_json: serde_json::Value = match ext {
        "yaml" | "yml" => serde_yaml::from_str(spec)?,
        "json" => serde_json::from_str(spec)?,
        _ => bail!("format not recognized"),
    };

    let spec = serde_json::from_value(schema_json)?;

    Ok(spec)
}

pub fn sanitize_spec(s: &str) -> String {
    s.replace("18446744073709552000", "18446744073709551615")
}

mod tests {

    #[test]
    fn test_sanitize_code() {
        let spec = super::sanitize_spec("my_spec_18446744073709552000");
        assert_eq!(spec, String::from("my_spec_18446744073709551615"));
    }
}
