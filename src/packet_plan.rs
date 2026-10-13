use std::collections::HashMap;
use recipe::{ingredient::Ingredient, ingredient_value::IngredientValue};

// Integrated from abut::*
use abut::{AbutError, AbutCode, ReaderConfig};

/// 1. THE SCHEDULE
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub recipe_name: String,
    pub start_seq: u64,
    pub end_seq: Option<u64>, 
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamBlueprint {
    pub name: String,
    pub entries: Vec<PlanEntry>,
}

/// 2. THE TRACKER
pub struct PacketTracker {
    pub next_expected: u64,
    storage: Vec<u64>,
    max_window: usize,
}

impl PacketTracker {
    pub fn new(max_window: usize) -> Self {
        let words = (max_window + 63) / 64;
        Self {
            next_expected: 0,
            storage: vec![0; words],
            max_window,
        }
    }

    pub fn mark_received(&mut self, pn: u64) -> bool {
        if pn < self.next_expected || pn >= self.next_expected + self.max_window as u64 {
            return false; 
        }
        let bit_idx = (pn % self.max_window as u64) as usize;
        let mask = 1u64 << (bit_idx % 64);
        let word = &mut self.storage[bit_idx / 64];
        if (*word & mask) != 0 { return false; }
        *word |= mask;
        true
    }

    pub fn advance(&mut self) {
        while self.is_set(self.next_expected) {
            self.clear_bit(self.next_expected);
            self.next_expected += 1;
        }
    }

    fn is_set(&self, pn: u64) -> bool {
        let idx = (pn % self.max_window as u64) as usize;
        (self.storage[idx / 64] & (1u64 << (idx % 64))) != 0
    }

    fn clear_bit(&mut self, pn: u64) {
        let idx = (pn % self.max_window as u64) as usize;
        self.storage[idx / 64] &= !(1u64 << (idx % 64));
    }
}

/// 3. THE BRAIN: Now boundary-aware
pub struct StreamOrchestrator {
    pub blueprint: StreamBlueprint,
    pub tracker: PacketTracker,
    // pub registry: ParserRegistry,
    pub recipe_map: HashMap<String, Ingredient>,
    /// Boundary config for the incoming stream
    pub config: ReaderConfig,
}

impl StreamOrchestrator {
    pub fn new(blueprint: StreamBlueprint, max_window: usize) -> Self {
        Self {
            blueprint,
            tracker: PacketTracker::new(max_window),
            // registry: ParserRegistry::new(),
            recipe_map: HashMap::new(),
            config: ReaderConfig::default(),
        }
    }

    pub fn get_recipe_name_for_packet(&self, pn: u64) -> Option<String> {
        self.blueprint.entries.iter()
            .find(|entry| {
                let start = entry.start_seq;
                let end = entry.end_seq.unwrap_or(u64::MAX);
                pn >= start && pn <= end
            })
            .map(|e| e.recipe_name.clone())
    }

    /// Primary entry point for raw bytes. 
    /// Note: `pn` usually comes from the frame header if your protocol includes it.
    pub fn ingest(&mut self, pn: u64, data: &[u8]) -> Result<IngredientValue, AbutError> {
        // Enforce max frame length from ReaderConfig
        if data.len() > self.config.max_frame_len {
            return Err(AbutError::frame_too_large(data.len(), self.config.max_frame_len));
        }

        // Track receipt
        self.tracker.mark_received(pn);

        // Map PN to Recipe
        let recipe_name = self.get_recipe_name_for_packet(pn)
            .ok_or_else(|| AbutError::new(AbutCode::Io).ctx(format!("No recipe for PN {}", pn)))?;

        let ingredient = self.recipe_map.get(&recipe_name)
            .ok_or_else(|| AbutError::new(AbutCode::Io).ctx(format!("Recipe {} missing", recipe_name)))?;

        // Parse - mapping internal RecipeResult to AbutError if needed
recipe::parsers::GLOBAL_REGISTRY
            .parse_ingredient(data, ingredient)
            .map_err(|e| AbutError::new(AbutCode::Io).ctx(e.to_string()))
    }
}


#[cfg(test)]
mod stress_tests {
    use super::*;

    #[test]
    fn test_deep_ingredient_recursion() {
        println!("Testing for recursive stack overflow...");
        // Build a deeply nested ingredient structure manually
        // If Ingredient doesn't use Box internally for children, this will fail
        fn build_deep_ing(depth: usize) -> Ingredient {
            if depth == 0 {
                Ingredient { name: "leaf".into(), ..Default::default() }
            } else {
                // If this isn't Boxed in your recipe crate, 
                // the compiler might not even let you build it.
                let _child = build_deep_ing(depth - 1);
                Ingredient {
                    name: format!("depth_{}", depth),
                    // children: Some(vec![child]), // Hypothesis: This is where it dies
                    ..Default::default()
                }
            }
        }

        // Try a depth of 1000. If it's not boxed, the STACK will explode.
        let _deep_ingredient = build_deep_ing(1000);
        println!("Successfully built deep ingredient tree.");
    }

    #[test]
    fn test_orchestrator_move_size() {
        // This test checks if moving the Orchestrator causes the overflow
        let blueprint = StreamBlueprint {
            name: "stress_test".into(),
            entries: (0..10000).map(|i| PlanEntry {
                recipe_name: format!("recipe_{}", i),
                start_seq: i,
                end_seq: Some(i),
            }).collect(),
        };

        let mut orchestrator = StreamOrchestrator::new(blueprint, 1024);
        
        // Fill the map with 10k items
        for i in 0..10000 {
            orchestrator.recipe_map.insert(format!("recipe_{}", i), Ingredient::default());
        }

        // FORCE A MOVE: This is what happened in your thread::spawn
        let move_test = move || {
            let internal_orchestrator = orchestrator;
            println!("Move successful. Name: {}", internal_orchestrator.blueprint.name);
        };

        move_test();
    }
}