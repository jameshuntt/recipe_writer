use std::collections::HashMap;

use abut::{AbutCode, AbutError, frame::FramedReader};
use recipe::{ingredient::Ingredient, ingredient_value::IngredientValue, parser_registry::ParserRegistry, parsers::parse_table};

use crate::packet_plan::StreamOrchestrator;

pub fn process_abut_stream<R: std::io::Read>(
    reader: &mut FramedReader<R>,
    ingredient: &Ingredient,
    _registry: &ParserRegistry,
) -> Result<HashMap<String, IngredientValue>, AbutError> {
    let mut results = HashMap::new();
    let mut frame_buffer = Vec::new();
    let mut packet_count = 0;

//     pub fn run_planned_stream<R: std::io::Read>(
//         reader: &mut FramedReader<R>,
//         plan: &PacketPlan,
//         _registry: &ParserRegistry,
//     ) -> Result<HashMap<String, IngredientValue>, AbutError> {
//         let mut current_packet = 0;
//         let mut global_results = HashMap::new();
// 
//         while let Ok(_) = reader.recv_into(&mut frame_buffer) {
//             // Find the recipe for THIS specific sequence number
//             let entry = plan.entries.iter()
//                 .find(|e| current_packet >= e.start_seq && (e.end_seq.is_none() || current_packet < e.end_seq.unwrap()))
//                 .ok_or_else(|| AbutError::new(AbutCode::Io).ctx("No recipe for sequence"))?;
// 
//             let recipe = registry.get(&entry.recipe_name)?;
//             
//             // Use the sequence number as part of the key for organization
//             let prefix = format!("{}.{}.{}", plan.name, entry.recipe_name, current_packet);
//             
//             parse_table(&frame_buffer, recipe, &prefix, &mut global_results)?;
//             
//             current_packet += 1;
//         }
//         Ok(global_results)
//     }

    // ABUT Logic: Pull the next safe, length-prefixed frame
    while let Ok(_) = reader.recv_into(&mut frame_buffer) {
        let prefix = format!("frame_{}", packet_count);

        // RECIPE Logic: Lens the frame into structured data
        // We map AbutError and RecipeError together here
        parse_table(&frame_buffer, ingredient, &prefix,
            // registry,
            &mut results)
            .map_err(|e| AbutError::new(AbutCode::Io).ctx(format!("Recipe Error: {:?}", e)))?;

        packet_count += 1;
    }

    Ok(results)
}




pub mod dependent;
pub mod recipe_runner;
pub mod http3;
pub mod qpack;
pub mod qpack_table;
pub mod helpers;
pub mod huffman;
pub mod packet_plan;
pub mod transfer_plan;
pub mod program;
pub mod path_manifest;
pub mod path_manifest_export;
pub mod path_lookup;
pub mod fixture;


pub fn run_orchestrated_stream<R: std::io::Read>(
    reader: &mut FramedReader<R>,
    orchestrator: &mut StreamOrchestrator,
) -> Result<HashMap<String, IngredientValue>, AbutError> {
    let mut results = HashMap::new();
    let mut frame_buffer = Vec::new();
    let mut packet_counter: u64 = 0;

    // 1. Abut pulls the length-prefixed frame from the wire
    while reader.recv_into(&mut frame_buffer).is_ok() {
        let pn = packet_counter;
        
        // 2. Orchestrator uses its internal Blueprint and Registry to parse
        // We convert RecipeError to AbutError using your 'ctx' helper
        let value = orchestrator.ingest(pn, &frame_buffer)
            .map_err(|e| AbutError::new(abut::AbutCode::Io).ctx(format!("Recipe Error: {:?}", e)))?;

        // 3. Store result with a unique key
        let recipe_name = orchestrator.get_recipe_name_for_packet(pn).unwrap_or_default();
        let key = format!("{}_{}_{}", orchestrator.blueprint.name, recipe_name, pn);
        results.insert(key, value);

        packet_counter += 1;
        
        // 4. Update the tracker (optional: handle ACKs here)
        orchestrator.tracker.advance();
    }

    Ok(results)
}

// #[derive(Debug, Clone, Deserialize)]
// #[serde(tag = "type", rename_all = "lowercase")]
// pub enum IngredientKind {
//     /// Leaves (no children): "u8", "i32", "ipv4", "qpack_block", etc.
//     Primitive { 
//         format: String, 
//     },
//     /// A collection of named fields (Recursion)
//     Struct { 
//         fields: Vec<Ingredient>, 
//     },
//     /// Repeating elements: "u8" count 10, or "Packet" count 5
//     Array {
//         element: Box<Ingredient>,
//         count: usize,
//         stride: Option<usize>, // The "Multiplication" logic we discussed
//     },
// }
// 
// 








// #[derive(Debug, Clone, Deserialize)]
// #[serde(rename_all = "lowercase")]
// pub enum IngredientFormat {
//     U8,
//     U16,
//     I32,
//     F32,
//     F64,
//     Ipv4,
//     Struct,
//     Array,
//     #[serde(other)]
//     Custom(String), // fallback for unknown/custom formats
// }
// 
// 

