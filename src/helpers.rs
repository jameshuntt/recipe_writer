use std::collections::HashMap;

use recipe::{
    error::RecipeError,
    ingredient::Ingredient,
    ingredient_value::IngredientValue,
    parsers::GLOBAL_REGISTRY,
    types::{Length, Offset},
};

// QUIC-style variable-length integer decoder
pub fn read_quic_varint(input: &[u8]) -> Result<(u64, usize), String> {
    if input.is_empty() {
        return Err("No bytes to read varint".into());
    }

    let first = input[0];
    let len = match first >> 6 {
        0b00 => 1,
        0b01 => 2,
        0b10 => 4,
        0b11 => 8,
        _ => unreachable!(),
    };

    if input.len() < len {
        return Err("Truncated varint".into());
    }

    let val = match len {
        1 => (first & 0b0011_1111) as u64,
        2 => (((first & 0b0011_1111) as u64) << 8) | input[1] as u64,
        4 => {
            ((first & 0b0011_1111) as u64) << 24
                | (input[1] as u64) << 16
                | (input[2] as u64) << 8
                | input[3] as u64
        }
        8 => {
            ((first & 0b0011_1111) as u64) << 56
                | (input[1] as u64) << 48
                | (input[2] as u64) << 40
                | (input[3] as u64) << 32
                | (input[4] as u64) << 24
                | (input[5] as u64) << 16
                | (input[6] as u64) << 8
                | input[7] as u64
        }
        _ => unreachable!(),
    };

    Ok((val, len))
}

pub fn read_quic_varint_simple(input: &[u8]) -> Result<(u64, usize), RecipeError> {
    if input.is_empty() { return Err(RecipeError::OutOfBounds); }

    let first = input[0];
    let (len, val_init) = match first >> 6 {
        0b00 => (1, (first & 0x3F) as u64),
        0b01 => (2, (first & 0x3F) as u64),
        0b10 => (4, (first & 0x3F) as u64),
        0b11 => (8, (first & 0x3F) as u64),
        _ => unreachable!(),
    };

    if input.len() < len { return Err(RecipeError::OutOfBounds); }

    let mut val = val_init;
    for i in 1..len {
        val = (val << 8) | input[i] as u64;
    }

    Ok((val, len))
}





pub fn u8_named(name: &str, offset: usize) -> Ingredient {
    recipe::ingredient_builder::IngredientBuilder::new()
        .name(name)
        .offset_bytes(offset)
        .format("u8")
        .length_bytes(1)
        .build()
}

pub fn match_tagged(target: &str, _variants: &[(u8, &str)]) -> Ingredient {
    // Variant metadata is kept at the recipe-authoring boundary for now.
    recipe::ingredient_builder::IngredientBuilder::new()
        .name(&format!("match_{}", target))
        .format("keyed_match")
        .build()
}

// 3. Fix the Schema: Add the borrow '&'
pub fn frame_schema() -> Ingredient {
    Ingredient {
        name: "frame".into(),
        offset: Offset(0),
        children: Some(vec![
            u8_named("frame_type", 0),
            match_tagged(
                "frame_type",
                &[ // Added borrow here
                    (0x00, "padding_frame"),
                    (0x01, "ping_frame"),
                    (0x02, "ack_frame"),
                    (0x06, "crypto_frame"),
                    (0x08, "stream_frame"),
                ]
            ),
        ]),
        ..Default::default()
    }
}

pub fn frame_schema2() -> Ingredient {
    Ingredient {
        name: "packet".into(),
        length: Length(100), // Our 'outer' boundary
        children: Some(vec![
            u8_named("count", 0),
            // This loop will use the 'inner_loop logic above
            Ingredient {
                name: "items".into(),
                num_elements: Some(5), // The "Main Count"
                child_size: Some(10),   // The "Multiplication" Stride
                children: Some(vec![
                    u8_named("id", 0),
                    u8_named("val", 1),
                ]),
                ..Default::default()
            }
        ]),
        ..Default::default()
    }
}


// 2. Fix the Context: Use consistent naming
pub struct ParsingContext {
    pub global_cursor: usize,       // Renamed from global_cursor to match your code
    pub limit: usize,        // Added limit field
    pub scopes: HashMap<String, (usize, usize)>,
}

impl ParsingContext {
    pub fn new(total_len: usize) -> Self {
        Self {
            global_cursor: 0,
            limit: total_len,
            scopes: HashMap::new(),
        }
    }

    pub fn push_scope(&mut self, label: &str, start: usize, len: usize) {
        self.scopes.insert(label.to_string(), (start, start + len));
    }

    pub fn bytes_remaining_in(&self, label: &str) -> Result<usize, RecipeError> {
        let (_, limit) = self.scopes.get(label).ok_or_else(|| {
            // Note: If RecipeError doesn't have Violation, use a generic error
            RecipeError::InvalidConfig(format!("Scope label '{}' not found", label))
        })?;
        Ok(limit.saturating_sub(self.global_cursor))
    }
}




pub fn parse_labeled(
    data: &[u8],
    ingredient: &Ingredient,
    ctx: &mut ParsingContext,
    parent_label: Option<&str>,
) -> Result<IngredientValue, RecipeError> {
    // 1. If this ingredient has its own label, register it as a new boundary
    let current_label = if !ingredient.name.is_empty() {
        ctx.push_scope(&ingredient.name, ctx.global_cursor, ingredient.length.0);
        Some(ingredient.name.as_str())
    } else {
        parent_label
    };

    // 2. The "Ack": Ensure we aren't exceeding the PARENT's limit
    if let Some(label) = parent_label {
        let remaining = ctx.bytes_remaining_in(label)?;
        if ingredient.length.0 > remaining {
            return Err(RecipeError::InvalidConfig(format!(
                "Truncation: Field '{}' needs {} bytes, but scope '{}' only has {} left",
                ingredient.name, ingredient.length.0, label, remaining
            )));
        }
    }

    // 3. Handle Children (Recursion)
    if let Some(ref children) = ingredient.children {
        let mut results = Vec::new();
        
        // This is where your 'Main Count' multiplication happens
        for child in children {
            let child_val = parse_labeled(data, child, ctx, current_label)?;
            results.push(child_val);
        }
        
        // 4. Handle Padding: check if we reached the end of the current label's limit
        if let Some(label) = current_label {
            let padding = ctx.bytes_remaining_in(label)?;
            if padding > 0 {
                // We handle the 'unused' bytes here
                ctx.global_cursor += padding; 
            }
        }
        
        Ok(IngredientValue::Array(results))
    } else {
        // Standard leaf-node parsing
        // let val = ingredient.parse(&data[ctx.global_cursor..ctx.global_cursor + ingredient.length.0])?;
        let val = GLOBAL_REGISTRY.parse_ingredient(&data[ctx.global_cursor..ctx.global_cursor + ingredient.length.0], ingredient)?;
        ctx.global_cursor += ingredient.length.0;
        Ok(val)
    }
}



pub fn parse_recursive(
    data: &[u8],
    ingredients: &[Ingredient],
    ctx: &mut ParsingContext,
) -> Result<Vec<IngredientValue>, RecipeError> {
    let mut results = Vec::new();

    'outer: for ingredient in ingredients {
        // If this ingredient is a named container, label the boundary
        if !ingredient.name.is_empty() && ingredient.children.is_some() {
            ctx.push_scope(&ingredient.name, ctx.global_cursor, ingredient.length.0);
        }

        let start = ctx.global_cursor;
        let len = ingredient.length.0;
        
        // 1. BOUNDARY CHECK: Truncation detection
        if start + len > ctx.limit {
            break 'outer; 
        }

        // 2. LOOP LOGIC: Handles "Main Count" and "Multiplication"
        if let Some(count) = ingredient.num_elements {
            let stride = ingredient.child_size.unwrap_or(0);
            let mut loop_items = Vec::new();
            
            for _ in 0..count {
                if ctx.global_cursor + stride > ctx.limit { break 'outer; }

                if let Some(ref children) = ingredient.children {
                    let child_res = parse_recursive(data, children, ctx)?;
                    loop_items.push(IngredientValue::Array(child_res));
                }
                ctx.global_cursor += stride;
            }
            results.extend(loop_items);
            continue 'outer;
        }

        // 3. LEAF LOGIC: Parse actual bytes
        // let val = ingredient.parse(&data[start..start + len])?;
        let val = GLOBAL_REGISTRY.parse_ingredient(&data[start..start + len], ingredient)?;

        ctx.global_cursor += len;
        results.push(val);
    }

    Ok(results)
}



