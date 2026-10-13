use crate::qpack_table::QPACK_TABLE;

#[derive(Debug)]
pub struct HuffmanNode {
    pub symbol: Option<u16>,
    pub left: Option<Box<HuffmanNode>>,
    pub right: Option<Box<HuffmanNode>>,
}

impl HuffmanNode {
    pub fn new() -> Self {
        HuffmanNode {
            symbol: None,
            left: None,
            right: None,
        }
    }

    pub fn with_symbol(symbol: u16) -> Self {
        HuffmanNode {
            symbol: Some(symbol),
            left: None,
            right: None,
        }
    }
}

pub struct HuffmanDecoder {
    root: Box<HuffmanNode>,
}
impl HuffmanDecoder {
    pub fn new_v2() -> Self {
        let mut root = Box::new(HuffmanNode::new());

        for &(sym, code, len) in crate::qpack_table::QPACK_TABLE.iter() {
            let mut node = &mut root;
            for i in (0..len).rev() {
                let bit = (code >> i) & 1;
                // Use a standard match to avoid multiple mutable borrows in some compiler versions
                if bit == 0 {
                    if node.left.is_none() {
                        node.left = Some(Box::new(HuffmanNode::new()));
                    }
                    node = node.left.as_mut().unwrap();
                } else {
                    if node.right.is_none() {
                        node.right = Some(Box::new(HuffmanNode::new()));
                    }
                    node = node.right.as_mut().unwrap();
                }
            }
            node.symbol = Some(sym);
        }
        HuffmanDecoder { root }
    }
}
impl HuffmanDecoder {
    pub fn new() -> Self {
        let mut root = Box::new(HuffmanNode::new());

        for &(sym, code, len) in QPACK_TABLE.iter() {
            let mut node = &mut root;
            for i in (0..len).rev() {
                let bit = (code >> i) & 1;
                node = if bit == 0 {
                    node.left
                        .get_or_insert_with(|| Box::new(HuffmanNode::new()))
                } else {
                    node.right
                        .get_or_insert_with(|| Box::new(HuffmanNode::new()))
                };
            }
            node.symbol = Some(sym);
        }

        HuffmanDecoder { root }
    }

    pub fn decode(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut result = Vec::new();
        let mut node = &*self.root;
        let mut current = 0u64;
        let mut nbits = 0;

        for &byte in data {
            current = (current << 8) | byte as u64;
            nbits += 8;

            while nbits > 0 {
                let bit = (current >> (nbits - 1)) & 1;
                nbits -= 1;

                node = if bit == 0 {
                    node.left
                        .as_ref()
                        .ok_or_else(|| "Invalid Huffman encoding".to_string())?
                } else {
                    node.right
                        .as_ref()
                        .ok_or_else(|| "Invalid Huffman encoding".to_string())?
                };

                if let Some(sym) = node.symbol {
                    // 256 is the EOS symbol in QPACK/HPACK.
                    // If we hit it, we stop decoding.
                    if sym == 256 {
                        return Ok(result);
                    }

                    // Since it's not 256, it's a valid byte (0-255).
                    // We cast to u8 so 'result' becomes Vec<u8>
                    result.push(sym as u8);
                    node = &*self.root;
                }
            }
        }

        Ok(result)
    }
}
