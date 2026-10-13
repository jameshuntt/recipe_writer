use std::collections::HashMap;
use recipe::{
    ingredient::Ingredient, 
    ingredient_value::IngredientValue, 
    traits::IngredientParser, 
    types::RecipeResult,
    error::RecipeError
};
use crate::helpers::read_quic_varint;

pub struct QpackHeaderBlockParser;

impl IngredientParser for QpackHeaderBlockParser {
    fn parse(
        &self,
        input: &[u8],
        ingredient: &Ingredient
    ) -> RecipeResult<IngredientValue> {
        let mut offset = ingredient.offset.0;
        let mut fields = HashMap::new();

        // 1. Map String errors to RecipeError
        let (insert_count, len1) = read_quic_varint(&input[offset..])
            .map_err(|e| RecipeError::InvalidConfig(e))?;
        offset += len1;

        let (delta_base, len2) = read_quic_varint(&input[offset..])
            .map_err(|e| RecipeError::InvalidConfig(e))?;
        offset += len2;

        fields.insert("required_insert_count".into(), IngredientValue::U64(insert_count));
        fields.insert("delta_base".into(), IngredientValue::U64(delta_base));

        let mut instructions = Vec::new();

        // 2. Decode Instructions into an Array of Structs
        while offset < input.len() {
            let b = input[offset];
            let mut inst_map = HashMap::new();

            if b & 0b1000_0000 != 0 {
                // Indexed Header Field
                let (index, len) = read_quic_varint(&input[offset..]) // Note: offset, not offset-1
                    .map_err(|e| RecipeError::InvalidConfig(e))?;
                offset += len;

                inst_map.insert("type".into(), IngredientValue::String("indexed".into()));
                inst_map.insert("index".into(), IngredientValue::U64(index & 0x7F)); // Masking the prefix
            } 
            else if b & 0b1100_0000 == 0b0100_0000 {
                // Literal With Name Reference
                let (name_index, len1) = read_quic_varint(&input[offset..])
                    .map_err(|e| RecipeError::InvalidConfig(e))?;
                offset += len1;

                let (value_len, len2) = read_quic_varint(&input[offset..])
                    .map_err(|e| RecipeError::InvalidConfig(e))?;
                offset += len2;

                let value = input.get(offset..offset + value_len as usize)
                    .ok_or(RecipeError::OutOfBounds)?;
                offset += value_len as usize;

                inst_map.insert("type".into(), IngredientValue::String("literal_with_name_ref".into()));
                inst_map.insert("name_index".into(), IngredientValue::U64(name_index & 0x3F));
                inst_map.insert("value".into(), IngredientValue::Binary(value.to_vec()));
            } 
            else if b & 0b1111_0000 == 0b0011_0000 {
                // Literal Without Name Reference
                offset += 1; // Skip opcode byte
                let (name_len, len1) = read_quic_varint(&input[offset..])
                    .map_err(|e| RecipeError::InvalidConfig(e))?;
                offset += len1;

                let name = input.get(offset..offset + name_len as usize)
                    .ok_or(RecipeError::OutOfBounds)?;
                offset += name_len as usize;

                let (value_len, len2) = read_quic_varint(&input[offset..])
                    .map_err(|e| RecipeError::InvalidConfig(e))?;
                offset += len2;

                let value = input.get(offset..offset + value_len as usize)
                    .ok_or(RecipeError::OutOfBounds)?;
                offset += value_len as usize;

                inst_map.insert("type".into(), IngredientValue::String("literal".into()));
                inst_map.insert("name".into(), IngredientValue::Binary(name.to_vec()));
                inst_map.insert("value".into(), IngredientValue::Binary(value.to_vec()));
            } 
            else {
                inst_map.insert("type".into(), IngredientValue::String("unknown".into()));
                inst_map.insert("opcode".into(), IngredientValue::U8(b));
                offset += 1;
            }
            
            instructions.push(IngredientValue::Struct(inst_map));
        }

        fields.insert("instructions".into(), IngredientValue::Array(instructions));

        Ok(IngredientValue::Struct(fields))
    }
}