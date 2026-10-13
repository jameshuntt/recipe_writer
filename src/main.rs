use std::thread;
use std::collections::HashMap;
use std::time::Duration;
use std::io::Cursor;
use std::sync::LazyLock;

use abut::FrameSink;
use abut::frame::{FramedReader, FramedWriter};
use abut::ring::HeaplessRing;
use recipe_writer::packet_plan::{StreamOrchestrator, StreamBlueprint, PlanEntry, PacketTracker};
use recipe::{
    ingredient::Ingredient,
    ingredient_value::IngredientValue, 
    types::{Length, Offset},
    parser_registry::ParserRegistry,
};

// GLOBAL REGISTRY: The "Permanent Power Grid" we discussed.
// This ensures that no matter how many threads you spawn, they all
// share the same heap-allocated parser rules.
pub static GLOBAL_REGISTRY: LazyLock<ParserRegistry> = LazyLock::new(|| {
    // Logic: The Registry is now initialized once here.
    ParserRegistry::new()
});

const MAX_PACKET_SIZE: usize = 64 * 1024;
const QUEUE_DEPTH: usize = 128;

struct RecipeResources {
    orchestrator: Box<StreamOrchestrator>,
    results: HashMap<String, IngredientValue>,
    packet_count: u64,
}

pub fn start_parallel_processor<R: std::io::Read + Send + 'static>(
    mut reader: FramedReader<R>,
    orchestrator: Box<StreamOrchestrator>,
) {
    // 1. Setup the Ring Buffer in the Heap (Leaked for static lifetime across threads)
    let ring = Box::leak(Box::new(HeaplessRing::<MAX_PACKET_SIZE, QUEUE_DEPTH>::new()));
    let (mut producer, mut consumer) = ring.split();

    // 2. Ingestor Thread: Moves raw bytes into the Ring
    thread::spawn(move || {
        let mut tmp_buf = Vec::with_capacity(MAX_PACKET_SIZE);
        while reader.recv_into(&mut tmp_buf).is_ok() {
            // Spin-lock on the producer if the ring is full
            while let Err(_) = producer.send_frame(&tmp_buf) {
                thread::yield_now();
            }
        }
    });

    // 3. Processor Thread: The heavy lifter
    let builder = thread::Builder::new()
        .name("processor".into())
        .stack_size(2 * 1024 * 1024); // 2MB is plenty now that we're using pointers!

    builder.spawn(move || {
        let mut resources = RecipeResources {
            orchestrator, // Moving the pointer (8 bytes)
            results: HashMap::new(),
            packet_count: 0,
        };

        loop {
            if let Some(frame) = consumer.pop() {
                let pn = resources.packet_count;
                resources.packet_count += 1;

                // The Orchestrator now uses GLOBAL_REGISTRY internally via your Ingredient::parse fix
                match resources.orchestrator.ingest(pn, &frame) {
                    Ok(val) => {
                        if let Some(name) = resources.orchestrator.get_recipe_name_for_packet(pn) {
                            println!("[PN {}] Parsed {}: {:?}", pn, name, val);
                            resources.results.insert(format!("{}_{}", name, pn), val);
                        }
                    }
                    Err(e) => eprintln!("[Error] PN {}: {:?}", pn, e),
                }
            } else {
                // If the ring is empty, take a breath.
                thread::sleep(Duration::from_micros(500));
            }
        }
    }).expect("Failed to spawn processor thread");
}

fn main() {
    println!("--- Initializing Safe Production Environment ---");

    // Build the Blueprint
    let blueprint = StreamBlueprint {
        name: "test_stream".to_string(),
        entries: vec![
            PlanEntry { 
                recipe_name: "http3_header".to_string(), 
                start_seq: 0, 
                end_seq: None // Keep parsing forever
            },
        ],
    };

    // Setup the Orchestrator with its own HashMap of Ingredients
    let mut recipe_map = HashMap::new();
    recipe_map.insert("http3_header".to_string(), Ingredient {
        name: "frame_type".to_string(),
        offset: Offset(0),
        length: Length(1),
        format: "u8".to_string(),
        ..Default::default()
    });

    let orchestrator = Box::new(StreamOrchestrator {
        blueprint,
        tracker: PacketTracker::new(1024),
        // registry: GLOBAL_REGISTRY, // Local copy is fine, but it calls GLOBAL_REGISTRY
        recipe_map,
        config: abut::ReaderConfig::default(),
    });

    // Mock some data [0x01, 0x02, 0x03] representing 3 packets
    let mut transport_buffer = Vec::new();
    {
        let mut writer = FramedWriter::new(&mut transport_buffer);
        writer.write_frame(&[0x01]).unwrap();
        writer.write_frame(&[0x02]).unwrap();
        writer.write_frame(&[0x03]).unwrap();
    }
    
    let reader = FramedReader::new(Cursor::new(transport_buffer));

    println!("Engine starting...");
    start_parallel_processor(reader, orchestrator);

    // Keep the main thread alive for the test
    thread::sleep(Duration::from_secs(1));
    println!("Process complete.");
}



// fn main() {
//     let builder = thread::Builder::new()
//         .name("main_executor".into())
//         .stack_size(4 * 1024 * 1024); // 4MB for the setup

//     let handler = builder.spawn(|| {
//         run_engine(); // Move your current main logic here
//     }).unwrap();

//     handler.join().unwrap();
// }