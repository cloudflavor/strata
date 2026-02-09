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
    /// Build a generator with an empty module-name map.
    /// Use this when you want names derived only from schema keys.
    pub fn new() -> Self {
        Self {
            module_name_map: HashMap::new(),
        }
    }

    /// Build a generator with a precomputed schema-name map.
    /// Keeps module naming deterministic across runs.
    pub fn with_module_name_map(module_name_map: HashMap<String, String>) -> Self {
        Self { module_name_map }
    }

    /// Walk components.schemas and build a ModelRegistry.
    /// Hoists inline schemas and refreshes render types and deps.
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

    /// Convert one component schema entry into a ModelDef.
    /// Preserves refs and computes the Rust name and render type.
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

    /// Map an OpenAPI SchemaKind into a ModelType.
    /// Handles composites, allOf flattening, and opaque fallback.
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

    /// Resolve a schema name into a module file name.
    /// Uses the name map first, then falls back to sanitization.
    fn module_name_for_schema(&self, name: &str) -> String {
        self.module_name_map
            .get(name)
            .cloned()
            .unwrap_or_else(|| sanitize_module_name(name))
    }

    /// Iteratively hoist inline structs/composites into named models.
    /// Updates render types, type map, and dependency lists.
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

    /// Traverse a model and replace inline shapes with refs.
    /// Accumulates new ModelDef entries for hoisted inline types.
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

    /// Recursive helper that hoists inline structs/composites/arrays.
    /// Returns whether the input ModelType was rewritten.
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

    /// Derive a stable inline model name from parent and field names.
    /// Ensures uniqueness using signature-aware suffixing.
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

    /// Recompute render_type for each model and its fields.
    /// Also rebuilds composite variant render types after edits.
    fn refresh_render_types(&self, registry: &mut ModelRegistry) {
        let boxed_types: HashSet<String> = registry
            .models
            .iter()
            .filter_map(|(name, model)| {
                matches!(model.kind, ModelType::Struct(_) | ModelType::Composite(_))
                    .then(|| name.clone())
            })
            .collect();
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
                    comp.render_variants = render_composite_variants(&comp.variants, &boxed_types);
                }
                _ => {}
            }
        }
    }

    /// Rebuild the Rust-type to module mapping from registry models.
    /// Used later to group imports by module.
    fn refresh_type_map(&self, registry: &mut ModelRegistry) {
        registry.type_map = registry
            .models
            .values()
            .map(|model| (model.rust_name.clone(), model.name.clone()))
            .collect();
    }

    /// Recompute dependency lists and grouped imports for all models.
    /// Keeps model deps in sync after hoisting or renaming.
    fn refresh_deps(&self, registry: &mut ModelRegistry) {
        let type_map = registry.type_map.clone();
        for model in registry.models.values_mut() {
            model.deps = collect_model_deps(model);
            model.dep_imports = group_dep_imports(&model.deps, &type_map);
        }
    }

    /// Convert an OpenAPI Type into a ModelType.
    /// Object and array shapes are recursively mapped.
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

    /// Convert a ReferenceOr<Schema> into a ModelType.
    /// Refs are preserved as ModelType::Ref with mapped names.
    pub fn schema_ref_to_type(&self, schema: &ReferenceOr<Schema>) -> Result<ModelType> {
        match schema {
            ReferenceOr::Item(schema) => self.schema_kind_to_model_type(&schema.schema_kind),
            ReferenceOr::Reference { reference } => Ok(ModelType::Ref(
                self.module_name_for_schema(self.ref_name(reference).unwrap_or(reference)),
            )),
        }
    }

    /// Convert a ReferenceOr<Box<Schema>> into a ModelType.
    /// Used for object properties and array items.
    pub fn schema_ref_boxed_to_type(&self, schema: &ReferenceOr<Box<Schema>>) -> Result<ModelType> {
        match schema {
            ReferenceOr::Item(schema) => self.schema_kind_to_model_type(&schema.schema_kind),
            ReferenceOr::Reference { reference } => Ok(ModelType::Ref(
                self.module_name_for_schema(self.ref_name(reference).unwrap_or(reference)),
            )),
        }
    }

    /// Map a list of schema refs into ModelTypes in order.
    /// Preserves ordering for composites.
    pub fn refs_to_types(&self, refs: &[ReferenceOr<Schema>]) -> Result<Vec<ModelType>> {
        let mut out = Vec::with_capacity(refs.len());
        for schema in refs {
            out.push(self.schema_ref_to_type(schema)?);
        }
        Ok(out)
    }

    /// Interpret an AnySchema into a ModelType.
    /// Handles composites, typed objects/arrays, or opaque fallback.
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

    /// Flatten an allOf list into a single struct shape.
    /// Non-struct members become synthetic part_N fields.
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

    /// Convert object properties into ModelField entries.
    /// Applies required/nullable flags and name sanitization.
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

    /// Extract the schema name from a #/components/schemas ref.
    /// Returns None for non-component refs.
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
    /// Fetch a model by name, respecting the name map.
    /// Falls back to sanitized names when needed.
    pub fn get(&self, name: &str) -> Option<&ModelDef> {
        if let Some(mapped) = self.name_map.get(name) {
            return self.models.get(mapped);
        }
        self.models
            .get(name)
            .or_else(|| self.models.get(&sanitize_module_name(name)))
    }

    /// Iterate all models in insertion order.
    /// Useful for deterministic output generation.
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
    /// Build a template-ready RenderModel from a ModelDef.
    /// Expands fields for structs and aliases otherwise.
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

/// Render a ModelType as a Rust type string.
/// Used for fields, aliases, and composite variants.
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

/// Produce a stable signature string for struct shapes.
/// Used to dedupe and hoist inline structs.
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

/// Produce a stable signature string for any ModelType.
/// Used to dedupe and hoist inline/composite types.
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

/// Remove duplicate composite variants by signature.
/// Preserves the first occurrence ordering.
fn dedupe_composite_variants(variants: &mut Vec<ModelType>) {
    let mut seen = HashSet::new();
    variants.retain(|variant| seen.insert(type_signature(variant)));
}

/// Render all composite variants to Rust type strings.
/// Applies boxing rules for recursive refs.
fn render_composite_variants(
    variants: &[ModelType],
    boxed_types: &HashSet<String>,
) -> Vec<String> {
    variants
        .iter()
        .map(|variant| render_composite_variant(variant, boxed_types))
        .collect()
}

/// Render a single composite variant to a Rust type string.
/// Boxes refs that point at struct/composite models.
fn render_composite_variant(variant: &ModelType, boxed_types: &HashSet<String>) -> String {
    match variant {
        ModelType::Ref(name) => {
            let ty = sanitize_type_name(name);
            let boxed = boxed_types.contains(name);
            if boxed {
                format!("Box<{ty}>")
            } else {
                ty
            }
        }
        ModelType::Opaque => "SerdeJsonValue".into(),
        _ => model_type_to_rust(variant),
    }
}

/// Return the OpenAPI flavor name for a composite.
/// Used in signatures and debug output.
fn composite_flavor_name(flavor: CompositeFlavor) -> &'static str {
    match flavor {
        CompositeFlavor::OneOf => "oneOf",
        CompositeFlavor::AnyOf => "anyOf",
        CompositeFlavor::AllOf => "allOf",
    }
}

/// Build a unique module-name map for all schema keys.
/// Disambiguates collisions from sanitization.
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

/// Ensure a module name is unique in the used set.
/// Adds a stable suffix when collisions occur.
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

/// Generate a deterministic unique name from a signature.
/// Keeps existing matches stable across runs.
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

/// Hash helper that yields a short alphabetic suffix.
/// Used for stable name disambiguation.
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

/// Collect referenced model dependencies for a ModelDef.
/// Sorted for stable output.
fn collect_model_deps(model: &ModelDef) -> Vec<String> {
    let mut deps = indexmap::IndexSet::new();
    collect_deps_from_type(&model.kind, &mut deps);
    deps.shift_remove(&model.rust_name);
    let mut out: Vec<String> = deps.into_iter().collect();
    out.sort();
    out
}

/// Group dependency types by module for imports.
/// Keeps imports compact and deterministic.
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

/// Recursively gather dependencies from a ModelType.
/// Skips primitives and opaque shapes.
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
/// Produces PascalCase without separators.
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
/// Lowercases, replaces separators, and trims underscores.
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

/// Sanitize a schema name while encoding separator types.
/// Used when raw sanitization would collide.
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

/// Sanitize a property name into a Rust field identifier.
/// Handles keywords and leading digits.
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

/// Check whether an identifier is a Rust keyword.
/// Used to avoid generating invalid field names.
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

/// Return the components section or an error if missing.
/// Lets callers fail fast on incomplete specs.
pub fn require_components(doc: &OpenAPI) -> Result<&openapiv3::Components> {
    doc.components
        .as_ref()
        .ok_or_else(|| anyhow!("openapi document missing components"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexSet;
    use openapiv3::{AnySchema, Components, ReferenceOr, Schema, SchemaKind, Type};
    use std::collections::HashSet as StdHashSet;

    /// Create a minimal OpenAPI document with empty components.
    /// Shared helper for model generator tests.
    fn make_doc() -> OpenAPI {
        OpenAPI {
            components: Some(Components::default()),
            ..OpenAPI::default()
        }
    }

    /// Verify primitive ModelTypes map to correct Rust types.
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

    /// Ensure object schemas become struct models with fields.
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

    /// Ensure array schemas become Vec<T> models.
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

    /// Ensure inline array item structs are hoisted into models.
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

    /// Ensure refs produce ModelType::Ref with correct names.
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
        assert_eq!(name, "userid");
        assert_eq!(user.rust_name, "User");
        assert_eq!(user.render_type, "Userid");
    }

    /// Ensure type name sanitization is consistent.
    #[test]
    fn sanitizes_type_names() {
        assert_eq!(sanitize_type_name("user_id"), "UserId");
        assert_eq!(sanitize_type_name("user-id"), "UserId");
        assert_eq!(sanitize_type_name("User ID"), "UserId");
    }

    /// Ensure module name sanitization is applied to models.
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

    /// Ensure dependency collection finds referenced models.
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

    /// Ensure field name sanitization handles keywords and digits.
    #[test]
    fn sanitizes_field_names() {
        assert_eq!(sanitize_field_name("type"), "type_");
        assert_eq!(sanitize_field_name("user-id"), "user_id");
        assert_eq!(sanitize_field_name("123name"), "_123name");
    }

    /// Ensure dependency naming stays in PascalCase for imports.
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

    /// Ensure oneOf schemas become composite models.
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

    /// Ensure allOf schemas flatten into struct fields.
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

    /// Ensure AnySchema with allOf produces a struct.
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

    /// Ensure nullable schema fields set the nullable flag.
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

    /// Ensure nullable+required fields are still wrapped as Option.
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

    /// Ensure the generator starts with an empty name map.
    #[test]
    fn new_starts_with_empty_map() {
        let generator = ModelGenerator::new();
        assert!(generator.module_name_map.is_empty());
    }

    /// Ensure the provided module name map is retained.
    #[test]
    fn with_module_name_map_sets_map() {
        let mut map = HashMap::new();
        map.insert("Widget".into(), "widget".into());
        let generator = ModelGenerator::with_module_name_map(map.clone());
        assert_eq!(generator.module_name_map, map);
    }

    /// Ensure collect_models returns an empty registry when components are missing.
    #[test]
    fn collect_models_returns_empty_without_components() {
        let doc = OpenAPI::default();
        let mut generator = ModelGenerator::new();
        let registry = generator.collect_models(&doc).expect("collect");
        assert!(registry.models.is_empty());
    }

    /// Ensure schema_to_model keeps references as ModelType::Ref.
    #[test]
    fn schema_to_model_preserves_reference() {
        let mut map = HashMap::new();
        map.insert("Widget".into(), "widget".into());
        let generator = ModelGenerator::with_module_name_map(map);
        let schema = ReferenceOr::Reference {
            reference: "#/components/schemas/Widget".into(),
        };
        let def = generator
            .schema_to_model("Alias", &schema)
            .expect("schema_to_model");
        let ModelType::Ref(name) = def.kind else {
            panic!("expected ref model");
        };
        assert_eq!(name, "widget");
    }

    /// Ensure empty oneOf collapses to an opaque model type.
    #[test]
    fn schema_kind_to_model_type_empty_one_of_is_opaque() {
        let generator = ModelGenerator::new();
        let one_of = vec![ReferenceOr::Item(Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Any(AnySchema::default()),
        })];
        let kind = SchemaKind::OneOf { one_of };
        let typ = generator.schema_kind_to_model_type(&kind).expect("kind");
        assert!(matches!(typ, ModelType::Opaque));
    }

    /// Ensure module_name_for_schema honors the map and falls back to sanitization.
    #[test]
    fn module_name_for_schema_uses_map_and_sanitizes() {
        let mut map = HashMap::new();
        map.insert("Widget".into(), "widget_mod".into());
        let generator = ModelGenerator::with_module_name_map(map);
        assert_eq!(generator.module_name_for_schema("Widget"), "widget_mod");
        assert_eq!(generator.module_name_for_schema("My-Model"), "my_model");
    }

    /// Ensure hoist_inline_models hoists inline structs into named models.
    #[test]
    fn hoist_inline_models_creates_inline_refs() {
        let generator = ModelGenerator::new();
        let mut registry = ModelRegistry::default();
        let inline_field = ModelField {
            name: "inner".into(),
            rust_name: "inner".into(),
            required: true,
            nullable: false,
            render_type: String::new(),
            typ: ModelType::Struct(ModelStruct { fields: Vec::new() }),
            flatten: false,
        };
        registry.models.insert(
            "Container".into(),
            ModelDef {
                name: "Container".into(),
                rust_name: "Container".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Struct(ModelStruct {
                    fields: vec![inline_field],
                }),
            },
        );

        generator
            .hoist_inline_models(&mut registry)
            .expect("hoist");
        let container = registry.get("Container").expect("container");
        let ModelType::Struct(def) = &container.kind else {
            panic!("expected struct model");
        };
        let field = &def.fields[0];
        let ModelType::Ref(name) = &field.typ else {
            panic!("expected ref field");
        };
        assert_eq!(name, "container_inner");
        assert!(registry.get("container_inner").is_some());
    }

    /// Ensure hoist_in_type converts inline structs into refs.
    #[test]
    fn hoist_in_type_hoists_inline_struct() {
        let generator = ModelGenerator::new();
        let mut typ = ModelType::Struct(ModelStruct { fields: Vec::new() });
        let mut additions = Vec::new();
        let mut existing = IndexSet::new();
        let mut name_to_sig = HashMap::new();
        let mut sig_to_name = HashMap::new();

        let changed = generator
            .hoist_in_type(
                "Container",
                Some("inner"),
                &mut typ,
                &mut additions,
                &mut existing,
                &mut name_to_sig,
                &mut sig_to_name,
            )
            .expect("hoist");
        assert!(changed);
        let ModelType::Ref(name) = typ else {
            panic!("expected ref");
        };
        assert_eq!(name, "container_inner");
        assert_eq!(additions.len(), 1);
    }

    /// Ensure inline_model_name reuses a name when signatures match.
    #[test]
    fn inline_model_name_reuses_signature() {
        let generator = ModelGenerator::new();
        let mut existing = IndexSet::new();
        existing.insert("container_inner".into());
        let mut name_to_sig = HashMap::new();
        name_to_sig.insert("container_inner".into(), "sig".into());
        let name = generator.inline_model_name(
            "Container",
            Some("inner"),
            false,
            "sig",
            &mut existing,
            &name_to_sig,
        );
        assert_eq!(name, "container_inner");
    }

    /// Ensure refresh_render_types updates composite render variants.
    #[test]
    fn refresh_render_types_updates_composite_variants() {
        let mut registry = ModelRegistry::default();
        registry.models.insert(
            "Node".into(),
            ModelDef {
                name: "Node".into(),
                rust_name: "Node".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Struct(ModelStruct { fields: Vec::new() }),
            },
        );
        registry.models.insert(
            "NodeUnion".into(),
            ModelDef {
                name: "NodeUnion".into(),
                rust_name: "NodeUnion".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Composite(ModelComposite {
                    flavor: CompositeFlavor::OneOf,
                    variants: vec![ModelType::Ref("Node".into())],
                    render_variants: Vec::new(),
                }),
            },
        );

        let generator = ModelGenerator::new();
        generator.refresh_render_types(&mut registry);
        let union = registry.get("NodeUnion").expect("union");
        let ModelType::Composite(comp) = &union.kind else {
            panic!("expected composite");
        };
        assert_eq!(comp.render_variants, vec!["Box<Node>"]);
    }

    /// Ensure refresh_type_map builds the Rust type map.
    #[test]
    fn refresh_type_map_rebuilds() {
        let mut registry = ModelRegistry::default();
        registry.models.insert(
            "widget".into(),
            ModelDef {
                name: "widget".into(),
                rust_name: "Widget".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Opaque,
            },
        );
        let generator = ModelGenerator::new();
        generator.refresh_type_map(&mut registry);
        assert_eq!(registry.type_map.get("Widget"), Some(&"widget".to_string()));
    }

    /// Ensure refresh_deps recomputes model dependencies.
    #[test]
    fn refresh_deps_rebuilds() {
        let mut registry = ModelRegistry::default();
        registry.models.insert(
            "Widget".into(),
            ModelDef {
                name: "Widget".into(),
                rust_name: "Widget".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Opaque,
            },
        );
        registry.models.insert(
            "Container".into(),
            ModelDef {
                name: "Container".into(),
                rust_name: "Container".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Struct(ModelStruct {
                    fields: vec![ModelField {
                        name: "widget".into(),
                        rust_name: "widget".into(),
                        required: true,
                        nullable: false,
                        render_type: String::new(),
                        typ: ModelType::Ref("Widget".into()),
                        flatten: false,
                    }],
                }),
            },
        );
        let generator = ModelGenerator::new();
        generator.refresh_deps(&mut registry);
        let container = registry.get("Container").expect("container");
        assert_eq!(container.deps, vec!["Widget"]);
    }

    /// Ensure type_to_model_type maps arrays and objects correctly.
    #[test]
    fn type_to_model_type_maps_array_and_object() {
        let generator = ModelGenerator::new();
        let array = Type::Array(openapiv3::ArrayType {
            items: Some(ReferenceOr::Item(Box::new(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }))),
            min_items: None,
            max_items: None,
            unique_items: false,
        });
        let array_type = generator.type_to_model_type(&array).expect("array");
        assert!(matches!(array_type, ModelType::Array(_)));

        let object = Type::Object(openapiv3::ObjectType {
            properties: IndexMap::new(),
            required: Vec::new(),
            ..Default::default()
        });
        let object_type = generator.type_to_model_type(&object).expect("object");
        assert!(matches!(object_type, ModelType::Struct(_)));
    }

    /// Ensure schema_ref_to_type maps inline schemas to ModelType.
    #[test]
    fn schema_ref_to_type_handles_item() {
        let generator = ModelGenerator::new();
        let schema = ReferenceOr::Item(Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(Type::Boolean(Default::default())),
        });
        let typ = generator.schema_ref_to_type(&schema).expect("schema_ref");
        assert!(matches!(typ, ModelType::Primitive(PrimitiveType::Boolean)));
    }

    /// Ensure schema_ref_boxed_to_type maps references to ModelType::Ref.
    #[test]
    fn schema_ref_boxed_to_type_handles_ref() {
        let mut map = HashMap::new();
        map.insert("Widget".into(), "widget".into());
        let generator = ModelGenerator::with_module_name_map(map);
        let schema = ReferenceOr::Reference {
            reference: "#/components/schemas/Widget".into(),
        };
        let typ = generator
            .schema_ref_boxed_to_type(&schema)
            .expect("schema_ref_boxed");
        let ModelType::Ref(name) = typ else {
            panic!("expected ref");
        };
        assert_eq!(name, "widget");
    }

    /// Ensure refs_to_types preserves the original ordering.
    #[test]
    fn refs_to_types_keeps_order() {
        let generator = ModelGenerator::new();
        let refs = vec![
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Integer(Default::default())),
            }),
        ];
        let types = generator.refs_to_types(&refs).expect("refs");
        assert!(matches!(types[0], ModelType::Primitive(PrimitiveType::String)));
        assert!(matches!(types[1], ModelType::Primitive(PrimitiveType::Integer)));
    }

    /// Ensure any_schema_to_model_type handles oneOf composites.
    #[test]
    fn any_schema_to_model_type_prefers_one_of() {
        let generator = ModelGenerator::new();
        let any = AnySchema {
            one_of: vec![
                ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: SchemaKind::Type(Type::String(Default::default())),
                }),
                ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: SchemaKind::Type(Type::Integer(Default::default())),
                }),
            ],
            ..Default::default()
        };
        let typ = generator.any_schema_to_model_type(&any).expect("any");
        assert!(matches!(typ, ModelType::Composite(_)));
    }

    /// Ensure all_of_to_struct creates part_N fields for non-struct members.
    #[test]
    fn all_of_to_struct_adds_part_fields() {
        let generator = ModelGenerator::new();
        let all_of = vec![
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::String(Default::default())),
            }),
            ReferenceOr::Item(Schema {
                schema_data: Default::default(),
                schema_kind: SchemaKind::Type(Type::Object(openapiv3::ObjectType {
                    properties: {
                        let mut props = IndexMap::new();
                        props.insert(
                            "name".into(),
                            ReferenceOr::Item(Box::new(Schema {
                                schema_data: Default::default(),
                                schema_kind: SchemaKind::Type(Type::String(Default::default())),
                            })),
                        );
                        props
                    },
                    required: vec!["name".into()],
                    ..Default::default()
                })),
            }),
        ];
        let typ = generator.all_of_to_struct(&all_of, None).expect("all_of");
        let ModelType::Struct(def) = typ else {
            panic!("expected struct");
        };
        assert!(def.fields.iter().any(|f| f.name == "part_1"));
        assert!(def.fields.iter().any(|f| f.name == "name"));
    }

    /// Ensure object_properties_to_fields honors required and nullable flags.
    #[test]
    fn object_properties_to_fields_applies_required_and_nullable() {
        let generator = ModelGenerator::new();
        let mut props = IndexMap::new();
        let mut schema = Schema {
            schema_data: Default::default(),
            schema_kind: SchemaKind::Type(Type::String(Default::default())),
        };
        schema.schema_data.nullable = true;
        props.insert("note".into(), ReferenceOr::Item(Box::new(schema)));
        let fields = generator
            .object_properties_to_fields(&props, &["note".into()])
            .expect("fields");
        assert_eq!(fields.len(), 1);
        assert!(fields[0].required);
        assert!(fields[0].nullable);
    }

    /// Ensure ref_name parses component schema references.
    #[test]
    fn ref_name_parses_components_path() {
        let generator = ModelGenerator::new();
        assert_eq!(
            generator.ref_name("#/components/schemas/Widget"),
            Some("Widget")
        );
        assert_eq!(generator.ref_name("#/other/path"), None);
    }

    /// Ensure ModelRegistry::get respects name_map entries.
    #[test]
    fn model_registry_get_uses_name_map() {
        let mut registry = ModelRegistry::default();
        registry.name_map.insert("Widget".into(), "widget".into());
        registry.models.insert(
            "widget".into(),
            ModelDef {
                name: "widget".into(),
                rust_name: "Widget".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Opaque,
            },
        );
        assert!(registry.get("Widget").is_some());
    }

    /// Ensure ModelRegistry::all yields models in insertion order.
    #[test]
    fn model_registry_all_preserves_order() {
        let mut registry = ModelRegistry::default();
        registry.models.insert(
            "first".into(),
            ModelDef {
                name: "first".into(),
                rust_name: "First".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Opaque,
            },
        );
        registry.models.insert(
            "second".into(),
            ModelDef {
                name: "second".into(),
                rust_name: "Second".into(),
                render_type: String::new(),
                deps: Vec::new(),
                dep_imports: Vec::new(),
                kind: ModelType::Opaque,
            },
        );
        let names: Vec<String> = registry.all().map(|model| model.name.clone()).collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    /// Ensure RenderModel::from_model handles structs and aliases.
    #[test]
    fn render_model_from_model_struct_and_alias() {
        let model = ModelDef {
            name: "Widget".into(),
            rust_name: "Widget".into(),
            render_type: String::new(),
            deps: Vec::new(),
            dep_imports: Vec::new(),
            kind: ModelType::Struct(ModelStruct { fields: Vec::new() }),
        };
        let render = RenderModel::from_model(&model, "widget").expect("render");
        assert_eq!(render.kind, "struct");

        let alias = ModelDef {
            name: "Alias".into(),
            rust_name: "Alias".into(),
            render_type: String::new(),
            deps: Vec::new(),
            dep_imports: Vec::new(),
            kind: ModelType::Primitive(PrimitiveType::String),
        };
        let render = RenderModel::from_model(&alias, "alias").expect("render");
        assert_eq!(render.kind, "alias");
    }

    /// Ensure model_type_to_rust renders refs and arrays.
    #[test]
    fn model_type_to_rust_handles_ref_and_array() {
        assert_eq!(
            model_type_to_rust(&ModelType::Ref("Widget".into())),
            "Widget"
        );
        let array = ModelType::Array(Box::new(ModelType::Primitive(PrimitiveType::Boolean)));
        assert_eq!(model_type_to_rust(&array), "Vec<bool>");
    }

    /// Ensure struct_signature is stable for identical structs.
    #[test]
    fn struct_signature_is_stable() {
        let def = ModelStruct {
            fields: vec![ModelField {
                name: "name".into(),
                rust_name: "name".into(),
                required: true,
                nullable: false,
                render_type: "String".into(),
                typ: ModelType::Primitive(PrimitiveType::String),
                flatten: false,
            }],
        };
        assert_eq!(struct_signature(&def), struct_signature(&def));
    }

    /// Ensure type_signature encodes composite flavor and variants.
    #[test]
    fn type_signature_for_composite_includes_flavor() {
        let comp = ModelComposite {
            flavor: CompositeFlavor::OneOf,
            variants: vec![
                ModelType::Primitive(PrimitiveType::String),
                ModelType::Primitive(PrimitiveType::Integer),
            ],
            render_variants: Vec::new(),
        };
        let sig = type_signature(&ModelType::Composite(comp));
        assert!(sig.contains("oneOf"));
        assert!(sig.contains("prim:string"));
        assert!(sig.contains("prim:integer"));
    }

    /// Ensure duplicate composite variants are removed.
    #[test]
    fn dedupe_composite_variants_removes_duplicates() {
        let mut variants = vec![
            ModelType::Primitive(PrimitiveType::String),
            ModelType::Primitive(PrimitiveType::String),
            ModelType::Primitive(PrimitiveType::Boolean),
        ];
        dedupe_composite_variants(&mut variants);
        assert_eq!(variants.len(), 2);
    }

    /// Ensure render_composite_variant boxes struct/composite refs.
    #[test]
    fn render_composite_variant_boxes_struct_refs() {
        let mut boxed = StdHashSet::new();
        boxed.insert("Node".into());
        let rendered = render_composite_variant(&ModelType::Ref("Node".into()), &boxed);
        assert_eq!(rendered, "Box<Node>");
    }

    /// Ensure render_composite_variant maps opaque types to SerdeJsonValue.
    #[test]
    fn render_composite_variant_returns_serde_value_for_opaque() {
        let boxed = StdHashSet::new();
        let rendered = render_composite_variant(&ModelType::Opaque, &boxed);
        assert_eq!(rendered, "SerdeJsonValue");
    }

    /// Ensure render_composite_variants applies boxing across variants.
    #[test]
    fn render_composite_variants_uses_boxing_rules() {
        let mut boxed = StdHashSet::new();
        boxed.insert("Node".into());
        let variants = vec![
            ModelType::Ref("Node".into()),
            ModelType::Primitive(PrimitiveType::Boolean),
        ];
        let rendered = render_composite_variants(&variants, &boxed);
        assert_eq!(rendered, vec!["Box<Node>", "bool"]);
    }

    /// Ensure composite_flavor_name maps enum variants to strings.
    #[test]
    fn composite_flavor_name_maps_values() {
        assert_eq!(composite_flavor_name(CompositeFlavor::OneOf), "oneOf");
        assert_eq!(composite_flavor_name(CompositeFlavor::AnyOf), "anyOf");
        assert_eq!(composite_flavor_name(CompositeFlavor::AllOf), "allOf");
    }

    /// Ensure build_component_module_name_map avoids name collisions.
    #[test]
    fn build_component_module_name_map_disambiguates() {
        let names = vec!["foo-bar".to_string(), "foo_bar".to_string()];
        let map = build_component_module_name_map(names.iter());
        let first = map.get("foo-bar").unwrap();
        let second = map.get("foo_bar").unwrap();
        assert_ne!(first, second);
    }

    /// Ensure ensure_unique_name appends a suffix on collision.
    #[test]
    fn ensure_unique_name_appends_suffix() {
        let mut used = StdHashSet::new();
        used.insert("name".into());
        let unique = ensure_unique_name("name".into(), &mut used, "seed");
        assert_ne!(unique, "name");
        assert!(used.contains(&unique));
    }

    /// Ensure unique_name_with_hash reuses a name for matching signatures.
    #[test]
    fn unique_name_with_hash_reuses_signature() {
        let mut used = IndexSet::new();
        let mut name_to_sig = HashMap::new();
        let signature = "sig";
        let candidate = format!("base_{}", alpha_hash(signature));
        used.insert(candidate.clone());
        name_to_sig.insert(candidate.clone(), signature.to_string());
        let result = unique_name_with_hash("base", signature, &mut used, &name_to_sig);
        assert_eq!(result, candidate);
    }

    /// Ensure alpha_hash returns six lowercase letters.
    #[test]
    fn alpha_hash_is_six_lowercase_letters() {
        let hash = alpha_hash("seed");
        assert_eq!(hash.len(), 6);
        assert!(hash.chars().all(|c| c.is_ascii_lowercase()));
    }

    /// Ensure collect_model_deps extracts referenced model names.
    #[test]
    fn collect_model_deps_collects_refs() {
        let model = ModelDef {
            name: "Container".into(),
            rust_name: "Container".into(),
            render_type: String::new(),
            deps: Vec::new(),
            dep_imports: Vec::new(),
            kind: ModelType::Struct(ModelStruct {
                fields: vec![ModelField {
                    name: "widget".into(),
                    rust_name: "widget".into(),
                    required: true,
                    nullable: false,
                    render_type: String::new(),
                    typ: ModelType::Ref("Widget".into()),
                    flatten: false,
                }],
            }),
        };
        let deps = collect_model_deps(&model);
        assert_eq!(deps, vec!["Widget"]);
    }

    /// Ensure group_dep_imports groups deps by module name.
    #[test]
    fn group_dep_imports_groups_by_module() {
        let deps = vec!["Alpha".into(), "Beta".into()];
        let mut type_map = HashMap::new();
        type_map.insert("Alpha".into(), "group".into());
        type_map.insert("Beta".into(), "group".into());
        let grouped = group_dep_imports(&deps, &type_map);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].module, "group");
        assert_eq!(grouped[0].types, vec!["Alpha", "Beta"]);
    }

    /// Ensure collect_deps_from_type skips primitives and collects refs.
    #[test]
    fn collect_deps_from_type_skips_primitives() {
        let mut deps = indexmap::IndexSet::new();
        collect_deps_from_type(&ModelType::Primitive(PrimitiveType::String), &mut deps);
        collect_deps_from_type(&ModelType::Ref("Widget".into()), &mut deps);
        assert_eq!(deps.into_iter().collect::<Vec<_>>(), vec!["Widget"]);
    }

    /// Ensure sanitize_module_name trims and collapses underscores.
    #[test]
    fn sanitize_module_name_trims_and_collapses() {
        assert_eq!(sanitize_module_name("__A--"), "a");
    }

    /// Ensure sanitize_module_name_disambiguated encodes separators.
    #[test]
    fn sanitize_module_name_disambiguated_encodes_separators() {
        assert_eq!(sanitize_module_name_disambiguated("a-b"), "a_dash_b");
        assert_eq!(
            sanitize_module_name_disambiguated("a_b"),
            "a_underscore_b"
        );
    }

    /// Ensure is_rust_keyword detects known Rust keywords.
    #[test]
    fn is_rust_keyword_detects_keywords() {
        assert!(is_rust_keyword("type"));
        assert!(!is_rust_keyword("widget"));
    }

    /// Ensure require_components returns an error when components are missing.
    #[test]
    fn require_components_errors_without_components() {
        let doc = OpenAPI::default();
        assert!(require_components(&doc).is_err());
    }
}
