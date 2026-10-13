use recipe::{
    error::RecipeError, ingredient::Ingredient, ingredient_value::IngredientValue,
    parser_registry::ParserRegistry,
};

use crate::helpers::{ParsingContext, parse_labeled};

pub struct RecipeRunner {
    pub registry: ParserRegistry,
}

impl RecipeRunner {
    pub fn new(registry: ParserRegistry) -> Self {
        Self { registry }
    }

    /// The high-level entry point for parsing a full packet/message.
    pub fn run(&self, data: &[u8], blueprint: &Ingredient) -> Result<IngredientValue, RecipeError> {
        // 1. Initialize the jurisdictional context
        let mut ctx = ParsingContext::new(data.len());

        // 2. Delegate to the recursive labeling logic
        // This ensures boundaries and 'scopes' are respected automatically.
        parse_labeled(data, blueprint, &mut ctx, None)
    }
}
