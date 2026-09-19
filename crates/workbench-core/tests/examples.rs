use workbench_core::parse_config;

#[test]
fn shipped_examples_parse() {
    let examples = [
        (
            "rust-development",
            include_str!("../../../examples/rust-development.toml"),
        ),
        (
            "web-development",
            include_str!("../../../examples/web-development.toml"),
        ),
        (
            "study-research",
            include_str!("../../../examples/study-research.toml"),
        ),
        (
            "android-development",
            include_str!("../../../examples/android-development.toml"),
        ),
    ];

    for (name, text) in examples {
        parse_config(text, name).unwrap_or_else(|err| panic!("{name}: {err}"));
    }
}
