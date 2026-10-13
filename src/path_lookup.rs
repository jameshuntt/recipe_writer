use recipe::{error::RecipeError, ingredient_value::IngredientValue};

use crate::{path_manifest::normalize_segment, program::ProgramParseResult};

/// Look up one concrete value from a parsed program result using a pathgen-style path.
///
/// Supported forms:
///
/// ```text
/// frames[0].frame_type
/// http3.stream_minimal.frames[0].frame_type
/// ```
///
/// Wildcards (`[*]`) are intentionally rejected here. Use `collect_program_path`
/// when a manifest path should return every matching repeated value.
pub fn lookup_program_path<'a>(
    result: &'a ProgramParseResult,
    namespace: &str,
    path: &str,
) -> Result<&'a IngredientValue, RecipeError> {
    let relative = strip_program_root(path, namespace, &result.program_name);
    lookup_value_path(&result.value, relative)
}

/// Collect zero or more values from a parsed program result using a path that may contain `[*]`.
///
/// This is the runtime counterpart to manifest paths such as:
///
/// ```text
/// http3.stream_minimal.frames[*].frame_type
/// ```
pub fn collect_program_path<'a>(
    result: &'a ProgramParseResult,
    namespace: &str,
    path: &str,
) -> Result<Vec<&'a IngredientValue>, RecipeError> {
    let relative = strip_program_root(path, namespace, &result.program_name);
    collect_value_path(&result.value, relative)
}

/// Look up one concrete value from an `IngredientValue` tree using a relative path.
pub fn lookup_value_path<'a>(
    root: &'a IngredientValue,
    relative_path: &str,
) -> Result<&'a IngredientValue, RecipeError> {
    if relative_path.trim().is_empty() {
        return Ok(root);
    }

    let mut current = root;
    for segment in relative_path.split('.').filter(|segment| !segment.is_empty()) {
        let parsed = PathSegment::parse(segment)?;
        if parsed.index == Some(PathIndex::Wildcard) {
            return Err(RecipeError::InvalidConfig(format!(
                "Path '{}' contains wildcard; use collect_value_path instead",
                relative_path
            )));
        }

        current = get_child(current, &parsed.name)?;
        if let Some(PathIndex::Index(index)) = parsed.index {
            current = get_index(current, index)?;
        }
    }
    Ok(current)
}

/// Collect values from an `IngredientValue` tree using a relative path that may contain `[*]`.
pub fn collect_value_path<'a>(
    root: &'a IngredientValue,
    relative_path: &str,
) -> Result<Vec<&'a IngredientValue>, RecipeError> {
    if relative_path.trim().is_empty() {
        return Ok(vec![root]);
    }

    let mut current = vec![root];
    for segment in relative_path.split('.').filter(|segment| !segment.is_empty()) {
        let parsed = PathSegment::parse(segment)?;
        let mut next = Vec::new();

        for value in current {
            let child = get_child(value, &parsed.name)?;
            match parsed.index {
                None => next.push(child),
                Some(PathIndex::Index(index)) => next.push(get_index(child, index)?),
                Some(PathIndex::Wildcard) => {
                    for item in get_array(child)? {
                        next.push(item);
                    }
                }
            }
        }

        current = next;
    }

    Ok(current)
}

/// Converts concrete indexed paths into their manifest form.
///
/// ```text
/// frames[0].payload -> frames[*].payload
/// ```
pub fn wildcardize_indexes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '[' {
            let mut digits = String::new();
            while let Some(next) = chars.peek().copied() {
                if next.is_ascii_digit() {
                    digits.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !digits.is_empty() && chars.peek() == Some(&']') {
                chars.next();
                out.push_str("[*]");
            } else {
                out.push('[');
                out.push_str(&digits);
            }
        } else {
            out.push(ch);
        }
    }

    out
}

pub fn value_as_u64(value: &IngredientValue) -> Option<u64> {
    match value {
        IngredientValue::U8(value) => Some(u64::from(*value)),
        IngredientValue::U16(value) => Some(u64::from(*value)),
        IngredientValue::U32(value) => Some(u64::from(*value)),
        IngredientValue::U64(value) => Some(*value),
        _ => None,
    }
}

pub fn value_as_bytes(value: &IngredientValue) -> Option<&[u8]> {
    match value {
        IngredientValue::Binary(bytes) => Some(bytes.as_slice()),
        _ => None,
    }
}

fn strip_program_root<'a>(path: &'a str, namespace: &str, program_name: &str) -> &'a str {
    let root = format!("{}.{}", normalize_segment(namespace), normalize_segment(program_name));
    if path == root {
        ""
    } else if let Some(rest) = path.strip_prefix(&(root + ".")) {
        rest
    } else {
        path
    }
}

fn get_child<'a>(value: &'a IngredientValue, name: &str) -> Result<&'a IngredientValue, RecipeError> {
    match value {
        IngredientValue::Struct(map) => map
            .get(name)
            .ok_or_else(|| RecipeError::MissingField(name.to_string())),
        other => Err(RecipeError::InvalidData(format!(
            "Cannot access field '{}' on non-struct value {:?}",
            name, other
        ))),
    }
}

fn get_index(value: &IngredientValue, index: usize) -> Result<&IngredientValue, RecipeError> {
    let array = get_array(value)?;
    array.get(index).ok_or(RecipeError::OutOfBounds)
}

fn get_array(value: &IngredientValue) -> Result<&Vec<IngredientValue>, RecipeError> {
    match value {
        IngredientValue::Array(items) => Ok(items),
        other => Err(RecipeError::InvalidData(format!(
            "Cannot index non-array value {:?}",
            other
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathSegment {
    name: String,
    index: Option<PathIndex>,
}

impl PathSegment {
    fn parse(segment: &str) -> Result<Self, RecipeError> {
        if let Some(open) = segment.find('[') {
            if !segment.ends_with(']') {
                return Err(RecipeError::InvalidConfig(format!(
                    "Malformed path segment '{}'",
                    segment
                )));
            }
            let name = &segment[..open];
            let index_part = &segment[open + 1..segment.len() - 1];
            if name.is_empty() {
                return Err(RecipeError::InvalidConfig(format!(
                    "Path segment '{}' is missing a field name",
                    segment
                )));
            }
            let index = if index_part == "*" {
                PathIndex::Wildcard
            } else {
                PathIndex::Index(index_part.parse::<usize>().map_err(|_| {
                    RecipeError::InvalidConfig(format!(
                        "Path segment '{}' has invalid array index",
                        segment
                    ))
                })?)
            };
            Ok(Self {
                name: normalize_segment(name),
                index: Some(index),
            })
        } else {
            Ok(Self {
                name: normalize_segment(segment),
                index: None,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathIndex {
    Index(usize),
    Wildcard,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dependent::{DependentField, DependentRecipe, LengthExpr},
        program::{RecipeProgram, SequenceRecipe},
    };

    fn tiny_stream() -> RecipeProgram {
        let frame = RecipeProgram::Dependent(
            DependentRecipe::new("tiny_frame")
                .field(DependentField::quic_varint("len"))
                .field(DependentField::bytes("payload", LengthExpr::from_field("len"))),
        );
        RecipeProgram::Sequence(
            SequenceRecipe::new("tiny_stream", "frames", frame).repeat_until_end(),
        )
    }

    #[test]
    fn lookup_returns_value_at_concrete_path() {
        let parsed = tiny_stream().parse(&[0x02, 0xAA, 0xBB, 0x01, 0xCC]).unwrap();
        let value = lookup_program_path(&parsed, "fixture", "fixture.tiny_stream.frames[0].len").unwrap();
        assert_eq!(value_as_u64(value), Some(2));

        let payload = lookup_program_path(&parsed, "fixture", "frames[1].payload").unwrap();
        assert_eq!(value_as_bytes(payload), Some(&[0xCC][..]));
    }

    #[test]
    fn collect_returns_values_for_wildcard_path() {
        let parsed = tiny_stream().parse(&[0x02, 0xAA, 0xBB, 0x01, 0xCC]).unwrap();
        let values = collect_program_path(&parsed, "fixture", "fixture.tiny_stream.frames[*].len").unwrap();
        let lens = values
            .iter()
            .map(|value| value_as_u64(value).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lens, vec![2, 1]);
    }

    #[test]
    fn wildcardizes_concrete_indexes_for_manifest_matching() {
        assert_eq!(
            wildcardize_indexes("http3.stream.frames[12].payload.data"),
            "http3.stream.frames[*].payload.data"
        );
    }
}
