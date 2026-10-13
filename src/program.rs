use std::collections::HashMap;

use recipe::{
    error::RecipeError,
    ingredient::Ingredient,
    ingredient_value::IngredientValue,
    parsers::GLOBAL_REGISTRY,
};

use crate::{
    dependent::{DependentRecipe, SchemaTransition},
    packet_plan::StreamBlueprint,
};

/// A compiled recipe program that can be executed by the stream layer.
///
/// This is the bridge between the stable `recipe` kernel, dynamic dependent
/// recipes, and repeated stream recipes. Path generation should target this
/// level instead of caring which low-level parser produced a value.
#[derive(Debug, Clone)]
pub enum RecipeProgram {
    Static(Ingredient),
    Dependent(DependentRecipe),
    Sequence(SequenceRecipe),
}

impl RecipeProgram {
    pub fn name(&self) -> &str {
        match self {
            RecipeProgram::Static(ingredient) => &ingredient.name,
            RecipeProgram::Dependent(recipe) => &recipe.name,
            RecipeProgram::Sequence(sequence) => &sequence.name,
        }
    }

    pub fn parse(&self, data: &[u8]) -> Result<ProgramParseResult, RecipeError> {
        match self {
            RecipeProgram::Static(ingredient) => {
                ingredient.validate_tree_depth(recipe::ingredient::DEFAULT_MAX_INGREDIENT_DEPTH)?;
                let value = GLOBAL_REGISTRY.parse_ingredient(data, ingredient)?;
                let bytes_consumed = ingredient
                    .offset
                    .0
                    .checked_add(ingredient.length.0)
                    .ok_or(RecipeError::OutOfBounds)?;

                Ok(ProgramParseResult {
                    program_name: self.name().to_string(),
                    bytes_consumed,
                    value,
                    transitions: Vec::new(),
                    frames: Vec::new(),
                })
            }
            RecipeProgram::Dependent(recipe) => {
                let parsed = recipe.parse(data)?;
                Ok(ProgramParseResult {
                    program_name: parsed.recipe_name,
                    bytes_consumed: parsed.bytes_consumed,
                    value: parsed.value,
                    transitions: parsed.transitions,
                    frames: Vec::new(),
                })
            }
            RecipeProgram::Sequence(sequence) => sequence.parse(data),
        }
    }
}

/// A repeated program over a single byte stream.
///
/// For HTTP/3 this is "repeat one frame recipe until the stream is exhausted".
/// For file ingestion this can become "repeat chunk recipe until the declared
/// packet budget is exhausted, then switch to the next instruction recipe".
#[derive(Debug, Clone)]
pub struct SequenceRecipe {
    pub name: String,
    pub item_field_name: String,
    pub item: Box<RecipeProgram>,
    pub repeat: RepeatMode,
}

impl SequenceRecipe {
    pub fn new(
        name: impl Into<String>,
        item_field_name: impl Into<String>,
        item: RecipeProgram,
    ) -> Self {
        Self {
            name: name.into(),
            item_field_name: item_field_name.into(),
            item: Box::new(item),
            repeat: RepeatMode::UntilEnd,
        }
    }

    pub fn repeat_until_end(mut self) -> Self {
        self.repeat = RepeatMode::UntilEnd;
        self
    }

    pub fn repeat_count(mut self, count: usize) -> Self {
        self.repeat = RepeatMode::Count(count);
        self
    }

    pub fn parse(&self, data: &[u8]) -> Result<ProgramParseResult, RecipeError> {
        let mut cursor = 0usize;
        let mut index = 0usize;
        let mut values = Vec::new();
        let mut transitions = Vec::new();
        let mut frames = Vec::new();

        loop {
            match self.repeat {
                RepeatMode::UntilEnd => {
                    if cursor >= data.len() {
                        break;
                    }
                }
                RepeatMode::Count(count) => {
                    if index >= count {
                        break;
                    }
                    if cursor >= data.len() {
                        return Err(RecipeError::OutOfBounds);
                    }
                }
            }

            let start = cursor;
            let parsed = self.item.parse(&data[start..])?;
            if parsed.bytes_consumed == 0 {
                return Err(RecipeError::InvalidConfig(format!(
                    "Sequence recipe '{}' item '{}' consumed zero bytes",
                    self.name,
                    self.item.name()
                )));
            }

            let end = start
                .checked_add(parsed.bytes_consumed)
                .ok_or(RecipeError::OutOfBounds)?;
            if end > data.len() {
                return Err(RecipeError::OutOfBounds);
            }

            for mut transition in parsed.transitions {
                transition.cursor_at_transition = transition
                    .cursor_at_transition
                    .checked_add(start)
                    .ok_or(RecipeError::OutOfBounds)?;
                transitions.push(transition);
            }

            for mut frame in parsed.frames {
                frame.stream_index_path = format!(
                    "{}[{}].{}",
                    self.item_field_name, index, frame.stream_index_path
                );
                frame.byte_start = frame
                    .byte_start
                    .checked_add(start)
                    .ok_or(RecipeError::OutOfBounds)?;
                frame.byte_end = frame
                    .byte_end
                    .checked_add(start)
                    .ok_or(RecipeError::OutOfBounds)?;
                frames.push(frame);
            }

            frames.push(FrameReceipt {
                stream_index_path: format!("{}[{}]", self.item_field_name, index),
                program_name: self.item.name().to_string(),
                byte_start: start,
                byte_end: end,
            });
            values.push(parsed.value);
            cursor = end;
            index += 1;
        }

        let mut root = HashMap::new();
        root.insert(
            self.item_field_name.clone(),
            IngredientValue::Array(values),
        );

        Ok(ProgramParseResult {
            program_name: self.name.clone(),
            bytes_consumed: cursor,
            value: IngredientValue::Struct(root),
            transitions,
            frames,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepeatMode {
    UntilEnd,
    Count(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameReceipt {
    pub stream_index_path: String,
    pub program_name: String,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Clone)]
pub struct ProgramParseResult {
    pub program_name: String,
    pub bytes_consumed: usize,
    pub value: IngredientValue,
    pub transitions: Vec<SchemaTransition>,
    pub frames: Vec<FrameReceipt>,
}

/// A program-aware orchestrator for the existing sequence blueprint layer.
///
/// This leaves the older `StreamOrchestrator` intact while giving new code a
/// place to run `Static`, `Dependent`, or `Sequence` programs by packet number.
#[derive(Debug, Clone)]
pub struct ProgramStreamOrchestrator {
    pub blueprint: StreamBlueprint,
    pub program_map: HashMap<String, RecipeProgram>,
}

impl ProgramStreamOrchestrator {
    pub fn new(blueprint: StreamBlueprint) -> Self {
        Self {
            blueprint,
            program_map: HashMap::new(),
        }
    }

    pub fn with_program(mut self, name: impl Into<String>, program: RecipeProgram) -> Self {
        self.program_map.insert(name.into(), program);
        self
    }

    pub fn insert_program(&mut self, name: impl Into<String>, program: RecipeProgram) {
        self.program_map.insert(name.into(), program);
    }

    pub fn get_program_name_for_packet(&self, pn: u64) -> Option<String> {
        self.blueprint
            .entries
            .iter()
            .find(|entry| {
                let start = entry.start_seq;
                let end = entry.end_seq.unwrap_or(u64::MAX);
                pn >= start && pn <= end
            })
            .map(|entry| entry.recipe_name.clone())
    }

    pub fn ingest(&self, pn: u64, data: &[u8]) -> Result<ProgramParseResult, RecipeError> {
        let program_name = self
            .get_program_name_for_packet(pn)
            .ok_or_else(|| RecipeError::MissingField(format!("No program for packet {}", pn)))?;
        let program = self.program_map.get(&program_name).ok_or_else(|| {
            RecipeError::UnsupportedFormat(format!("Program '{}' not registered", program_name))
        })?;
        program.parse(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependent::{DependentField, LengthExpr};

    fn get_struct(value: &IngredientValue) -> &HashMap<String, IngredientValue> {
        match value {
            IngredientValue::Struct(map) => map,
            other => panic!("expected struct, got {other:?}"),
        }
    }

    fn get_array(value: &IngredientValue) -> &Vec<IngredientValue> {
        match value {
            IngredientValue::Array(items) => items,
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn sequence_recipe_repeats_dependent_frames_until_end() {
        let frame = RecipeProgram::Dependent(
            DependentRecipe::new("tiny_frame")
                .field(DependentField::quic_varint("len"))
                .field(DependentField::bytes("payload", LengthExpr::from_field("len"))),
        );
        let stream = RecipeProgram::Sequence(
            SequenceRecipe::new("tiny_stream", "frames", frame).repeat_until_end(),
        );

        let parsed = stream.parse(&[0x02, 0xAA, 0xBB, 0x01, 0xCC]).unwrap();
        let root = get_struct(&parsed.value);
        let frames = get_array(root.get("frames").unwrap());

        assert_eq!(parsed.bytes_consumed, 5);
        assert_eq!(frames.len(), 2);
        assert_eq!(parsed.frames.len(), 2);
        assert_eq!(parsed.frames[0].stream_index_path, "frames[0]");
        assert_eq!(parsed.frames[0].byte_start, 0);
        assert_eq!(parsed.frames[0].byte_end, 3);
        assert_eq!(parsed.frames[1].stream_index_path, "frames[1]");
        assert_eq!(parsed.frames[1].byte_start, 3);
        assert_eq!(parsed.frames[1].byte_end, 5);
    }

    #[test]
    fn program_orchestrator_selects_dependent_program_from_blueprint() {
        let blueprint = StreamBlueprint {
            name: "planned".into(),
            entries: vec![crate::packet_plan::PlanEntry {
                recipe_name: "tiny_frame".into(),
                start_seq: 0,
                end_seq: None,
            }],
        };
        let program = RecipeProgram::Dependent(
            DependentRecipe::new("tiny_frame")
                .field(DependentField::quic_varint("len"))
                .field(DependentField::bytes("payload", LengthExpr::from_field("len"))),
        );
        let orchestrator = ProgramStreamOrchestrator::new(blueprint).with_program("tiny_frame", program);

        let parsed = orchestrator.ingest(42, &[0x01, 0xEF]).unwrap();
        assert_eq!(parsed.program_name, "tiny_frame");
        assert_eq!(parsed.bytes_consumed, 2);
    }
}
