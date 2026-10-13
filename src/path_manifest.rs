use recipe::{ingredient::Ingredient, ingredient_value::IngredientValue};

use crate::{
    dependent::{DependentField, DependentFieldKind, DependentRecipe, SchemaCase},
    program::{RecipeProgram, SequenceRecipe},
};

/// A path manifest is the handoff contract for pathgen.
///
/// `recipe_writer` owns parsing semantics. Pathgen should only need this stable
/// list of paths, value kinds, repetition markers, and producing schema names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathManifest {
    pub namespace: String,
    pub program_name: String,
    pub paths: Vec<PathSpec>,
}

impl PathManifest {
    pub fn new(namespace: impl Into<String>, program_name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            program_name: program_name.into(),
            paths: Vec::new(),
        }
    }

    pub fn from_program(namespace: impl Into<String>, program: &RecipeProgram) -> Self {
        let namespace = namespace.into();
        let mut manifest = Self::new(namespace.clone(), program.name().to_string());
        let root = format!("{}.{}", normalize_segment(&namespace), normalize_segment(program.name()));
        collect_program_paths(program, &root, Cardinality::Single, &mut manifest);
        manifest
    }

    pub fn add(&mut self, spec: PathSpec) {
        if !self.paths.iter().any(|existing| existing.path == spec.path) {
            self.paths.push(spec);
        }
    }

    pub fn contains_path(&self, path: &str) -> bool {
        self.paths.iter().any(|spec| spec.path == path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSpec {
    pub path: String,
    pub value_kind: ValueKind,
    pub cardinality: Cardinality,
    pub produced_by_schema: String,
}

impl PathSpec {
    pub fn new(
        path: impl Into<String>,
        value_kind: ValueKind,
        cardinality: Cardinality,
        produced_by_schema: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            value_kind,
            cardinality,
            produced_by_schema: produced_by_schema.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    Single,
    Repeated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    U8,
    U16,
    U32,
    U64,
    Binary,
    String,
    Struct,
    Array,
    Unknown,
}

fn collect_program_paths(
    program: &RecipeProgram,
    root: &str,
    cardinality: Cardinality,
    manifest: &mut PathManifest,
) {
    match program {
        RecipeProgram::Static(ingredient) => {
            collect_ingredient_paths(ingredient, root, cardinality, manifest);
        }
        RecipeProgram::Dependent(recipe) => {
            collect_dependent_paths(recipe, root, cardinality, manifest);
        }
        RecipeProgram::Sequence(sequence) => {
            collect_sequence_paths(sequence, root, manifest);
        }
    }
}

fn collect_sequence_paths(sequence: &SequenceRecipe, root: &str, manifest: &mut PathManifest) {
    let item_root = format!("{}.{}[*]", root, normalize_segment(&sequence.item_field_name));
    manifest.add(PathSpec::new(
        item_root.clone(),
        ValueKind::Array,
        Cardinality::Repeated,
        sequence.name.clone(),
    ));
    collect_program_paths(&sequence.item, &item_root, Cardinality::Repeated, manifest);
}

fn collect_dependent_paths(
    recipe: &DependentRecipe,
    root: &str,
    cardinality: Cardinality,
    manifest: &mut PathManifest,
) {
    for field in &recipe.fields {
        collect_dependent_field_paths(field, root, cardinality, &recipe.name, manifest);
    }
}

fn collect_dependent_field_paths(
    field: &DependentField,
    parent: &str,
    cardinality: Cardinality,
    schema_name: &str,
    manifest: &mut PathManifest,
) {
    let path = format!("{}.{}", parent, normalize_segment(&field.name));
    match &field.kind {
        DependentFieldKind::QuicVarInt => {
            manifest.add(PathSpec::new(path, ValueKind::U64, cardinality, schema_name));
        }
        DependentFieldKind::Bytes { .. } => {
            manifest.add(PathSpec::new(path, ValueKind::Binary, cardinality, schema_name));
        }
        DependentFieldKind::RepeatedGroup { fields } => {
            manifest.add(PathSpec::new(
                path.clone(),
                ValueKind::Array,
                Cardinality::Repeated,
                schema_name,
            ));
            let item_root = format!("{}[*]", path);
            for child in fields {
                collect_dependent_field_paths(
                    child,
                    &item_root,
                    Cardinality::Repeated,
                    schema_name,
                    manifest,
                );
            }
        }
        DependentFieldKind::Ingredient { ingredient } => {
            collect_ingredient_paths_at(ingredient, &path, cardinality, manifest);
        }
        DependentFieldKind::Scope { fields, .. } => {
            manifest.add(PathSpec::new(
                path.clone(),
                ValueKind::Struct,
                cardinality,
                schema_name,
            ));
            for child in fields {
                collect_dependent_field_paths(child, &path, cardinality, schema_name, manifest);
            }
        }
        DependentFieldKind::Switch {
            cases, fallback, ..
        } => {
            manifest.add(PathSpec::new(
                path.clone(),
                ValueKind::Struct,
                cardinality,
                schema_name,
            ));
            for case in cases.values() {
                collect_case_paths(case, &path, cardinality, manifest);
            }
            if let Some(case) = fallback {
                collect_case_paths(case, &path, cardinality, manifest);
            }
        }
    }
}

fn collect_case_paths(
    case: &SchemaCase,
    parent: &str,
    cardinality: Cardinality,
    manifest: &mut PathManifest,
) {
    for field in &case.fields {
        collect_dependent_field_paths(field, parent, cardinality, &case.name, manifest);
    }
}

fn collect_ingredient_paths(
    ingredient: &Ingredient,
    root: &str,
    cardinality: Cardinality,
    manifest: &mut PathManifest,
) {
    let path = if ingredient.name.is_empty() {
        root.to_string()
    } else {
        format!("{}.{}", root, normalize_segment(&ingredient.name))
    };

    collect_ingredient_paths_at(ingredient, &path, cardinality, manifest);
}

fn collect_ingredient_paths_at(
    ingredient: &Ingredient,
    path: &str,
    cardinality: Cardinality,
    manifest: &mut PathManifest,
) {
    manifest.add(PathSpec::new(
        path.to_string(),
        value_kind_for_ingredient(ingredient),
        cardinality,
        ingredient.format.clone(),
    ));

    if let Some(children) = &ingredient.children {
        let child_cardinality = if ingredient.format == "array" {
            Cardinality::Repeated
        } else {
            cardinality
        };
        let child_root = if ingredient.format == "array" {
            format!("{}[*]", path)
        } else {
            path.to_string()
        };
        for child in children {
            collect_ingredient_paths(child, &child_root, child_cardinality, manifest);
        }
    }
}

fn value_kind_for_ingredient(ingredient: &Ingredient) -> ValueKind {
    match ingredient.format.as_str() {
        "u8" => ValueKind::U8,
        "u16" => ValueKind::U16,
        "u32" => ValueKind::U32,
        "u64" => ValueKind::U64,
        "string" | "str" => ValueKind::String,
        "binary" | "bytes" => ValueKind::Binary,
        "struct" => ValueKind::Struct,
        "array" => ValueKind::Array,
        _ => value_kind_from_default(&ingredient.default).unwrap_or(ValueKind::Unknown),
    }
}

fn value_kind_from_default(value: &Option<IngredientValue>) -> Option<ValueKind> {
    match value.as_ref()? {
        IngredientValue::U8(_) => Some(ValueKind::U8),
        IngredientValue::U16(_) => Some(ValueKind::U16),
        IngredientValue::U32(_) => Some(ValueKind::U32),
        IngredientValue::U64(_) => Some(ValueKind::U64),
        IngredientValue::Binary(_) => Some(ValueKind::Binary),
        IngredientValue::String(_) => Some(ValueKind::String),
        IngredientValue::Struct(_) => Some(ValueKind::Struct),
        IngredientValue::Array(_) => Some(ValueKind::Array),
        _ => Some(ValueKind::Unknown),
    }
}

pub fn normalize_segment(segment: &str) -> String {
    segment.trim().replace('-', "_").replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dependent::{DependentField, DependentRecipe, LengthExpr},
        program::{RecipeProgram, SequenceRecipe},
    };
    use recipe::ingredient_builder::IngredientBuilder;

    #[test]
    fn manifest_lists_repeated_sequence_paths_for_pathgen() {
        let frame = RecipeProgram::Dependent(
            DependentRecipe::new("frame")
                .field(DependentField::quic_varint("len"))
                .field(DependentField::bytes("payload", LengthExpr::from_field("len"))),
        );
        let stream = RecipeProgram::Sequence(
            SequenceRecipe::new("stream", "frames", frame).repeat_until_end(),
        );
        let manifest = PathManifest::from_program("fixture", &stream);

        assert!(manifest.contains_path("fixture.stream.frames[*]"));
        assert!(manifest.contains_path("fixture.stream.frames[*].len"));
        assert!(manifest.contains_path("fixture.stream.frames[*].payload"));
    }

    #[test]
    fn dependent_ingredient_field_paths_do_not_duplicate_field_name() {
        let packet = RecipeProgram::Dependent(
            DependentRecipe::new("packet").field(DependentField::ingredient(
                "version",
                IngredientBuilder::new()
                    .name("version")
                    .offset_bytes(0)
                    .format("u32")
                    .length_bytes(4)
                    .big_endian()
                    .build(),
            )),
        );
        let manifest = PathManifest::from_program("quic", &packet);

        assert!(manifest.contains_path("quic.packet.version"));
        assert!(!manifest.contains_path("quic.packet.version.version"));
    }
}
