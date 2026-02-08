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
use openapiv3::{AnySchema, OpenAPI, ReferenceOr, Schema, SchemaKind, Type};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Entrypoint for transforming resolved OpenAPI schemas into model definitions.
#[derive(Debug, Default)]
pub struct ModelGenerator {
    module_name_map: HashMap<String, String>,
}

impl ModelGenerator {
    /// Create a new model generator with default settings.
    pub fn new() -> Self {
        Self {
            module_name_map: HashMap::new(),
        }
    }

    pub fn with_module_name_map(module_name_map: HashMap<String, String>) -> Self {
        Self { module_name_map }
    }

    /// Collect model definitions from a resolved OpenAPI document.
    ///
    /// This walks `components.schemas` and converts each schema into an
    /// intermediate `ModelDef` that can later be rendered into Rust code.
    pub fn collect_models(&mut self, doc: &OpenAPI) -> Result<ModelRegistry> {
        let mut registry = ModelRegistry::default();
        let Some(components) = doc.components.as_ref() else {
            return Ok(registry);
        };

        self.module_name_map = build_component_module_name_map(components.schemas.keys());
        registry.name_map = self.module_name_map.clone();

        for (name, schema) in components.schemas.iter() {
            let def = self.schema_to_model(name, schema)?;
            registry.models.insert(def.name.clone(), def);
        }

        self.hoist_inline_models(&mut registry)?;

        Ok(registry)
    }

    /// Convert a component schema into a model definition.
    ///
    /// References are preserved as `ModelType::Ref`, while inline schemas are
    /// converted based on their `SchemaKind`.
    pub fn schema_to_model(&self, name: &str, schema: &ReferenceOr<Schema>) -> Result<ModelDef> {
        let model_type = match schema {
            ReferenceOr::Item(schema) => self.schema_kind_to_model_type(&schema.schema_kind)?,
            ReferenceOr::Reference { reference } => {
                ModelType::Ref(self.module_name_for_schema(
                    self.ref_name(reference).unwrap_or(reference),
                ))
            }
        };
        let render_type = model_type_to_rust(&model_type);
        let module_name = self.module_name_for_schema(name);
        let rust_name = sanitize_type_name(&module_name);

        Ok(ModelDef {
            name: module_name,
            rust_name,
            render_type,
            deps: Vec::new(),
            dep_imports: Vec::new(),
            kind: model_type,
        })
    }

    /// Convert a schema kind into a model type.
    ///
    /// This is the core mapping from OpenAPI shapes to a Rust-friendly
    /// intermediate representation.
    pub fn schema_kind_to_model_type(&self, kind: &SchemaKind) -> Result<ModelType> {
        match kind {
            SchemaKind::Type(typ) => self.type_to_model_type(typ),
            SchemaKind::OneOf { one_of } => {
                let mut variants = self.refs_to_types(one_of)?;
                variants.retain(|variant| !matches!(variant, ModelType::Opaque));
                dedupe_composite_variants(&mut variants);
                if variants.is_empty() {
                    return Ok(ModelType::Opaque);
                }
                let render_variants = variants.iter().map(model_type_to_rust).collect();
                Ok(ModelType::Composite(ModelComposite {
                    flavor: CompositeFlavor::OneOf,
                    variants,
                    render_variants,
                }))
            }
            SchemaKind::AnyOf { any_of } => {
                let mut variants = self.refs_to_types(any_of)?;
                variants.retain(|variant| !matches!(variant, ModelType::Opaque));
                dedupe_composite_variants(&mut variants);
                if variants.is_empty() {
                    return Ok(ModelType::Opaque);
                }
                let render_variants = variants.iter().map(model_type_to_rust).collect();
                Ok(ModelType::Composite(ModelComposite {
                    flavor: CompositeFlavor::AnyOf,
                    variants,
                    render_variants,
                }))
            }
            SchemaKind::AllOf { all_of } => self.all_of_to_struct(all_of, None),
            SchemaKind::Not { .. } => Ok(ModelType::Opaque),
            SchemaKind::Any(schema) => self.any_schema_to_model_type(schema),
        }
    }

    fn module_name_for_schema(&self, name: &str) -> String {
        self.module_name_map
            .get(name)
            .cloned()
            .unwrap_or_else(|| sanitize_module_name(name))
    }

    fn hoist_inline_models(&self, registry: &mut ModelRegistry) -> Result<()> {
        loop {
            let mut additions: Vec<(String, ModelDef)> = Vec::new();
            let mut changed = false;
            let mut existing: indexmap::IndexSet<String> = registry
                .models
                .values()
                .map(|model| model.name.clone())
                .collect();
            let mut name_to_sig = HashMap::new();
            let mut sig_to_name = HashMap::new();

            for (name, model) in registry.models.iter() {
                if let ModelType::Struct(def) = &model.kind {
                    let sig = struct_signature(def);
                    name_to_sig.insert(name.clone(), sig.clone());
                    sig_to_name.entry(sig).or_insert_with(|| name.clone());
                }
            }

            for model in registry.models.values_mut() {
                if self.hoist_in_model(
                    model,
                    &mut additions,
                    &mut existing,
                    &mut name_to_sig,
                    &mut sig_to_name,
                )? {
                    changed = true;
                }
            }

            if additions.is_empty() {
                if !changed {
                    break;
                }
                continue;
            }

            for (name, def) in additions {
                if !registry.models.contains_key(&name) {
                    registry.models.insert(name.clone(), def);
                    existing.insert(name);
                    changed = true;
                }
            }

            if !changed {
                break;
            }
        }

        self.refresh_render_types(registry);
        self.refresh_type_map(registry);
        self.refresh_deps(registry);

        Ok(())
    }

    fn hoist_in_model(
        &self,
        model: &mut ModelDef,
        additions: &mut Vec<(String, ModelDef)>,
        existing: &mut indexmap::IndexSet<String>,
        name_to_sig: &mut HashMap<String, String>,
        sig_to_name: &mut HashMap<String, String>,
    ) -> Result<bool> {
        let mut changed = false;
        match &mut model.kind {
            ModelType::Struct(def) => {
                for field in def.fields.iter_mut() {
                    changed |= self.hoist_in_type(
                        &model.name,
                        Some(field.name.as_str()),
                        &mut field.typ,
                        additions,
                        existing,
                        name_to_sig,
                        sig_to_name,
                    )?;
                }
            }
            ModelType::Array(inner) => {
                changed |= self.hoist_in_type(
                    &model.name,
                    None,
                    inner.as_mut(),
                    additions,
                    existing,
                    name_to_sig,
                    sig_to_name,
                )?;
            }
            ModelType::Composite(comp) => {
                for variant in comp.variants.iter_mut() {
                    changed |= self.hoist_in_type(
                        &model.name,
                        None,
                        variant,
                        additions,
                        existing,
                        name_to_sig,
                        sig_to_name,
                    )?;
                }
            }
            _ => {}
        }

        if changed {
            model.render_type = model_type_to_rust(&model.kind);
        }

        Ok(changed)
    }

    fn hoist_in_type(
        &self,
        parent_name: &str,
        field_name: Option<&str>,
        typ: &mut ModelType,
        additions: &mut Vec<(String, ModelDef)>,
        existing: &mut indexmap::IndexSet<String>,
        name_to_sig: &mut HashMap<String, String>,
        sig_to_name: &mut HashMap<String, String>,
    ) -> Result<bool> {
        match typ {
            ModelType::Struct(def) => {
                let sig = struct_signature(def);
                if let Some(existing_name) = sig_to_name.get(&sig) {
                    *typ = ModelType::Ref(existing_name.clone());
                    return Ok(true);
                }
                let name = self.inline_model_name(
                    parent_name,
                    field_name,
                    false,
                    &sig,
                    existing,
                    name_to_sig,
                );
                let rust_name = sanitize_type_name(&name);
                let render_type = String::new();
                let def = ModelDef {
                    name: name.clone(),
                    rust_name,
                    render_type,
                    deps: Vec::new(),
                    dep_imports: Vec::new(),
                    kind: ModelType::Struct(ModelStruct {
                        fields: def.fields.clone(),
                    }),
                };
                additions.push((name.clone(), def));
                name_to_sig.insert(name.clone(), sig.clone());
                sig_to_name.insert(sig, name.clone());
                *typ = ModelType::Ref(name);
                Ok(true)
            }
            ModelType::Array(inner) => match inner.as_mut() {
                ModelType::Struct(def) => {
                    let sig = struct_signature(def);
                    if let Some(existing_name) = sig_to_name.get(&sig) {
                        *inner = Box::new(ModelType::Ref(existing_name.clone()));
                        return Ok(true);
                    }
                    let name = self.inline_model_name(
                        parent_name,
                        field_name,
                        true,
                        &sig,
                        existing,
                        name_to_sig,
                    );
                    let rust_name = sanitize_type_name(&name);
                    let render_type = String::new();
                    let def = ModelDef {
                        name: name.clone(),
                        rust_name,
                        render_type,
                        deps: Vec::new(),
                        dep_imports: Vec::new(),
                        kind: ModelType::Struct(ModelStruct {
                            fields: def.fields.clone(),
                        }),
                    };
                    additions.push((name.clone(), def));
                    name_to_sig.insert(name.clone(), sig.clone());
                    sig_to_name.insert(sig, name.clone());
                    *inner = Box::new(ModelType::Ref(name));
                    Ok(true)
                }
                _ => self.hoist_in_type(
                    parent_name,
                    field_name,
                    inner.as_mut(),
                    additions,
                    existing,
                    name_to_sig,
                    sig_to_name,
                ),
            },
            ModelType::Composite(comp) => {
                for variant in comp.variants.iter_mut() {
                    self.hoist_in_type(
                        parent_name,
                        field_name,
                        variant,
                        additions,
                        existing,
                        name_to_sig,
                        sig_to_name,
                    )?;
                }
                dedupe_composite_variants(&mut comp.variants);
                comp.render_variants = comp.variants.iter().map(model_type_to_rust).collect();

                let sig = {
                    let snapshot = ModelType::Composite(comp.clone());
                    type_signature(&snapshot)
                };
                if let Some(existing_name) = sig_to_name.get(&sig) {
                    *typ = ModelType::Ref(existing_name.clone());
                    return Ok(true);
                }

                let name = self.inline_model_name(
                    parent_name,
                    field_name,
                    false,
                    &sig,
                    existing,
                    name_to_sig,
                );
                let rust_name = sanitize_type_name(&name);
                let render_type = String::new();
                let def = ModelDef {
                    name: name.clone(),
                    rust_name,
                    render_type,
                    deps: Vec::new(),
                    dep_imports: Vec::new(),
                    kind: ModelType::Composite(comp.clone()),
                };
                additions.push((name.clone(), def));
                name_to_sig.insert(name.clone(), sig.clone());
                sig_to_name.insert(sig, name.clone());
                *typ = ModelType::Ref(name);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn inline_model_name(
        &self,
        parent_name: &str,
        field_name: Option<&str>,
        array_item: bool,
        signature: &str,
        existing: &mut indexmap::IndexSet<String>,
        name_to_sig: &HashMap<String, String>,
    ) -> String {
        let mut base = parent_name.to_string();
        if let Some(field) = field_name {
            base.push('_');
            base.push_str(field);
        }
        if array_item {
            base.push_str("_item");
        }

        let base = sanitize_module_name(&base);

        if let Some(existing_sig) = name_to_sig.get(&base) {
            if existing_sig == signature {
                return base;
            }
        }

        if !existing.contains(&base) {
            existing.insert(base.clone());
            return base;
        }

        unique_name_with_hash(&base, signature, existing, name_to_sig)
    }

    fn refresh_render_types(&self, registry: &mut ModelRegistry) {
        for model in registry.models.values_mut() {
            model.render_type = model_type_to_rust(&model.kind);
            match &mut model.kind {
                ModelType::Struct(def) => {
                    for field in &mut def.fields {
                        field.render_type = model_type_to_rust(&field.typ);
                    }
                }
                ModelType::Composite(comp) => {
                    dedupe_composite_variants(&mut comp.variants);
                    comp.render_variants = comp.variants.iter().map(model_type_to_rust).collect();
                }
                _ => {}
            }
        }
    }

    fn refresh_type_map(&self, registry: &mut ModelRegistry) {
        registry.type_map = registry
            .models
            .values()
            .map(|model| (model.rust_name.clone(), model.name.clone()))
            .collect();
    }

    fn refresh_deps(&self, registry: &mut ModelRegistry) {
        let type_map = registry.type_map.clone();
        for model in registry.models.values_mut() {
            model.deps = collect_model_deps(model);
            model.dep_imports = group_dep_imports(&model.deps, &type_map);
        }
    }

    /// Convert a concrete schema type into a model type.
    pub fn type_to_model_type(&self, typ: &Type) -> Result<ModelType> {
        match typ {
            Type::String(_) => Ok(ModelType::Primitive(PrimitiveType::String)),
            Type::Number(_) => Ok(ModelType::Primitive(PrimitiveType::Number)),
            Type::Integer(_) => Ok(ModelType::Primitive(PrimitiveType::Integer)),
            Type::Boolean(_) => Ok(ModelType::Primitive(PrimitiveType::Boolean)),
            Type::Array(array) => {
                let item = match array.items.as_ref() {
                    Some(item) => self.schema_ref_boxed_to_type(item)?,
                    None => ModelType::Opaque,
                };
                Ok(ModelType::Array(Box::new(item)))
            }
            Type::Object(obj) => {
                let fields = self.object_properties_to_fields(&obj.properties, &obj.required)?;
                Ok(ModelType::Struct(ModelStruct { fields }))
            }
        }
    }

    /// Convert a schema reference into a model type.
    pub fn schema_ref_to_type(&self, schema: &ReferenceOr<Schema>) -> Result<ModelType> {
        match schema {
            ReferenceOr::Item(schema) => self.schema_kind_to_model_type(&schema.schema_kind),
            ReferenceOr::Reference { reference } => Ok(ModelType::Ref(
                self.module_name_for_schema(self.ref_name(reference).unwrap_or(reference)),
            )),
        }
    }

    /// Convert a boxed schema reference into a model type.
    pub fn schema_ref_boxed_to_type(&self, schema: &ReferenceOr<Box<Schema>>) -> Result<ModelType> {
        match schema {
            ReferenceOr::Item(schema) => self.schema_kind_to_model_type(&schema.schema_kind),
            ReferenceOr::Reference { reference } => Ok(ModelType::Ref(
                self.module_name_for_schema(self.ref_name(reference).unwrap_or(reference)),
            )),
        }
    }

    /// Convert a list of schema references into model types.
    pub fn refs_to_types(&self, refs: &[ReferenceOr<Schema>]) -> Result<Vec<ModelType>> {
        let mut out = Vec::with_capacity(refs.len());
        for schema in refs {
            out.push(self.schema_ref_to_type(schema)?);
        }
        Ok(out)
    }

    /// Convert an OpenAPI "any" schema into a model type.
    pub fn any_schema_to_model_type(&self, schema: &AnySchema) -> Result<ModelType> {
        if !schema.all_of.is_empty() {
            let extra_fields = if schema.properties.is_empty() {
                None
            } else {
                Some(self.object_properties_to_fields(&schema.properties, &schema.required)?)
            };
            return self.all_of_to_struct(&schema.all_of, extra_fields);
        }

        if !schema.one_of.is_empty() {
            let mut variants = self.refs_to_types(&schema.one_of)?;
            variants.retain(|variant| !matches!(variant, ModelType::Opaque));
            dedupe_composite_variants(&mut variants);
            if variants.is_empty() {
                return Ok(ModelType::Opaque);
            }
            let render_variants = variants.iter().map(model_type_to_rust).collect();
            return Ok(ModelType::Composite(ModelComposite {
                flavor: CompositeFlavor::OneOf,
                variants,
                render_variants,
            }));
        }

        if !schema.any_of.is_empty() {
            let mut variants = self.refs_to_types(&schema.any_of)?;
            variants.retain(|variant| !matches!(variant, ModelType::Opaque));
            dedupe_composite_variants(&mut variants);
            if variants.is_empty() {
                return Ok(ModelType::Opaque);
            }
            let render_variants = variants.iter().map(model_type_to_rust).collect();
            return Ok(ModelType::Composite(ModelComposite {
                flavor: CompositeFlavor::AnyOf,
                variants,
                render_variants,
            }));
        }

        match schema.typ.as_deref() {
            Some("string") => return Ok(ModelType::Primitive(PrimitiveType::String)),
            Some("number") => return Ok(ModelType::Primitive(PrimitiveType::Number)),
            Some("integer") => return Ok(ModelType::Primitive(PrimitiveType::Integer)),
            Some("boolean") => return Ok(ModelType::Primitive(PrimitiveType::Boolean)),
            Some("array") => {
                let item = match schema.items.as_ref() {
                    Some(item) => self.schema_ref_boxed_to_type(item)?,
                    None => ModelType::Opaque,
                };
                return Ok(ModelType::Array(Box::new(item)));
            }
            Some("object") => {
                let fields =
                    self.object_properties_to_fields(&schema.properties, &schema.required)?;
                return Ok(ModelType::Struct(ModelStruct { fields }));
            }
            _ => {}
        }

        if !schema.properties.is_empty() {
            let fields = self.object_properties_to_fields(&schema.properties, &schema.required)?;
            return Ok(ModelType::Struct(ModelStruct { fields }));
        }

        if let Some(items) = schema.items.as_ref() {
            let item = self.schema_ref_boxed_to_type(items)?;
            return Ok(ModelType::Array(Box::new(item)));
        }

        Ok(ModelType::Opaque)
    }

    fn all_of_to_struct(
        &self,
        all_of: &[ReferenceOr<Schema>],
        extra_fields: Option<Vec<ModelField>>,
    ) -> Result<ModelType> {
        let mut fields = extra_fields.unwrap_or_default();
        let mut field_index: HashMap<String, usize> = fields
            .iter()
            .enumerate()
            .map(|(idx, field)| (field.name.clone(), idx))
            .collect();
        let mut part_index = 1;

        for schema in all_of {
            let model = self.schema_ref_to_type(schema)?;
            match model {
                ModelType::Struct(def) => {
                    for field in def.fields {
                        if let Some(idx) = field_index.get(&field.name).copied() {
                            fields[idx] = field;
                        } else {
                            field_index.insert(field.name.clone(), fields.len());
                            fields.push(field);
                        }
                    }
                }
                other => {
                    let name = format!("part_{}", part_index);
                    part_index += 1;
                    let render_type = model_type_to_rust(&other);
                    let flatten = matches!(other, ModelType::Ref(_));
                    let field = ModelField {
                        name: name.clone(),
                        rust_name: name,
                        required: true,
                        nullable: false,
                        typ: other,
                        render_type,
                        flatten,
                    };
                    if let Some(idx) = field_index.get(&field.name).copied() {
                        fields[idx] = field;
                    } else {
                        field_index.insert(field.name.clone(), fields.len());
                        fields.push(field);
                    }
                }
            }
        }

        for field in &mut fields {
            if field.render_type.is_empty() {
                field.render_type = model_type_to_rust(&field.typ);
            }
        }

        Ok(ModelType::Struct(ModelStruct { fields }))
    }

    fn object_properties_to_fields(
        &self,
        properties: &IndexMap<String, ReferenceOr<Box<Schema>>>,
        required: &[String],
    ) -> Result<Vec<ModelField>> {
        let mut fields = Vec::new();
        for (name, schema) in properties.iter() {
            let nullable = match schema {
                ReferenceOr::Item(schema) => schema.schema_data.nullable,
                ReferenceOr::Reference { .. } => false,
            };
            let typ = self.schema_ref_boxed_to_type(schema)?;
            let render_type = model_type_to_rust(&typ);
            fields.push(ModelField {
                name: name.clone(),
                rust_name: sanitize_field_name(name),
                required: required.contains(name),
                nullable,
                typ,
                render_type,
                flatten: false,
            });
        }
        Ok(fields)
    }

    /// Extract a component schema name from a `$ref` string.
    ///
    /// Supports `#/components/schemas/<Name>` and returns `None` for other refs.
    pub fn ref_name<'a>(&self, reference: &'a str) -> Option<&'a str> {
        reference.strip_prefix("#/components/schemas/")
    }
}

/// In-memory registry of model definitions discovered from the spec.
#[derive(Debug, Default)]
pub struct ModelRegistry {
    /// Map of model name to its definition.
    pub models: IndexMap<String, ModelDef>,
    /// Map of original schema names to module names.
    pub name_map: HashMap<String, String>,
    /// Map of Rust type names to module names.
    pub type_map: HashMap<String, String>,
}

impl ModelRegistry {
    /// Return a model by name, if present.
    pub fn get(&self, name: &str) -> Option<&ModelDef> {
        if let Some(mapped) = self.name_map.get(name) {
            return self.models.get(mapped);
        }
        self.models
            .get(name)
            .or_else(|| self.models.get(&sanitize_module_name(name)))
    }

    /// Return all models in insertion order.
    pub fn all(&self) -> impl Iterator<Item = &ModelDef> {
        self.models.values()
    }
}

/// A top-level model definition, typically corresponding to a component schema.
#[derive(Debug, Clone, Serialize)]
pub struct ModelDef {
    /// The name of the model as derived from the component key.
    pub name: String,
    /// The Rust type name for this model.
    pub rust_name: String,
    /// The rendered Rust type for non-struct models.
    pub render_type: String,
    /// Other model types referenced by this model.
    pub deps: Vec<String>,
    /// Renderable imports grouped by module.
    pub dep_imports: Vec<DepImport>,
    /// The modeled shape.
    pub kind: ModelType,
}

/// Grouped imports for a module.
#[derive(Debug, Clone, Serialize)]
pub struct DepImport {
    pub module: String,
    pub types: Vec<String>,
}

/// The high-level shape of a model.
#[derive(Debug, Clone, Serialize)]
pub enum ModelType {
    /// A Rust struct with named fields.
    Struct(ModelStruct),
    /// A list of variants that can match multiple schema branches.
    Composite(ModelComposite),
    /// A reference to another named model.
    Ref(String),
    /// A list of items with a shared schema.
    Array(Box<ModelType>),
    /// A primitive scalar value.
    Primitive(PrimitiveType),
    /// A placeholder for unsupported/opaque schema shapes.
    Opaque,
}

/// The supported primitive scalar types.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum PrimitiveType {
    /// String-like data.
    String,
    /// Floating-point numbers.
    Number,
    /// Integer numbers.
    Integer,
    /// Boolean values.
    Boolean,
}

/// A Rust struct model with named fields.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStruct {
    /// Fields in declaration order.
    pub fields: Vec<ModelField>,
}

/// A struct field definition.
#[derive(Debug, Clone, Serialize)]
pub struct ModelField {
    /// The source schema field name.
    pub name: String,
    /// The Rust field identifier.
    pub rust_name: String,
    /// Whether the field is required by the schema.
    pub required: bool,
    /// Whether the field is nullable by the schema.
    pub nullable: bool,
    /// The rendered Rust type for this field.
    pub render_type: String,
    /// The field type.
    pub typ: ModelType,
    /// Whether the field should be flattened into the parent.
    pub flatten: bool,
}

/// A composite schema type like oneOf/anyOf/allOf.
#[derive(Debug, Clone, Serialize)]
pub struct ModelComposite {
    /// The composite variant type.
    pub flavor: CompositeFlavor,
    /// The candidate schemas for this composite.
    pub variants: Vec<ModelType>,
    /// The rendered Rust type for each variant.
    pub render_variants: Vec<String>,
}

/// The type of composite schema.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum CompositeFlavor {
    /// Exactly one schema must match.
    OneOf,
    /// Any schema may match.
    AnyOf,
    /// All schemas must match.
    AllOf,
}

/// Renderable model data for Tera templates.
#[derive(Debug, Clone, Serialize)]
pub struct RenderModel {
    /// The Rust type name.
    pub name: String,
    /// The module file name.
    pub module: String,
    /// The model kind (struct/composite/alias/opaque).
    pub kind: String,
    /// Renderable fields for struct models.
    pub fields: Vec<RenderField>,
    /// Optional alias type for non-struct models.
    pub alias: Option<String>,
}

impl RenderModel {
    /// Build renderable data from a model definition.
    pub fn from_model(model: &ModelDef, module: &str) -> Result<Self> {
        match &model.kind {
            ModelType::Struct(def) => Ok(Self {
                name: model.rust_name.clone(),
                module: module.to_string(),
                kind: "struct".into(),
                fields: def
                    .fields
                    .iter()
                    .map(|field| RenderField {
                        name: field.name.clone(),
                        rust_name: field.rust_name.clone(),
                        typ: model_type_to_rust(&field.typ),
                        required: field.required,
                        nullable: field.nullable,
                        flatten: field.flatten,
                    })
                    .collect(),
                alias: None,
            }),
            other => Ok(Self {
                name: model.rust_name.clone(),
                module: module.to_string(),
                kind: "alias".into(),
                fields: Vec::new(),
                alias: Some(model_type_to_rust(other)),
            }),
        }
    }
}

/// Renderable struct field data for Tera templates.
#[derive(Debug, Clone, Serialize)]
pub struct RenderField {
    /// The original field name.
    pub name: String,
    /// The sanitized Rust field identifier.
    pub rust_name: String,
    /// The Rust type for the field.
    pub typ: String,
    /// Whether the field is required by the schema.
    pub required: bool,
    /// Whether the field is nullable by the schema.
    pub nullable: bool,
    /// Whether the field should be flattened into the parent.
    pub flatten: bool,
}

/// Convert a model type into a Rust type string.
pub fn model_type_to_rust(typ: &ModelType) -> String {
    match typ {
        ModelType::Primitive(PrimitiveType::String) => "String".into(),
        ModelType::Primitive(PrimitiveType::Number) => "f64".into(),
        ModelType::Primitive(PrimitiveType::Integer) => "i64".into(),
        ModelType::Primitive(PrimitiveType::Boolean) => "bool".into(),
        ModelType::Array(inner) => format!("Vec<{}>", model_type_to_rust(inner)),
        ModelType::Ref(name) => sanitize_type_name(name),
        ModelType::Struct(_) => "serde_json::Value".into(),
        ModelType::Composite(_) => "serde_json::Value".into(),
        ModelType::Opaque => "serde_json::Value".into(),
    }
}

fn struct_signature(def: &ModelStruct) -> String {
    let mut parts = Vec::with_capacity(def.fields.len());
    for field in &def.fields {
        parts.push(format!(
            "{}:{}:{}:{}",
            field.name,
            field.required,
            field.nullable,
            type_signature(&field.typ)
        ));
    }
    format!("struct{{{}}}", parts.join(","))
}

fn type_signature(typ: &ModelType) -> String {
    match typ {
        ModelType::Primitive(PrimitiveType::String) => "prim:string".into(),
        ModelType::Primitive(PrimitiveType::Number) => "prim:number".into(),
        ModelType::Primitive(PrimitiveType::Integer) => "prim:integer".into(),
        ModelType::Primitive(PrimitiveType::Boolean) => "prim:boolean".into(),
        ModelType::Array(inner) => format!("array<{}>", type_signature(inner)),
        ModelType::Ref(name) => format!("ref:{name}"),
        ModelType::Struct(def) => struct_signature(def),
        ModelType::Composite(comp) => {
            let mut variants = Vec::with_capacity(comp.variants.len());
            for variant in &comp.variants {
                variants.push(type_signature(variant));
            }
            format!(
                "composite:{}({})",
                composite_flavor_name(comp.flavor),
                variants.join("|")
            )
        }
        ModelType::Opaque => "opaque".into(),
    }
}

fn dedupe_composite_variants(variants: &mut Vec<ModelType>) {
    let mut seen = HashSet::new();
    variants.retain(|variant| seen.insert(type_signature(variant)));
}

fn composite_flavor_name(flavor: CompositeFlavor) -> &'static str {
    match flavor {
        CompositeFlavor::OneOf => "oneOf",
        CompositeFlavor::AnyOf => "anyOf",
        CompositeFlavor::AllOf => "allOf",
    }
}

fn build_component_module_name_map<'a>(
    names: impl Iterator<Item = &'a String> + Clone,
) -> HashMap<String, String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for name in names.clone() {
        let base = sanitize_module_name(name);
        *counts.entry(base).or_insert(0) += 1;
    }

    let mut used: HashSet<String> = HashSet::new();
    let mut map: HashMap<String, String> = HashMap::new();

    for name in names {
        let base = sanitize_module_name(name);
        let resolved = if counts.get(&base).copied().unwrap_or(0) == 1 {
            base
        } else {
            sanitize_module_name_disambiguated(name)
        };
        let resolved = ensure_unique_name(resolved, &mut used, name);
        map.insert(name.clone(), resolved);
    }

    map
}

fn ensure_unique_name(name: String, used: &mut HashSet<String>, seed: &str) -> String {
    if !used.contains(&name) {
        used.insert(name.clone());
        return name;
    }

    let mut attempt = 0usize;
    loop {
        let salt = if attempt == 0 {
            seed.to_string()
        } else {
            format!("{seed}:{attempt}")
        };
        let suffix = alpha_hash(&salt);
        let candidate = format!("{name}_{suffix}");
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        attempt += 1;
    }
}

fn unique_name_with_hash(
    base: &str,
    signature: &str,
    existing: &mut indexmap::IndexSet<String>,
    name_to_sig: &HashMap<String, String>,
) -> String {
    let mut attempt = 0usize;
    loop {
        let salt = if attempt == 0 {
            signature.to_string()
        } else {
            format!("{signature}:{attempt}")
        };
        let suffix = alpha_hash(&salt);
        let candidate = format!("{base}_{suffix}");
        if let Some(existing_sig) = name_to_sig.get(&candidate) {
            if existing_sig == signature {
                return candidate;
            }
        }
        if !existing.contains(&candidate) {
            existing.insert(candidate.clone());
            return candidate;
        }
        attempt += 1;
    }
}

fn alpha_hash(input: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    let mut value = hasher.finish();
    let mut out = String::new();
    for _ in 0..6 {
        let idx = (value % 26) as u8;
        out.push((b'a' + idx) as char);
        value /= 26;
    }
    out
}

fn collect_model_deps(model: &ModelDef) -> Vec<String> {
    let mut deps = indexmap::IndexSet::new();
    collect_deps_from_type(&model.kind, &mut deps);
    deps.shift_remove(&model.rust_name);
    let mut out: Vec<String> = deps.into_iter().collect();
    out.sort();
    out
}

pub fn group_dep_imports(
    deps: &[String],
    type_map: &HashMap<String, String>,
) -> Vec<DepImport> {
    let mut grouped: IndexMap<String, Vec<String>> = IndexMap::new();
    for dep in deps {
        let module = type_map
            .get(dep)
            .cloned()
            .unwrap_or_else(|| sanitize_module_name(dep));
        grouped.entry(module).or_default().push(dep.clone());
    }

    let mut out: Vec<DepImport> = grouped
        .into_iter()
        .map(|(module, types)| DepImport { module, types })
        .collect();
    out.sort_by(|a, b| a.module.cmp(&b.module));
    out
}

fn collect_deps_from_type(typ: &ModelType, deps: &mut indexmap::IndexSet<String>) {
    match typ {
        ModelType::Ref(name) => {
            deps.insert(sanitize_type_name(name));
        }
        ModelType::Array(inner) => {
            if model_type_to_rust(inner) != "serde_json::Value" {
                collect_deps_from_type(inner, deps)
            }
        }
        ModelType::Struct(def) => {
            for field in &def.fields {
                if model_type_to_rust(&field.typ) != "serde_json::Value" {
                    collect_deps_from_type(&field.typ, deps);
                }
            }
        }
        ModelType::Composite(comp) => {
            for variant in &comp.variants {
                if model_type_to_rust(variant) != "serde_json::Value" {
                    collect_deps_from_type(variant, deps);
                }
            }
        }
        ModelType::Primitive(_) | ModelType::Opaque => {}
    }
}

/// Sanitize a schema name into a Rust type identifier.
pub fn sanitize_type_name(name: &str) -> String {
    let has_separator = name.chars().any(|ch| !ch.is_ascii_alphanumeric());
    let mut out = String::new();
    let mut upper_next = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper_next {
                out.push(ch.to_ascii_uppercase());
                upper_next = false;
            } else {
                if has_separator {
                    out.push(ch.to_ascii_lowercase());
                } else {
                    out.push(ch);
                }
            }
        } else {
            upper_next = true;
        }
    }
    if out.is_empty() {
        "Model".into()
    } else {
        out
    }
}

/// Sanitize a schema name into a Rust module file name.
pub fn sanitize_module_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    // Collapse repeated underscores and trim leading/trailing ones.
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "_".into()
    } else {
        out
    }
}

fn sanitize_module_name_disambiguated(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            continue;
        }

        if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
        out.push_str(match ch {
            '-' => "dash",
            '_' => "underscore",
            '.' => "dot",
            '/' => "slash",
            ':' => "colon",
            '+' => "plus",
            '@' => "at",
            ' ' => "space",
            _ => "sep",
        });
        out.push('_');
    }

    if out.ends_with('_') {
        out.pop();
    }

    if out.is_empty() {
        "_".into()
    } else {
        out
    }
}

/// Sanitize a schema property name into a Rust field identifier.
pub fn sanitize_field_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return "_".into();
    }
    let mut out = out;
    if out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, '_');
    }
    if is_rust_keyword(&out) {
        out.push('_');
    }
    out
}

fn is_rust_keyword(ident: &str) -> bool {
    matches!(
        ident,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
    )
}

/// Validate that the resolved OpenAPI document includes components.
///
/// This is a small helper that can be used by callers to short-circuit if
/// a spec does not contain schema components.
pub fn require_components(doc: &OpenAPI) -> Result<&openapiv3::Components> {
    doc.components
        .as_ref()
        .ok_or_else(|| anyhow!("openapi document missing components"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openapiv3::{Components, SchemaKind};

    fn make_doc() -> OpenAPI {
        OpenAPI {
            components: Some(Components::default()),
            ..OpenAPI::default()
        }
    }

    #[test]
    fn model_type_to_rust_primitives() {
        assert_eq!(
            model_type_to_rust(&ModelType::Primitive(PrimitiveType::String)),
            "String"
        );
        assert_eq!(
            model_type_to_rust(&ModelType::Primitive(PrimitiveType::Number)),
            "f64"
        );
        assert_eq!(
            model_type_to_rust(&ModelType::Primitive(PrimitiveType::Integer)),
            "i64"
        );
        assert_eq!(
            model_type_to_rust(&ModelType::Primitive(PrimitiveType::Boolean)),
            "bool"
        );
    }

    #[test]
    fn maps_object_schema_to_struct_model() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        let mut object = openapiv3::ObjectType::default();
        object.properties.insert(
            "name".into(),
            ReferenceOr::Item(Box::new(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            })),
        );
        object.required.push("name".into());
        components.schemas.insert(
            "User".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(object)),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let user = registry.get("User").expect("user model");
        let ModelType::Struct(def) = &user.kind else {
            panic!("expected struct model");
        };
        assert_eq!(def.fields.len(), 1);
        assert_eq!(def.fields[0].name, "name");
        assert_eq!(def.fields[0].rust_name, "name");
        assert!(def.fields[0].required);
        assert!(!def.fields[0].nullable);
        assert_eq!(def.fields[0].render_type, "String");
    }

    #[test]
    fn maps_array_schema_to_array_model() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "Tags".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Array(openapiv3::ArrayType {
                    items: Some(ReferenceOr::Item(Box::new(Schema {
                        schema_data: Default::default(),
                        schema_kind: SchemaKind::Type(Type::String(Default::default())),
                    }))),
                    min_items: None,
                    max_items: None,
                    unique_items: false,
                })),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let tags = registry.get("Tags").expect("tags model");
        let ModelType::Array(inner) = &tags.kind else {
            panic!("expected array model");
        };
        assert!(matches!(
            **inner,
            ModelType::Primitive(PrimitiveType::String)
        ));
        assert_eq!(tags.render_type, "Vec<String>");
    }

    #[test]
    fn hoists_inline_array_item_structs() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();

        let mut item_obj = openapiv3::ObjectType::default();
        item_obj.properties.insert(
            "name".into(),
            ReferenceOr::Item(Box::new(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            })),
        );

        let mut container = openapiv3::ObjectType::default();
        container.properties.insert(
            "items".into(),
            ReferenceOr::Item(Box::new(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Array(openapiv3::ArrayType {
                    items: Some(ReferenceOr::Item(Box::new(Schema {
                        schema_data: Default::default(),
                        schema_kind: SchemaKind::Type(Type::Object(item_obj)),
                    }))),
                    min_items: None,
                    max_items: None,
                    unique_items: false,
                })),
            })),
        );

        components.schemas.insert(
            "container".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(container)),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let container = registry.get("container").expect("container model");
        let ModelType::Struct(def) = &container.kind else {
            panic!("expected struct model");
        };
        let field = def.fields.iter().find(|f| f.name == "items").unwrap();
        let ModelType::Array(inner) = &field.typ else {
            panic!("expected array field");
        };
        let ModelType::Ref(name) = inner.as_ref() else {
            panic!("expected hoisted ref");
        };
        assert_eq!(name, "container_items_item");
        assert_eq!(field.render_type, "Vec<ContainerItemsItem>");
        assert!(registry.get(name).is_some());
    }

    #[test]
    fn maps_reference_schema_to_ref_model() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "UserId".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
        );
        components.schemas.insert(
            "User".into(),
            ReferenceOr::Reference {
                reference: "#/components/schemas/UserId".into(),
            },
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let user = registry.get("User").expect("user model");
        let ModelType::Ref(name) = &user.kind else {
            panic!("expected ref model");
        };
        assert_eq!(name, "UserId");
        assert_eq!(user.rust_name, "User");
        assert_eq!(user.render_type, "UserId");
    }

    #[test]
    fn sanitizes_type_names() {
        assert_eq!(sanitize_type_name("user_id"), "UserId");
        assert_eq!(sanitize_type_name("user-id"), "UserId");
        assert_eq!(sanitize_type_name("User ID"), "UserId");
    }

    #[test]
    fn sanitizes_module_names_on_models() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "aaa_api-response".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let model = registry.get("aaa_api-response").expect("model");
        assert_eq!(model.name, "aaa_api_response");
    }

    #[test]
    fn collects_model_deps() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "Widget".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
        );
        let mut container = openapiv3::ObjectType::default();
        container.properties.insert(
            "widget".into(),
            ReferenceOr::Reference {
                reference: "#/components/schemas/Widget".into(),
            },
        );
        components.schemas.insert(
            "Container".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(container)),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let container = registry.get("Container").expect("container");
        assert_eq!(container.deps, vec!["Widget"]);
    }

    #[test]
    fn sanitizes_field_names() {
        assert_eq!(sanitize_field_name("type"), "type_");
        assert_eq!(sanitize_field_name("user-id"), "user_id");
        assert_eq!(sanitize_field_name("123name"), "_123name");
    }

    #[test]
    fn test_import_naming_consistency() {
        // This test verifies that model dependencies are correctly named for imports
        // The key issue is that dependencies should be PascalCase for import statements
        // but module names should be snake_case
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        components.schemas.insert(
            "Widget".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
        );
        let mut container = openapiv3::ObjectType::default();
        container.properties.insert(
            "widget".into(),
            ReferenceOr::Reference {
                reference: "#/components/schemas/Widget".into(),
            },
        );
        components.schemas.insert(
            "Container".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(container)),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let container = registry.get("Container").expect("container");
        // This should pass - the dependency should be "Widget" (PascalCase) not "widget" (snake_case)
        // This test is checking that the dependency collection is working correctly
        // The actual issue is in operation dependencies, not model dependencies
        println!("Container deps: {:?}", container.deps);
        // The model key is now "widget" due to module name sanitization
        let widget_model = registry.models.get("widget").unwrap();
        println!("Widget model name: {:?}", widget_model.name);
        println!("Widget model rust_name: {:?}", widget_model.rust_name);
        assert_eq!(container.deps, vec!["Widget"]);
    }

    #[test]
    fn maps_one_of_schema_to_composite_model() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        let one_of = vec![
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Integer(Default::default())),
            }),
        ];
        components.schemas.insert(
            "Scalar".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::OneOf { one_of },
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let scalar = registry.get("Scalar").expect("scalar model");
        let ModelType::Composite(comp) = &scalar.kind else {
            panic!("expected composite model");
        };
        assert!(matches!(comp.flavor, CompositeFlavor::OneOf));
        assert_eq!(comp.variants.len(), 2);
        assert_eq!(comp.render_variants.len(), 2);
    }

    #[test]
    fn maps_all_of_schema_to_struct_with_flatten() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();

        components.schemas.insert(
            "Common".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(openapiv3::ObjectType {
                    properties: {
                        let mut props = indexmap::IndexMap::new();
                        props.insert(
                            "success".into(),
                            ReferenceOr::Item(Box::new(Schema {
                                schema_data: Default::default(),
                                schema_kind: SchemaKind::Type(Type::Boolean(Default::default())),
                            })),
                        );
                        props
                    },
                    required: vec!["success".into()],
                    ..Default::default()
                })),
            }),
        );

        components.schemas.insert(
            "ResultInfo".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
        );

        let inline = ReferenceOr::Item(Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(Type::Object(openapiv3::ObjectType {
                properties: {
                    let mut props = indexmap::IndexMap::new();
                    props.insert(
                        "result_info".into(),
                        ReferenceOr::Reference {
                            reference: "#/components/schemas/ResultInfo".into(),
                        },
                    );
                    props
                },
                ..Default::default()
            })),
        });

        components.schemas.insert(
            "Collection".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::AllOf {
                    all_of: vec![
                        ReferenceOr::Reference {
                            reference: "#/components/schemas/Common".into(),
                        },
                        inline,
                    ],
                },
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let collection = registry.get("Collection").expect("collection model");
        let ModelType::Struct(def) = &collection.kind else {
            panic!("expected struct model");
        };
        assert!(def.fields.iter().any(|field| field.name == "result_info"));
        let flatten = def
            .fields
            .iter()
            .find(|field| field.flatten)
            .expect("flatten field");
        assert!(matches!(flatten.typ, ModelType::Ref(_)));
    }

    #[test]
    fn maps_any_schema_all_of_to_struct() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();

        components.schemas.insert(
            "Common".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(openapiv3::ObjectType {
                    properties: {
                        let mut props = indexmap::IndexMap::new();
                        props.insert(
                            "success".into(),
                            ReferenceOr::Item(Box::new(Schema {
                                schema_data: Default::default(),
                                schema_kind: SchemaKind::Type(Type::Boolean(Default::default())),
                            })),
                        );
                        props
                    },
                    required: vec!["success".into()],
                    ..Default::default()
                })),
            }),
        );

        let inline = ReferenceOr::Item(Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(Type::Object(openapiv3::ObjectType {
                properties: {
                    let mut props = indexmap::IndexMap::new();
                    props.insert(
                        "note".into(),
                        ReferenceOr::Item(Box::new(Schema {
                            schema_data: Default::default(),
                            schema_kind: SchemaKind::Type(Type::String(Default::default())),
                        })),
                    );
                    props
                },
                ..Default::default()
            })),
        });

        let any = AnySchema {
            typ: Some("object".into()),
            all_of: vec![
                ReferenceOr::Reference {
                    reference: "#/components/schemas/Common".into(),
                },
                inline,
            ],
            ..Default::default()
        };

        components.schemas.insert(
            "Collection".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Any(any),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let collection = registry.get("Collection").expect("collection model");
        let ModelType::Struct(def) = &collection.kind else {
            panic!("expected struct model");
        };
        assert!(def.fields.iter().any(|field| field.name == "note"));
        assert!(def.fields.iter().any(|field| field.flatten));
    }

    #[test]
    fn captures_nullable_field_flag() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        let mut object = openapiv3::ObjectType::default();
        let mut schema = Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(Type::String(Default::default())),
        };
        schema.schema_data.nullable = true;
        object
            .properties
            .insert("note".into(), ReferenceOr::Item(Box::new(schema)));
        object.required.push("note".into());
        components.schemas.insert(
            "Note".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(object)),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let note = registry.get("Note").expect("note model");
        let ModelType::Struct(def) = &note.kind else {
            panic!("expected struct model");
        };
        assert_eq!(def.fields.len(), 1);
        assert!(def.fields[0].nullable);
    }

    #[test]
    fn required_nullable_fields_use_option_type() {
        let mut doc = make_doc();
        let components = doc.components.as_mut().unwrap();
        let mut object = openapiv3::ObjectType::default();
        let mut schema = Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(Type::String(Default::default())),
        };
        schema.schema_data.nullable = true;
        object
            .properties
            .insert("note".into(), ReferenceOr::Item(Box::new(schema)));
        object.required.push("note".into());
        components.schemas.insert(
            "Note".into(),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(object)),
            }),
        );

        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        let note = registry.get("Note").expect("note model");
        let ModelType::Struct(def) = &note.kind else {
            panic!("expected struct model");
        };
        let field = &def.fields[0];
        assert!(field.required);
        assert!(field.nullable);

        let rendered = RenderModel::from_model(note, "note").expect("render");
        let field = &rendered.fields[0];
        assert!(field.required);
        assert!(field.nullable);
    }
}
