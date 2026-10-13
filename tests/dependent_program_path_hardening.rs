use std::collections::{BTreeMap, HashMap};

use recipe::{error::RecipeError, ingredient_value::IngredientValue};
use recipe_writer::{
    dependent::{DependentField, DependentRecipe, LengthExpr, SchemaCase},
    path_lookup::{collect_program_path, collect_value_path, lookup_program_path, lookup_value_path, value_as_bytes, value_as_u64, wildcardize_indexes},
    path_manifest::PathManifest,
    path_manifest_export::PathgenManifestV1,
    program::{ProgramStreamOrchestrator, RecipeProgram, SequenceRecipe},
    transfer_plan::TransferPlan,
};

fn as_struct(value: &IngredientValue) -> &HashMap<String, IngredientValue> {
    match value {
        IngredientValue::Struct(map) => map,
        other => panic!("expected struct, got {other:?}"),
    }
}

fn as_array(value: &IngredientValue) -> &Vec<IngredientValue> {
    match value {
        IngredientValue::Array(items) => items,
        other => panic!("expected array, got {other:?}"),
    }
}

fn len_payload_recipe() -> DependentRecipe {
    DependentRecipe::new("len_payload")
        .field(DependentField::quic_varint("len"))
        .field(DependentField::bytes("payload", LengthExpr::from_field("len")))
}

fn tiny_frame_program() -> RecipeProgram {
    RecipeProgram::Dependent(len_payload_recipe())
}

fn tiny_stream_program() -> RecipeProgram {
    RecipeProgram::Sequence(
        SequenceRecipe::new("tiny_stream", "frames", tiny_frame_program()).repeat_until_end(),
    )
}

fn switch_recipe_with_fallback() -> DependentRecipe {
    let mut cases = BTreeMap::new();
    cases.insert(
        1,
        SchemaCase::new("one_byte", vec![DependentField::bytes("one", LengthExpr::fixed(1))]),
    );
    cases.insert(
        2,
        SchemaCase::new("two_bytes", vec![DependentField::bytes("two", LengthExpr::fixed(2))]),
    );

    DependentRecipe::new("switch_payload")
        .field(DependentField::quic_varint("tag"))
        .field(DependentField::switch_with_fallback(
            "active_schema",
            "tag",
            cases,
            SchemaCase::new("fallback", vec![DependentField::bytes("raw", LengthExpr::Remaining)]),
        ))
}

#[test]
fn quic_varint_one_byte_decodes_value_and_cursor() {
    let parsed = len_payload_recipe().parse(&[0x00]).unwrap();
    assert_eq!(as_struct(&parsed.value).get("len"), Some(&IngredientValue::U64(0)));
    assert_eq!(parsed.bytes_consumed, 1);
}

#[test]
fn quic_varint_two_byte_decodes_value_and_cursor() {
    let recipe = DependentRecipe::new("varint").field(DependentField::quic_varint("value"));
    let parsed = recipe.parse(&[0x40, 0x25]).unwrap();
    assert_eq!(as_struct(&parsed.value).get("value"), Some(&IngredientValue::U64(37)));
    assert_eq!(parsed.bytes_consumed, 2);
}

#[test]
fn quic_varint_four_byte_decodes_value_and_cursor() {
    let recipe = DependentRecipe::new("varint").field(DependentField::quic_varint("value"));
    let parsed = recipe.parse(&[0x80, 0x00, 0x01, 0x00]).unwrap();
    assert_eq!(as_struct(&parsed.value).get("value"), Some(&IngredientValue::U64(256)));
    assert_eq!(parsed.bytes_consumed, 4);
}

#[test]
fn quic_varint_eight_byte_decodes_value_and_cursor() {
    let recipe = DependentRecipe::new("varint").field(DependentField::quic_varint("value"));
    let parsed = recipe.parse(&[0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]).unwrap();
    assert_eq!(as_struct(&parsed.value).get("value"), Some(&IngredientValue::U64(256)));
    assert_eq!(parsed.bytes_consumed, 8);
}

#[test]
fn truncated_two_byte_varint_is_out_of_bounds() {
    let recipe = DependentRecipe::new("varint").field(DependentField::quic_varint("value"));
    assert!(matches!(recipe.parse(&[0x40]).unwrap_err(), RecipeError::OutOfBounds));
}

#[test]
fn zero_length_payload_is_allowed_and_consumes_only_length_field() {
    let parsed = len_payload_recipe().parse(&[0x00]).unwrap();
    let root = as_struct(&parsed.value);
    assert_eq!(root.get("payload"), Some(&IngredientValue::Binary(vec![])));
    assert_eq!(parsed.bytes_consumed, 1);
}

#[test]
fn declared_payload_must_not_exceed_scope() {
    let err = len_payload_recipe().parse(&[0x03, 0xAA]).unwrap_err();
    assert!(matches!(err, RecipeError::OutOfBounds));
}

#[test]
fn repeated_group_allows_empty_scope() {
    let recipe = DependentRecipe::new("empty_group").field(DependentField::scope(
        "payload",
        LengthExpr::fixed(0),
        vec![DependentField::repeated_group(
            "pairs",
            vec![DependentField::quic_varint("id"), DependentField::quic_varint("value")],
        )],
    ));

    let parsed = recipe.parse(&[]).unwrap();
    let payload = as_struct(as_struct(&parsed.value).get("payload").unwrap());
    assert!(as_array(payload.get("pairs").unwrap()).is_empty());
}

#[test]
fn repeated_group_rejects_truncated_item_inside_scope() {
    let recipe = DependentRecipe::new("bad_group").field(DependentField::scope(
        "payload",
        LengthExpr::fixed(1),
        vec![DependentField::repeated_group(
            "pairs",
            vec![DependentField::quic_varint("id"), DependentField::quic_varint("value")],
        )],
    ));

    assert!(matches!(recipe.parse(&[0x01]).unwrap_err(), RecipeError::OutOfBounds));
}

#[test]
fn scope_remaining_consumes_every_unparsed_byte() {
    let recipe = DependentRecipe::new("remaining").field(DependentField::scope(
        "payload",
        LengthExpr::Remaining,
        vec![DependentField::bytes("raw", LengthExpr::Remaining)],
    ));

    let parsed = recipe.parse(&[0xAA, 0xBB, 0xCC]).unwrap();
    let payload = as_struct(as_struct(&parsed.value).get("payload").unwrap());
    assert_eq!(payload.get("raw"), Some(&IngredientValue::Binary(vec![0xAA, 0xBB, 0xCC])));
    assert_eq!(parsed.bytes_consumed, 3);
}

#[test]
fn switch_selects_named_case_and_records_transition() {
    let parsed = switch_recipe_with_fallback().parse(&[0x02, 0xAA, 0xBB]).unwrap();
    let active = as_struct(as_struct(&parsed.value).get("active_schema").unwrap());
    assert_eq!(active.get("two"), Some(&IngredientValue::Binary(vec![0xAA, 0xBB])));
    assert_eq!(parsed.transitions.len(), 1);
    assert_eq!(parsed.transitions[0].schema_name, "two_bytes");
    assert_eq!(parsed.transitions[0].cursor_at_transition, 1);
}

#[test]
fn switch_uses_fallback_and_preserves_remaining_bytes() {
    let parsed = switch_recipe_with_fallback().parse(&[0x2A, 0xDE, 0xAD]).unwrap();
    let active = as_struct(as_struct(&parsed.value).get("active_schema").unwrap());
    assert_eq!(active.get("raw"), Some(&IngredientValue::Binary(vec![0xDE, 0xAD])));
    assert_eq!(parsed.transitions[0].schema_name, "fallback");
}

#[test]
fn switch_without_fallback_rejects_unknown_selector() {
    let cases = BTreeMap::new();
    let recipe = DependentRecipe::new("strict_switch")
        .field(DependentField::quic_varint("tag"))
        .field(DependentField::switch("active", "tag", cases));

    assert!(matches!(recipe.parse(&[0x09]).unwrap_err(), RecipeError::InvalidConfig(_)));
}

#[test]
fn sequence_until_end_accepts_empty_stream() {
    let parsed = tiny_stream_program().parse(&[]).unwrap();
    assert_eq!(parsed.bytes_consumed, 0);
    assert!(as_array(as_struct(&parsed.value).get("frames").unwrap()).is_empty());
}

#[test]
fn sequence_repeat_count_requires_enough_items() {
    let stream = RecipeProgram::Sequence(
        SequenceRecipe::new("two_frames", "frames", tiny_frame_program()).repeat_count(2),
    );
    assert!(matches!(stream.parse(&[0x01, 0xAA]).unwrap_err(), RecipeError::OutOfBounds));
}

#[test]
fn sequence_repeat_count_stops_after_exact_count() {
    let stream = RecipeProgram::Sequence(
        SequenceRecipe::new("two_frames", "frames", tiny_frame_program()).repeat_count(2),
    );
    let parsed = stream.parse(&[0x01, 0xAA, 0x01, 0xBB, 0x01, 0xCC]).unwrap();
    assert_eq!(parsed.bytes_consumed, 4);
    assert_eq!(parsed.frames.len(), 2);
}

#[test]
fn sequence_frame_receipts_track_many_byte_ranges() {
    let mut bytes = Vec::new();
    for index in 0..10u8 {
        bytes.push(0x01);
        bytes.push(index);
    }

    let parsed = tiny_stream_program().parse(&bytes).unwrap();
    assert_eq!(parsed.frames.len(), 10);
    for (index, frame) in parsed.frames.iter().enumerate() {
        assert_eq!(frame.stream_index_path, format!("frames[{index}]"));
        assert_eq!(frame.byte_start, index * 2);
        assert_eq!(frame.byte_end, index * 2 + 2);
    }
}

#[test]
fn lookup_rejects_wildcards_but_collect_accepts_them() {
    let parsed = tiny_stream_program().parse(&[0x01, 0xAA, 0x02, 0xBB, 0xCC]).unwrap();
    assert!(lookup_program_path(&parsed, "fixture", "fixture.tiny_stream.frames[*].len").is_err());

    let lengths = collect_program_path(&parsed, "fixture", "fixture.tiny_stream.frames[*].len").unwrap();
    assert_eq!(lengths.iter().map(|value| value_as_u64(value).unwrap()).collect::<Vec<_>>(), vec![1, 2]);
}

#[test]
fn lookup_rejects_out_of_range_indexes() {
    let parsed = tiny_stream_program().parse(&[0x01, 0xAA]).unwrap();
    assert!(matches!(
        lookup_program_path(&parsed, "fixture", "fixture.tiny_stream.frames[3].len").unwrap_err(),
        RecipeError::OutOfBounds
    ));
}

#[test]
fn collect_value_path_returns_every_nested_payload() {
    let parsed = tiny_stream_program().parse(&[0x01, 0xAA, 0x02, 0xBB, 0xCC]).unwrap();
    let payloads = collect_value_path(&parsed.value, "frames[*].payload").unwrap();
    assert_eq!(value_as_bytes(payloads[0]), Some(&[0xAA][..]));
    assert_eq!(value_as_bytes(payloads[1]), Some(&[0xBB, 0xCC][..]));
}

#[test]
fn malformed_value_path_is_rejected() {
    let parsed = tiny_stream_program().parse(&[0x01, 0xAA]).unwrap();
    assert!(matches!(
        lookup_value_path(&parsed.value, "frames[nope].len").unwrap_err(),
        RecipeError::InvalidConfig(_)
    ));
}

#[test]
fn wildcardize_indexes_handles_multiple_indexes_and_leaves_non_numeric_markers() {
    assert_eq!(
        wildcardize_indexes("root.items[12].children[7].name"),
        "root.items[*].children[*].name"
    );
    assert_eq!(wildcardize_indexes("root.items[*].name"), "root.items[*].name");
}

#[test]
fn path_manifest_for_tiny_stream_declares_repeated_paths() {
    let manifest = PathManifest::from_program("fixture", &tiny_stream_program());
    assert!(manifest.contains_path("fixture.tiny_stream.frames[*]"));
    assert!(manifest.contains_path("fixture.tiny_stream.frames[*].len"));
    assert!(manifest.contains_path("fixture.tiny_stream.frames[*].payload"));
}

#[test]
fn pathgen_export_for_tiny_stream_is_sorted_and_unique() {
    let manifest = PathManifest::from_program("fixture", &tiny_stream_program());
    let export = PathgenManifestV1::from_manifest(&manifest);
    assert!(export.const_names_are_unique());
    assert!(export.contains_path("fixture.tiny_stream.frames[*].len"));
    assert!(export.to_manifest_lines().contains("FIXTURE_TINY_STREAM_FRAMES_LEN"));
}

#[test]
fn transfer_plan_zero_length_item_has_no_packets_and_keeps_next_instruction_boundary() {
    let schedule = TransferPlan::new("zero", 4)
        .item("empty.bin", 0, "chunk")
        .after("footer")
        .plan()
        .unwrap();
    assert_eq!(schedule.items[0].packet_count, 0);
    assert_eq!(schedule.total_packets, 0);
    assert_eq!(schedule.next_instruction_seq, 0);
    assert!(schedule.packets.is_empty());
}

#[test]
fn transfer_plan_start_seq_offsets_all_packets_and_footer() {
    let schedule = TransferPlan::new("offset", 3)
        .start_seq(10)
        .item("a", 4, "chunk")
        .item("b", 2, "chunk")
        .after("footer")
        .plan()
        .unwrap();
    assert_eq!(schedule.items[0].packet_start(), 10);
    assert_eq!(schedule.items[0].packet_end_exclusive(), 12);
    assert_eq!(schedule.items[1].packet_start(), 12);
    assert_eq!(schedule.next_instruction_seq, 13);
    assert_eq!(schedule.to_stream_blueprint().entries[2].start_seq, 13);
}

#[test]
fn transfer_schedule_values_and_manifest_paths_stay_in_lockstep() {
    let schedule = TransferPlan::new("lockstep", 2)
        .item("a", 3, "chunk")
        .after("done")
        .plan()
        .unwrap();
    let value = schedule.to_value();
    let manifest = schedule.to_path_manifest("transfer");

    for path in [
        "transfer.lockstep.total_packets",
        "transfer.lockstep.next_instruction_seq",
        "transfer.lockstep.items[*].declared_len",
        "transfer.lockstep.packets[*].payload_len",
    ] {
        assert!(manifest.contains_path(path), "missing manifest path {path}");
    }
    assert_eq!(lookup_value_path(&value, "items[0].packet_count").unwrap(), &IngredientValue::U64(2));
    assert_eq!(lookup_value_path(&value, "packets[1].payload_len").unwrap(), &IngredientValue::U64(1));
}

#[test]
fn program_stream_orchestrator_respects_blueprint_ranges() {
    let schedule = TransferPlan::new("planned", 4)
        .start_seq(5)
        .item("a", 5, "tiny_frame")
        .after("footer_frame")
        .plan()
        .unwrap();
    let blueprint = schedule.to_stream_blueprint();
    let orchestrator = ProgramStreamOrchestrator::new(blueprint)
        .with_program("tiny_frame", tiny_frame_program())
        .with_program("footer_frame", tiny_frame_program());

    assert_eq!(orchestrator.get_program_name_for_packet(4), None);
    assert_eq!(orchestrator.get_program_name_for_packet(5), Some("tiny_frame".to_string()));
    assert_eq!(orchestrator.get_program_name_for_packet(6), Some("tiny_frame".to_string()));
    assert_eq!(orchestrator.get_program_name_for_packet(7), Some("footer_frame".to_string()));
}
