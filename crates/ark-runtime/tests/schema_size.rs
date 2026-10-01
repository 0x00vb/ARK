//! Prints the size of every meta-tool schema. `cargo test -p ark-runtime --test schema_size -- --nocapture`
use ark_model::estimate_tokens;
use ark_runtime::catalog::{meta_tool_defs, MetaToolConfig};

#[test]
fn meta_tool_schema_sizes() {
    let all = meta_tool_defs(MetaToolConfig {
        knowledge: true,
        acquisition: true,
        present: true,
    });
    let mut total = 0;
    for t in &all {
        let n = estimate_tokens(&t.name)
            + estimate_tokens(&t.description)
            + estimate_tokens(&t.parameters.to_string());
        total += n;
        println!(
            "{:<20} {:>4} (desc {:>3}, params {:>3})",
            t.name,
            n,
            estimate_tokens(&t.description),
            estimate_tokens(&t.parameters.to_string())
        );
    }
    println!("total {total}");
}
