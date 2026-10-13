use recipe::{Ingredient, IngredientValue, ParserRegistry};
use hex_literal::hex;

fn main() {
    // Example Initial QUIC Long Header Packet
    let packet = hex!(
        "c300000001088394c8f03e5157080000449e00000002" // just enough for demo
    );

    // Define QUIC Long Header Ingredient
    let quic_packet = Ingredient::Struct {
        name: Some("quic_long_header".into()),
        offset: 0.into(),
        children: vec![
            Ingredient::u8_named("first_byte", 0),
            Ingredient::u32_be_named("version", 1),
            Ingredient::array_of_u8("dcid", 5, 8), // length = 8
            Ingredient::array_of_u8("scid", 13, 8), // length = 8
            // You can keep adding more fields like packet number, payload, etc.
        ],
    };

    // Parse it
    let registry = ParserRegistry::default();
    let result = registry.parse_ingredient(&packet, &quic_packet).unwrap();

    println!("Parsed QUIC Packet:\n{:#?}", result);
}
