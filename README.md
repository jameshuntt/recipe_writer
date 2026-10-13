# recipe_writer

The builder kit for [`recipe`](https://crates.io/crates/recipe). Where `recipe`
defines ingredients and parses one blueprint, `recipe_writer` composes
blueprints into things you can run and ship.

## What is in it

| module | what it does |
|---|---|
| `dependent` | dependent recipes: a `switch` reads a field it just parsed and picks the case that parses the rest |
| `recipe_runner`, `program` | run a recipe or a program of recipes over bytes and collect the results |
| `packet_plan`, `transfer_plan` | plan packets and transfers from a recipe, with frame receipts |
| `path_manifest`, `path_manifest_export`, `path_lookup` | manifests of the paths a recipe produces, exported and looked up by name |
| `http3`, `qpack`, `qpack_table`, `huffman` | HTTP/3 field decoding: the full QPACK static table and Huffman decoder |
| `fixture` | fixture runs for tests and examples |

Frames go out through [`abut`](https://crates.io/crates/abut) and the process
boundary is [`scope`](https://crates.io/crates/scope).

## Tests

```bash
cargo test
```

49 tests, including the dependent-program path hardening suite.

## License

MIT or Apache-2.0, at your option.
