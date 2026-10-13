use std::collections::HashMap;

use recipe::{error::RecipeError, ingredient_value::IngredientValue};

use crate::{
    packet_plan::{PlanEntry, StreamBlueprint},
    path_manifest::{normalize_segment, Cardinality, PathManifest, PathSpec, ValueKind},
    path_manifest_export::PathgenManifestV1,
};

/// A declared transfer item whose byte length is known before its payload is
/// received.
///
/// This is the planning counterpart to dependent parsing: once a stream header
/// says "file_len = N", the runtime can determine how many payload packets are
/// expected before the next instruction/schema boundary is allowed to begin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferItem {
    pub name: String,
    pub declared_len: usize,
    pub chunk_recipe_name: String,
}

impl TransferItem {
    pub fn new(
        name: impl Into<String>,
        declared_len: usize,
        chunk_recipe_name: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            declared_len,
            chunk_recipe_name: chunk_recipe_name.into(),
        }
    }
}

/// Declarative packet-ingestion plan for one or more length-known payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub name: String,
    pub max_payload_per_packet: usize,
    pub start_seq: u64,
    pub items: Vec<TransferItem>,
    pub after_recipe_name: Option<String>,
}

impl TransferPlan {
    pub fn new(name: impl Into<String>, max_payload_per_packet: usize) -> Self {
        Self {
            name: name.into(),
            max_payload_per_packet,
            start_seq: 0,
            items: Vec::new(),
            after_recipe_name: None,
        }
    }

    pub fn start_seq(mut self, start_seq: u64) -> Self {
        self.start_seq = start_seq;
        self
    }

    pub fn item(
        mut self,
        name: impl Into<String>,
        declared_len: usize,
        chunk_recipe_name: impl Into<String>,
    ) -> Self {
        self.items
            .push(TransferItem::new(name, declared_len, chunk_recipe_name));
        self
    }

    pub fn after(mut self, recipe_name: impl Into<String>) -> Self {
        self.after_recipe_name = Some(recipe_name.into());
        self
    }

    pub fn plan(&self) -> Result<TransferSchedule, RecipeError> {
        if self.max_payload_per_packet == 0 {
            return Err(RecipeError::InvalidConfig(
                "max_payload_per_packet must be greater than zero".to_string(),
            ));
        }

        let mut seq = self.start_seq;
        let mut planned_items = Vec::new();
        let mut packets = Vec::new();
        let mut total_payload_len = 0usize;

        for item in &self.items {
            total_payload_len = total_payload_len
                .checked_add(item.declared_len)
                .ok_or_else(|| RecipeError::InvalidConfig("transfer byte length overflow".into()))?;

            let packet_count = packets_needed(item.declared_len, self.max_payload_per_packet)?;
            let item_start_seq = seq;
            let item_end_seq = if packet_count == 0 {
                None
            } else {
                Some(seq.checked_add(packet_count - 1).ok_or_else(|| {
                    RecipeError::InvalidConfig("transfer sequence number overflow".into())
                })?)
            };

            let mut item_offset = 0usize;
            for packet_index in 0..packet_count {
                let packet_seq = seq.checked_add(packet_index).ok_or_else(|| {
                    RecipeError::InvalidConfig("transfer sequence number overflow".into())
                })?;
                let end = item_offset
                    .checked_add(self.max_payload_per_packet)
                    .map(|candidate| candidate.min(item.declared_len))
                    .ok_or_else(|| RecipeError::InvalidConfig("packet range overflow".into()))?;

                packets.push(PlannedPacket {
                    item_name: item.name.clone(),
                    chunk_recipe_name: item.chunk_recipe_name.clone(),
                    seq: packet_seq,
                    item_byte_start: item_offset,
                    item_byte_end: end,
                    is_final_for_item: end == item.declared_len,
                });

                item_offset = end;
            }

            planned_items.push(PlannedTransferItem {
                name: item.name.clone(),
                declared_len: item.declared_len,
                chunk_recipe_name: item.chunk_recipe_name.clone(),
                start_seq: item_start_seq,
                end_seq: item_end_seq,
                packet_count,
            });

            seq = seq.checked_add(packet_count).ok_or_else(|| {
                RecipeError::InvalidConfig("transfer sequence number overflow".into())
            })?;
        }

        let total_packets = seq
            .checked_sub(self.start_seq)
            .ok_or_else(|| RecipeError::InvalidConfig("transfer sequence underflow".into()))?;

        Ok(TransferSchedule {
            name: self.name.clone(),
            max_payload_per_packet: self.max_payload_per_packet,
            start_seq: self.start_seq,
            total_payload_len,
            total_packets,
            next_instruction_seq: seq,
            items: planned_items,
            packets,
            after_recipe_name: self.after_recipe_name.clone(),
        })
    }
}

/// The concrete packet schedule produced from declared lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSchedule {
    pub name: String,
    pub max_payload_per_packet: usize,
    pub start_seq: u64,
    pub total_payload_len: usize,
    pub total_packets: u64,
    pub next_instruction_seq: u64,
    pub items: Vec<PlannedTransferItem>,
    pub packets: Vec<PlannedPacket>,
    pub after_recipe_name: Option<String>,
}

impl TransferSchedule {
    /// Convert a declared transfer schedule into the existing packet-plan layer.
    ///
    /// Every file gets a contiguous sequence range. If `after_recipe_name` is
    /// set, the next instruction schema begins exactly at `next_instruction_seq`.
    pub fn to_stream_blueprint(&self) -> StreamBlueprint {
        let mut entries = Vec::new();

        for item in &self.items {
            if let Some(end_seq) = item.end_seq {
                entries.push(PlanEntry {
                    recipe_name: item.chunk_recipe_name.clone(),
                    start_seq: item.start_seq,
                    end_seq: Some(end_seq),
                });
            }
        }

        if let Some(after_recipe_name) = &self.after_recipe_name {
            entries.push(PlanEntry {
                recipe_name: after_recipe_name.clone(),
                start_seq: self.next_instruction_seq,
                end_seq: None,
            });
        }

        StreamBlueprint {
            name: self.name.clone(),
            entries,
        }
    }

    /// Materialize the transfer schedule as an `IngredientValue` tree.
    ///
    /// This lets the same path lookup code used by parsed recipe streams assert
    /// transfer-planning paths such as `items[0].declared_len` or
    /// `packets[2].item_byte_end`. The schedule remains planning metadata; this
    /// does not imply any packet payload was received or written.
    pub fn to_value(&self) -> IngredientValue {
        let mut root = HashMap::new();
        root.insert("name".to_string(), IngredientValue::String(self.name.clone()));
        root.insert(
            "max_payload_per_packet".to_string(),
            usize_value(self.max_payload_per_packet),
        );
        root.insert("start_seq".to_string(), IngredientValue::U64(self.start_seq));
        root.insert(
            "total_payload_len".to_string(),
            usize_value(self.total_payload_len),
        );
        root.insert("total_packets".to_string(), IngredientValue::U64(self.total_packets));
        root.insert(
            "next_instruction_seq".to_string(),
            IngredientValue::U64(self.next_instruction_seq),
        );
        if let Some(after_recipe_name) = &self.after_recipe_name {
            root.insert(
                "after_recipe_name".to_string(),
                IngredientValue::String(after_recipe_name.clone()),
            );
        }

        root.insert(
            "items".to_string(),
            IngredientValue::Array(
                self.items
                    .iter()
                    .map(PlannedTransferItem::to_value)
                    .collect(),
            ),
        );
        root.insert(
            "packets".to_string(),
            IngredientValue::Array(self.packets.iter().map(PlannedPacket::to_value).collect()),
        );

        IngredientValue::Struct(root)
    }

    /// Export the transfer schedule's stable planning paths for pathgen.
    ///
    /// The manifest describes what a schedule can expose; concrete schedule
    /// values are available through [`Self::to_value`].
    pub fn to_path_manifest(&self, namespace: impl Into<String>) -> PathManifest {
        let namespace = namespace.into();
        let root = format!(
            "{}.{}",
            normalize_segment(&namespace),
            normalize_segment(&self.name)
        );
        let mut manifest = PathManifest::new(namespace, self.name.clone());

        add_schedule_path_specs(&mut manifest, &root);
        manifest
    }

    pub fn to_pathgen_export(&self, namespace: impl Into<String>) -> PathgenManifestV1 {
        PathgenManifestV1::from_manifest(&self.to_path_manifest(namespace))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTransferItem {
    pub name: String,
    pub declared_len: usize,
    pub chunk_recipe_name: String,
    pub start_seq: u64,
    pub end_seq: Option<u64>,
    pub packet_count: u64,
}


impl PlannedTransferItem {
    pub fn packet_start(&self) -> u64 {
        self.start_seq
    }

    pub fn packet_end_exclusive(&self) -> u64 {
        self.start_seq.saturating_add(self.packet_count)
    }

    fn to_value(&self) -> IngredientValue {
        let mut map = HashMap::new();
        map.insert("name".to_string(), IngredientValue::String(self.name.clone()));
        map.insert("declared_len".to_string(), usize_value(self.declared_len));
        map.insert(
            "chunk_recipe_name".to_string(),
            IngredientValue::String(self.chunk_recipe_name.clone()),
        );
        map.insert("start_seq".to_string(), IngredientValue::U64(self.start_seq));
        map.insert(
            "packet_start".to_string(),
            IngredientValue::U64(self.packet_start()),
        );
        map.insert(
            "packet_end_exclusive".to_string(),
            IngredientValue::U64(self.packet_end_exclusive()),
        );
        if let Some(end_seq) = self.end_seq {
            map.insert("end_seq".to_string(), IngredientValue::U64(end_seq));
        }
        map.insert("packet_count".to_string(), IngredientValue::U64(self.packet_count));
        IngredientValue::Struct(map)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPacket {
    pub item_name: String,
    pub chunk_recipe_name: String,
    pub seq: u64,
    /// Inclusive byte start within the item payload.
    pub item_byte_start: usize,
    /// Exclusive byte end within the item payload.
    pub item_byte_end: usize,
    pub is_final_for_item: bool,
}

impl PlannedPacket {
    pub fn payload_len(&self) -> usize {
        self.item_byte_end.saturating_sub(self.item_byte_start)
    }

    fn to_value(&self) -> IngredientValue {
        let mut map = HashMap::new();
        map.insert(
            "item_name".to_string(),
            IngredientValue::String(self.item_name.clone()),
        );
        map.insert(
            "chunk_recipe_name".to_string(),
            IngredientValue::String(self.chunk_recipe_name.clone()),
        );
        map.insert("seq".to_string(), IngredientValue::U64(self.seq));
        map.insert(
            "item_byte_start".to_string(),
            usize_value(self.item_byte_start),
        );
        map.insert("item_byte_end".to_string(), usize_value(self.item_byte_end));
        map.insert("payload_len".to_string(), usize_value(self.payload_len()));
        map.insert(
            "is_final_for_item".to_string(),
            IngredientValue::U8(u8::from(self.is_final_for_item)),
        );
        IngredientValue::Struct(map)
    }
}

fn usize_value(value: usize) -> IngredientValue {
    IngredientValue::U64(value as u64)
}

fn add_schedule_path_specs(manifest: &mut PathManifest, root: &str) {
    let schedule_schema = "transfer_schedule";

    manifest.add(PathSpec::new(
        root.to_string(),
        ValueKind::Struct,
        Cardinality::Single,
        schedule_schema,
    ));
    manifest.add(PathSpec::new(
        format!("{}.max_payload_per_packet", root),
        ValueKind::U64,
        Cardinality::Single,
        schedule_schema,
    ));
    manifest.add(PathSpec::new(
        format!("{}.start_seq", root),
        ValueKind::U64,
        Cardinality::Single,
        schedule_schema,
    ));
    manifest.add(PathSpec::new(
        format!("{}.total_payload_len", root),
        ValueKind::U64,
        Cardinality::Single,
        schedule_schema,
    ));
    manifest.add(PathSpec::new(
        format!("{}.total_packets", root),
        ValueKind::U64,
        Cardinality::Single,
        schedule_schema,
    ));
    manifest.add(PathSpec::new(
        format!("{}.next_instruction_seq", root),
        ValueKind::U64,
        Cardinality::Single,
        schedule_schema,
    ));
    manifest.add(PathSpec::new(
        format!("{}.after_recipe_name", root),
        ValueKind::String,
        Cardinality::Single,
        schedule_schema,
    ));

    let item_root = format!("{}.items[*]", root);
    manifest.add(PathSpec::new(
        item_root.clone(),
        ValueKind::Array,
        Cardinality::Repeated,
        "transfer_item",
    ));
    for (field, kind) in [
        ("name", ValueKind::String),
        ("declared_len", ValueKind::U64),
        ("chunk_recipe_name", ValueKind::String),
        ("start_seq", ValueKind::U64),
        ("packet_start", ValueKind::U64),
        ("packet_end_exclusive", ValueKind::U64),
        ("end_seq", ValueKind::U64),
        ("packet_count", ValueKind::U64),
    ] {
        manifest.add(PathSpec::new(
            format!("{}.{}", item_root, field),
            kind,
            Cardinality::Repeated,
            "transfer_item",
        ));
    }

    let packet_root = format!("{}.packets[*]", root);
    manifest.add(PathSpec::new(
        packet_root.clone(),
        ValueKind::Array,
        Cardinality::Repeated,
        "transfer_packet",
    ));
    for (field, kind) in [
        ("item_name", ValueKind::String),
        ("chunk_recipe_name", ValueKind::String),
        ("seq", ValueKind::U64),
        ("item_byte_start", ValueKind::U64),
        ("item_byte_end", ValueKind::U64),
        ("payload_len", ValueKind::U64),
        ("is_final_for_item", ValueKind::U8),
    ] {
        manifest.add(PathSpec::new(
            format!("{}.{}", packet_root, field),
            kind,
            Cardinality::Repeated,
            "transfer_packet",
        ));
    }
}

pub fn packets_needed(declared_len: usize, max_payload_per_packet: usize) -> Result<u64, RecipeError> {
    if max_payload_per_packet == 0 {
        return Err(RecipeError::InvalidConfig(
            "max_payload_per_packet must be greater than zero".to_string(),
        ));
    }

    let packets = declared_len
        .checked_add(max_payload_per_packet - 1)
        .ok_or_else(|| RecipeError::InvalidConfig("packet count overflow".into()))?
        / max_payload_per_packet;

    u64::try_from(packets)
        .map_err(|_| RecipeError::InvalidConfig("packet count does not fit in u64".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_declared_file_length_into_packet_ranges_and_next_instruction_boundary() {
        let schedule = TransferPlan::new("file_then_instruction", 512)
            .item("avatar.png", 1500, "file_chunk")
            .after("next_instruction")
            .plan()
            .unwrap();

        assert_eq!(schedule.total_packets, 3);
        assert_eq!(schedule.next_instruction_seq, 3);
        assert_eq!(schedule.packets[0].item_byte_start, 0);
        assert_eq!(schedule.packets[0].item_byte_end, 512);
        assert_eq!(schedule.packets[1].item_byte_start, 512);
        assert_eq!(schedule.packets[1].item_byte_end, 1024);
        assert_eq!(schedule.packets[2].item_byte_start, 1024);
        assert_eq!(schedule.packets[2].item_byte_end, 1500);
        assert!(schedule.packets[2].is_final_for_item);

        let blueprint = schedule.to_stream_blueprint();
        assert_eq!(blueprint.entries[0].recipe_name, "file_chunk");
        assert_eq!(blueprint.entries[0].start_seq, 0);
        assert_eq!(blueprint.entries[0].end_seq, Some(2));
        assert_eq!(blueprint.entries[1].recipe_name, "next_instruction");
        assert_eq!(blueprint.entries[1].start_seq, 3);
        assert_eq!(blueprint.entries[1].end_seq, None);
    }

    #[test]
    fn plans_multiple_files_contiguously() {
        let schedule = TransferPlan::new("multi_file_upload", 4)
            .start_seq(10)
            .item("a.bin", 5, "file_chunk")
            .item("b.bin", 9, "file_chunk")
            .after("manifest_footer")
            .plan()
            .unwrap();

        assert_eq!(schedule.items[0].start_seq, 10);
        assert_eq!(schedule.items[0].end_seq, Some(11));
        assert_eq!(schedule.items[0].packet_count, 2);
        assert_eq!(schedule.items[1].start_seq, 12);
        assert_eq!(schedule.items[1].end_seq, Some(14));
        assert_eq!(schedule.items[1].packet_count, 3);
        assert_eq!(schedule.next_instruction_seq, 15);
        assert_eq!(schedule.total_payload_len, 14);
    }


    #[test]
    fn exports_transfer_schedule_manifest_for_pathgen() {
        let schedule = TransferPlan::new("multi_file_then_instruction", 4)
            .item("a.bin", 5, "file_chunk")
            .item("b.bin", 9, "file_chunk")
            .after("next_instruction")
            .plan()
            .unwrap();

        let export = schedule.to_pathgen_export("file_transfer");
        assert!(export.const_names_are_unique());
        assert!(export.contains_path(
            "file_transfer.multi_file_then_instruction.items[*].declared_len"
        ));
        assert!(export.contains_path(
            "file_transfer.multi_file_then_instruction.packets[*].item_byte_end"
        ));
        assert!(export.to_manifest_lines().contains(
            "FILE_TRANSFER_MULTI_FILE_THEN_INSTRUCTION_NEXT_INSTRUCTION_SEQ"
        ));
    }

    #[test]
    fn materializes_transfer_schedule_values_for_path_lookup() {
        let schedule = TransferPlan::new("multi_file_then_instruction", 4)
            .item("a.bin", 5, "file_chunk")
            .item("b.bin", 9, "file_chunk")
            .after("next_instruction")
            .plan()
            .unwrap();
        let value = schedule.to_value();

        assert_eq!(
            crate::path_lookup::lookup_value_path(&value, "items[1].declared_len").unwrap(),
            &IngredientValue::U64(9)
        );
        assert_eq!(
            crate::path_lookup::lookup_value_path(&value, "packets[4].item_byte_end").unwrap(),
            &IngredientValue::U64(9)
        );
        assert_eq!(
            crate::path_lookup::lookup_value_path(&value, "next_instruction_seq").unwrap(),
            &IngredientValue::U64(5)
        );
    }

    #[test]
    fn rejects_zero_payload_budget() {
        let err = TransferPlan::new("bad", 0)
            .item("file", 10, "file_chunk")
            .plan()
            .unwrap_err();

        assert!(matches!(err, RecipeError::InvalidConfig(_)));
    }
}
