use recipe::{error::RecipeError, ingredient_value::IngredientValue};

use crate::{
    path_lookup::{lookup_program_path, value_as_bytes, value_as_u64, wildcardize_indexes},
    path_manifest::PathManifest,
    program::{ProgramParseResult, RecipeProgram},
};

/// A named byte-stream fixture tied to a recipe program and expected paths.
///
/// This is the missing bridge between protocol recipes and pathgen: fixtures
/// prove that generated paths are not just declared by a manifest, but actually
/// resolve against parsed stream output.
#[derive(Debug, Clone)]
pub struct StreamFixture {
    pub name: String,
    pub namespace: String,
    pub program: RecipeProgram,
    pub input: Vec<u8>,
    pub expectations: Vec<PathExpectation>,
}

impl StreamFixture {
    pub fn new(
        name: impl Into<String>,
        namespace: impl Into<String>,
        program: RecipeProgram,
        input: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            name: name.into(),
            namespace: namespace.into(),
            program,
            input: input.into(),
            expectations: Vec::new(),
        }
    }

    pub fn expect_u64(mut self, path: impl Into<String>, expected: u64) -> Self {
        self.expectations.push(PathExpectation {
            path: path.into(),
            expected: ExpectedValue::U64(expected),
        });
        self
    }

    pub fn expect_bytes(mut self, path: impl Into<String>, expected: impl Into<Vec<u8>>) -> Self {
        self.expectations.push(PathExpectation {
            path: path.into(),
            expected: ExpectedValue::Bytes(expected.into()),
        });
        self
    }

    pub fn expect_string(mut self, path: impl Into<String>, expected: impl Into<String>) -> Self {
        self.expectations.push(PathExpectation {
            path: path.into(),
            expected: ExpectedValue::String(expected.into()),
        });
        self
    }

    pub fn run(&self) -> Result<FixtureRun, RecipeError> {
        let manifest = PathManifest::from_program(&self.namespace, &self.program);
        let parsed = self.program.parse(&self.input)?;

        for expectation in &self.expectations {
            let manifest_path = wildcardize_indexes(&expectation.path);
            if !manifest.contains_path(&manifest_path) {
                return Err(RecipeError::MissingField(format!(
                    "Fixture '{}' expected path '{}' but manifest does not contain '{}'",
                    self.name, expectation.path, manifest_path
                )));
            }

            let actual = lookup_program_path(&parsed, &self.namespace, &expectation.path)?;
            expectation.expected.assert_matches(&self.name, &expectation.path, actual)?;
        }

        Ok(FixtureRun { parsed, manifest })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathExpectation {
    pub path: String,
    pub expected: ExpectedValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedValue {
    U64(u64),
    Bytes(Vec<u8>),
    String(String),
}

impl ExpectedValue {
    fn assert_matches(
        &self,
        fixture_name: &str,
        path: &str,
        actual: &IngredientValue,
    ) -> Result<(), RecipeError> {
        match self {
            ExpectedValue::U64(expected) => match value_as_u64(actual) {
                Some(actual) if actual == *expected => Ok(()),
                Some(actual) => Err(RecipeError::InvalidData(format!(
                    "Fixture '{}' path '{}' expected u64 {}, got {}",
                    fixture_name, path, expected, actual
                ))),
                None => Err(RecipeError::InvalidData(format!(
                    "Fixture '{}' path '{}' expected u64, got {:?}",
                    fixture_name, path, actual
                ))),
            },
            ExpectedValue::Bytes(expected) => match value_as_bytes(actual) {
                Some(actual) if actual == expected.as_slice() => Ok(()),
                Some(actual) => Err(RecipeError::InvalidData(format!(
                    "Fixture '{}' path '{}' expected bytes {:?}, got {:?}",
                    fixture_name, path, expected, actual
                ))),
                None => Err(RecipeError::InvalidData(format!(
                    "Fixture '{}' path '{}' expected bytes, got {:?}",
                    fixture_name, path, actual
                ))),
            },
            ExpectedValue::String(expected) => match actual {
                IngredientValue::String(actual) if actual == expected => Ok(()),
                other => Err(RecipeError::InvalidData(format!(
                    "Fixture '{}' path '{}' expected string {:?}, got {:?}",
                    fixture_name, path, expected, other
                ))),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct FixtureRun {
    pub parsed: ProgramParseResult,
    pub manifest: PathManifest,
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
    fn fixture_asserts_manifest_paths_and_parsed_values() {
        let run = StreamFixture::new(
            "tiny_two_frame_stream",
            "fixture",
            tiny_stream(),
            [0x02, 0xAA, 0xBB, 0x01, 0xCC],
        )
        .expect_u64("fixture.tiny_stream.frames[0].len", 2)
        .expect_bytes("fixture.tiny_stream.frames[1].payload", [0xCC])
        .run()
        .unwrap();

        assert!(run.manifest.contains_path("fixture.tiny_stream.frames[*].len"));
        assert_eq!(run.parsed.frames.len(), 2);
    }
}
