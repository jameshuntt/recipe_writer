use std::collections::HashMap;

use recipe::{
    error::RecipeError,
    ingredient::Ingredient,
    ingredient_value::IngredientValue,
    traits::IngredientParser,
    types::RecipeResult,
};

use crate::helpers::read_quic_varint_simple;

pub struct Http3FrameParser;


impl IngredientParser for Http3FrameParser {
    fn parse(&self, input: &[u8], ingredient: &Ingredient) -> RecipeResult<IngredientValue> {
        let offset = ingredient.offset.0;
        
        // Safety check for header size (Type + 2-byte Len)
        if input.len() < offset + 3 {
            return Err(recipe::error::RecipeError::OutOfBounds);
        }

        let frame_type = input[offset];
        let payload_len = u16::from_be_bytes([input[offset + 1], input[offset + 2]]) as usize;
        
        let payload_start = offset + 3;
        if input.len() < payload_start + payload_len {
            return Err(recipe::error::RecipeError::OutOfBounds);
        }

        match frame_type {
            0x00 => {
                let mut map = HashMap::new();
                map.insert("type".to_string(), IngredientValue::String("DATA".into()));
                Ok(IngredientValue::Struct(map))
            },
            0x01 => Ok(IngredientValue::String("HEADERS".into())),
            _ => Ok(IngredientValue::String("UNKNOWN".into())),
        }
    }
}
// impl IngredientParser for Http3FrameParser {
//     fn parse(&self, input: &[u8], ingredient: &Ingredient) -> RecipeResult<IngredientValue> {
//         let mut offset = ingredient.offset.0;
// 
//         let frame_type = input[offset];
//         offset += 1;
// 
//         let payload_len = u16::from_be_bytes([input[offset], input[offset + 1]]);
//         offset += 2;
// 
//         let payload = &input[offset..offset + payload_len as usize];
// 
//         let parsed = match frame_type {
//             0x00 => {
//                 IngredientValue::Struct(vec![("type".into(), IngredientValue::Str("DATA".into()))])
//             }
//             0x01 => IngredientValue::String("HEADERS".into()), // You’d parse this further with qpack later
//             _ => IngredientValue::String("UNKNOWN".into()),
//         };
// 
//         Ok(parsed)
//     }
// }
impl Http3FrameParser {
    pub fn name(&self) -> &'static str {
        "http3_frame"
    }
}
impl Http3FrameParser {
    pub fn parse2(&self, data: &[u8], ingredient: &Ingredient) -> RecipeResult<IngredientValue> {
        // 1. Get current position from the ingredient/context
        let mut cursor = ingredient.offset.0;

        // 2. Use your existing Varint logic (Dynamic length decoding)
        let (frame_type, type_len) = read_quic_varint_simple(&data[cursor..])?;
        cursor += type_len;

        let (payload_len, len_len) = read_quic_varint_simple(&data[cursor..])?;
        cursor += len_len;

        // 3. Boundary check
        let payload_end = cursor + (payload_len as usize);
        if data.len() < payload_end {
            return Err(RecipeError::OutOfBounds);
        }

        let payload_slice = &data[cursor..payload_end];

        // 4. Jurisdictional Matching
        match frame_type {
            0x00 => Ok(IngredientValue::Binary(payload_slice.to_vec())),
            0x01 => Ok(IngredientValue::String("HEADERS".into())),
            _ => Ok(IngredientValue::String(format!("UNKNOWN_TYPE_{}", frame_type))),
        }
    }
}