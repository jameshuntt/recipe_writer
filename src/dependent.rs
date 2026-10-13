use std::collections::{BTreeMap, HashMap};

use recipe::{
    error::RecipeError,
    ingredient::Ingredient,
    ingredient_value::IngredientValue,
    parsers::GLOBAL_REGISTRY,
    types::Offset,
};

use crate::helpers::read_quic_varint_simple;

/// A sequential, context-aware recipe for protocols where later fields depend
/// on earlier fields.
///
/// This sits above `recipe::Ingredient`: the core crate still handles stable
/// primitive/container extraction, while `DependentRecipe` handles cursors,
/// length-prefixed scopes, tagged dispatch, and schema flips.
#[derive(Debug, Clone)]
pub struct DependentRecipe {
    pub name: String,
    pub fields: Vec<DependentField>,
}

impl DependentRecipe {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
        }
    }

    pub fn field(mut self, field: DependentField) -> Self {
        self.fields.push(field);
        self
    }

    pub fn parse(&self, data: &[u8]) -> Result<DependentParseResult, RecipeError> {
        let mut ctx = DependentContext::new(data.len());
        let value = parse_fields(data, &self.fields, &mut ctx)?;

        Ok(DependentParseResult {
            recipe_name: self.name.clone(),
            bytes_consumed: ctx.cursor,
            value: IngredientValue::Struct(value),
            transitions: ctx.transitions,
        })
    }
}

/// One field in a dependent recipe.
#[derive(Debug, Clone)]
pub struct DependentField {
    pub name: String,
    pub kind: DependentFieldKind,
}

impl DependentField {
    pub fn quic_varint(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::QuicVarInt,
        }
    }

    pub fn bytes(name: impl Into<String>, length: LengthExpr) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::Bytes { length },
        }
    }

    pub fn ingredient(name: impl Into<String>, ingredient: Ingredient) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::Ingredient { ingredient },
        }
    }

    /// Repeat a group of dependent fields until the current scope is exhausted.
    ///
    /// This is intentionally scope-bounded. It is useful for payloads such as
    /// HTTP/3 SETTINGS, where the declared frame length contains repeated
    /// `id, value` pairs.
    pub fn repeated_group(name: impl Into<String>, fields: Vec<DependentField>) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::RepeatedGroup { fields },
        }
    }

    pub fn scope(
        name: impl Into<String>,
        length: LengthExpr,
        fields: Vec<DependentField>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::Scope { length, fields },
        }
    }

    pub fn switch(
        name: impl Into<String>,
        on: impl Into<String>,
        cases: BTreeMap<u64, SchemaCase>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::Switch {
                on: on.into(),
                cases,
                fallback: None,
            },
        }
    }

    pub fn switch_with_fallback(
        name: impl Into<String>,
        on: impl Into<String>,
        cases: BTreeMap<u64, SchemaCase>,
        fallback: SchemaCase,
    ) -> Self {
        Self {
            name: name.into(),
            kind: DependentFieldKind::Switch {
                on: on.into(),
                cases,
                fallback: Some(fallback),
            },
        }
    }
}

/// The execution mode for a dependent field.
#[derive(Debug, Clone)]
pub enum DependentFieldKind {
    /// Delegate one field to the stable `recipe` parser registry.
    Ingredient { ingredient: Ingredient },

    /// QUIC / HTTP/3 variable-length integer.
    QuicVarInt,

    /// Raw bytes where the length can depend on a prior field.
    Bytes { length: LengthExpr },

    /// A repeated group of fields parsed until the current scope is exhausted.
    RepeatedGroup { fields: Vec<DependentField> },

    /// A bounded child parse. This is the key primitive for HTTP/3 frames:
    /// read the frame length, enter a payload scope, then dispatch inside it.
    Scope {
        length: LengthExpr,
        fields: Vec<DependentField>,
    },

    /// Tagged dispatch. Read selector `on` from already parsed fields and
    /// activate one schema case at the current cursor.
    Switch {
        on: String,
        cases: BTreeMap<u64, SchemaCase>,
        fallback: Option<SchemaCase>,
    },
}

/// A dynamic length expression. This is the smallest useful piece needed for
/// dependent fields: fixed bytes, remaining scope bytes, or "length from a field
/// I already parsed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LengthExpr {
    Fixed(usize),
    FromField(String),
    Remaining,
}

impl LengthExpr {
    pub fn fixed(len: usize) -> Self {
        Self::Fixed(len)
    }

    pub fn from_field(name: impl Into<String>) -> Self {
        Self::FromField(name.into())
    }
}

/// One branch of a `Switch` field.
#[derive(Debug, Clone)]
pub struct SchemaCase {
    pub name: String,
    pub fields: Vec<DependentField>,
}

impl SchemaCase {
    pub fn new(name: impl Into<String>, fields: Vec<DependentField>) -> Self {
        Self {
            name: name.into(),
            fields,
        }
    }
}

/// A receipt showing that a stream identifier activated a different schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaTransition {
    pub selector_field: String,
    pub selector_value: u64,
    pub schema_name: String,
    pub cursor_at_transition: usize,
}

#[derive(Debug, Clone)]
pub struct DependentParseResult {
    pub recipe_name: String,
    pub bytes_consumed: usize,
    pub value: IngredientValue,
    pub transitions: Vec<SchemaTransition>,
}

#[derive(Debug, Clone)]
struct DependentContext {
    cursor: usize,
    scope_limit: usize,
    values: HashMap<String, IngredientValue>,
    transitions: Vec<SchemaTransition>,
}

impl DependentContext {
    fn new(limit: usize) -> Self {
        Self {
            cursor: 0,
            scope_limit: limit,
            values: HashMap::new(),
            transitions: Vec::new(),
        }
    }

    fn fork_for_scope(&self, limit: usize) -> Self {
        Self {
            cursor: self.cursor,
            scope_limit: limit,
            values: self.values.clone(),
            transitions: Vec::new(),
        }
    }

    fn absorb_scope(&mut self, child: DependentContext) {
        self.cursor = child.cursor;
        self.values.extend(child.values);
        self.transitions.extend(child.transitions);
    }

    fn remaining(&self) -> usize {
        self.scope_limit.saturating_sub(self.cursor)
    }

    fn resolve_len(&self, expr: &LengthExpr) -> Result<usize, RecipeError> {
        match expr {
            LengthExpr::Fixed(len) => Ok(*len),
            LengthExpr::Remaining => Ok(self.remaining()),
            LengthExpr::FromField(name) => self.lookup_usize(name),
        }
    }

    fn lookup_u64(&self, name: &str) -> Result<u64, RecipeError> {
        let value = self
            .values
            .get(name)
            .ok_or_else(|| RecipeError::MissingField(name.to_string()))?;

        ingredient_value_to_u64(value).ok_or_else(|| {
            RecipeError::InvalidConfig(format!(
                "Field '{}' cannot be used as an unsigned selector/length: {:?}",
                name, value
            ))
        })
    }

    fn lookup_usize(&self, name: &str) -> Result<usize, RecipeError> {
        usize::try_from(self.lookup_u64(name)?).map_err(|_| {
            RecipeError::InvalidConfig(format!(
                "Field '{}' is too large to use as a usize length",
                name
            ))
        })
    }
}

fn parse_fields(
    data: &[u8],
    fields: &[DependentField],
    ctx: &mut DependentContext,
) -> Result<HashMap<String, IngredientValue>, RecipeError> {
    let mut out = HashMap::new();

    for field in fields {
        let value = parse_field(data, field, ctx)?;
        ctx.values.insert(field.name.clone(), value.clone());
        out.insert(field.name.clone(), value);
    }

    Ok(out)
}

fn parse_field(
    data: &[u8],
    field: &DependentField,
    ctx: &mut DependentContext,
) -> Result<IngredientValue, RecipeError> {
    match &field.kind {
        DependentFieldKind::QuicVarInt => {
            let input = data
                .get(ctx.cursor..ctx.scope_limit)
                .ok_or(RecipeError::OutOfBounds)?;
            let (value, consumed) = read_quic_varint_simple(input)?;
            advance(ctx, consumed)?;
            Ok(IngredientValue::U64(value))
        }
        DependentFieldKind::Bytes { length } => {
            let len = ctx.resolve_len(length)?;
            let start = ctx.cursor;
            let end = checked_end(start, len, ctx.scope_limit)?;
            ctx.cursor = end;
            Ok(IngredientValue::Binary(data[start..end].to_vec()))
        }
        DependentFieldKind::RepeatedGroup { fields } => {
            let mut values = Vec::new();
            while ctx.cursor < ctx.scope_limit {
                let start = ctx.cursor;
                let item_values = parse_fields(data, fields, ctx)?;
                if ctx.cursor == start {
                    return Err(RecipeError::InvalidConfig(format!(
                        "Repeated field group '{}' consumed zero bytes",
                        field.name
                    )));
                }
                values.push(IngredientValue::Struct(item_values));
            }
            Ok(IngredientValue::Array(values))
        }
        DependentFieldKind::Ingredient { ingredient } => {
            let mut local = ingredient.clone();
            local.offset = Offset(ctx.cursor + ingredient.offset.0);

            let consumed = ingredient
                .offset
                .0
                .checked_add(ingredient.length.0)
                .ok_or(RecipeError::OutOfBounds)?;
            checked_end(ctx.cursor, consumed, ctx.scope_limit)?;

            let value = GLOBAL_REGISTRY.parse_ingredient(data, &local)?;
            advance(ctx, consumed)?;
            Ok(value)
        }
        DependentFieldKind::Scope { length, fields } => {
            let len = ctx.resolve_len(length)?;
            let start = ctx.cursor;
            let end = checked_end(start, len, ctx.scope_limit)?;

            let mut child_ctx = ctx.fork_for_scope(end);
            let scoped_values = parse_fields(data, fields, &mut child_ctx)?;

            // A scoped payload may intentionally leave bytes unparsed, for example
            // unknown HTTP/3 frame data. Skip to the declared end to keep the stream aligned.
            child_ctx.cursor = end;
            ctx.absorb_scope(child_ctx);

            Ok(IngredientValue::Struct(scoped_values))
        }
        DependentFieldKind::Switch {
            on,
            cases,
            fallback,
        } => {
            let selector = ctx.lookup_u64(on)?;
            let case = cases.get(&selector).or(fallback.as_ref()).ok_or_else(|| {
                RecipeError::InvalidConfig(format!(
                    "No schema case for selector '{}' = {}",
                    on, selector
                ))
            })?;

            ctx.transitions.push(SchemaTransition {
                selector_field: on.clone(),
                selector_value: selector,
                schema_name: case.name.clone(),
                cursor_at_transition: ctx.cursor,
            });

            let case_values = parse_fields(data, &case.fields, ctx)?;
            Ok(IngredientValue::Struct(case_values))
        }
    }
}

fn advance(ctx: &mut DependentContext, len: usize) -> Result<(), RecipeError> {
    ctx.cursor = checked_end(ctx.cursor, len, ctx.scope_limit)?;
    Ok(())
}

fn checked_end(start: usize, len: usize, limit: usize) -> Result<usize, RecipeError> {
    let end = start.checked_add(len).ok_or(RecipeError::OutOfBounds)?;
    if end > limit {
        return Err(RecipeError::OutOfBounds);
    }
    Ok(end)
}

fn ingredient_value_to_u64(value: &IngredientValue) -> Option<u64> {
    match value {
        IngredientValue::U8(v) => Some((*v).into()),
        IngredientValue::U16(v) => Some((*v).into()),
        IngredientValue::U32(v) => Some((*v).into()),
        IngredientValue::U64(v) => Some(*v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_struct(value: &IngredientValue) -> &HashMap<String, IngredientValue> {
        match value {
            IngredientValue::Struct(map) => map,
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn parses_length_prefixed_payload_from_prior_field() {
        let recipe = DependentRecipe::new("http3_data_like_frame")
            .field(DependentField::quic_varint("frame_type"))
            .field(DependentField::quic_varint("frame_len"))
            .field(DependentField::bytes(
                "payload",
                LengthExpr::from_field("frame_len"),
            ));

        let parsed = recipe.parse(&[0x00, 0x03, 0xAA, 0xBB, 0xCC]).unwrap();
        let root = get_struct(&parsed.value);

        assert_eq!(root.get("frame_type"), Some(&IngredientValue::U64(0)));
        assert_eq!(root.get("frame_len"), Some(&IngredientValue::U64(3)));
        assert_eq!(
            root.get("payload"),
            Some(&IngredientValue::Binary(vec![0xAA, 0xBB, 0xCC]))
        );
        assert_eq!(parsed.bytes_consumed, 5);
    }


    #[test]
    fn parses_repeated_group_until_scope_end() {
        let recipe = DependentRecipe::new("http3_settings_like_frame")
            .field(DependentField::quic_varint("frame_type"))
            .field(DependentField::quic_varint("frame_len"))
            .field(DependentField::scope(
                "payload",
                LengthExpr::from_field("frame_len"),
                vec![DependentField::repeated_group(
                    "settings",
                    vec![
                        DependentField::quic_varint("id"),
                        DependentField::quic_varint("value"),
                    ],
                )],
            ));

        let parsed = recipe.parse(&[0x04, 0x04, 0x01, 0x00, 0x06, 0x10]).unwrap();
        let root = get_struct(&parsed.value);
        let payload = get_struct(root.get("payload").unwrap());
        let settings = match payload.get("settings").unwrap() {
            IngredientValue::Array(settings) => settings,
            other => panic!("expected settings array, got {other:?}"),
        };

        assert_eq!(settings.len(), 2);
        assert_eq!(parsed.bytes_consumed, 6);
        assert_eq!(
            get_struct(&settings[0]).get("id"),
            Some(&IngredientValue::U64(1))
        );
        assert_eq!(
            get_struct(&settings[1]).get("value"),
            Some(&IngredientValue::U64(16))
        );
    }

    #[test]
    fn switches_schema_from_stream_identifier_inside_payload_scope() {
        let mut cases = BTreeMap::new();
        cases.insert(
            0x00,
            SchemaCase::new(
                "data_frame",
                vec![DependentField::bytes("data", LengthExpr::Remaining)],
            ),
        );
        cases.insert(
            0x01,
            SchemaCase::new(
                "headers_frame",
                vec![DependentField::bytes("qpack_block", LengthExpr::Remaining)],
            ),
        );

        let recipe = DependentRecipe::new("http3_frame_minimal")
            .field(DependentField::quic_varint("frame_type"))
            .field(DependentField::quic_varint("frame_len"))
            .field(DependentField::scope(
                "payload",
                LengthExpr::from_field("frame_len"),
                vec![DependentField::switch("active_schema", "frame_type", cases)],
            ));

        // frame_type = 0x01 HEADERS, frame_len = 2, payload = QPACK bytes [0x10, 0x20]
        let parsed = recipe.parse(&[0x01, 0x02, 0x10, 0x20]).unwrap();
        let root = get_struct(&parsed.value);
        let payload = get_struct(root.get("payload").unwrap());
        let active_schema = get_struct(payload.get("active_schema").unwrap());

        assert_eq!(
            active_schema.get("qpack_block"),
            Some(&IngredientValue::Binary(vec![0x10, 0x20]))
        );
        assert_eq!(parsed.bytes_consumed, 4);
        assert_eq!(parsed.transitions.len(), 1);
        assert_eq!(parsed.transitions[0].selector_field, "frame_type");
        assert_eq!(parsed.transitions[0].selector_value, 0x01);
        assert_eq!(parsed.transitions[0].schema_name, "headers_frame");
        assert_eq!(parsed.transitions[0].cursor_at_transition, 2);
    }

    #[test]
    fn rejects_payloads_that_exceed_declared_scope() {
        let recipe = DependentRecipe::new("bad_frame")
            .field(DependentField::quic_varint("frame_type"))
            .field(DependentField::quic_varint("frame_len"))
            .field(DependentField::bytes(
                "payload",
                LengthExpr::from_field("frame_len"),
            ));

        let err = recipe.parse(&[0x00, 0x03, 0xAA]).unwrap_err();
        assert!(matches!(err, RecipeError::OutOfBounds));
    }
}
