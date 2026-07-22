//! The macro's compile-error contract: each deliberate `syn::Error` the
//! `#[server]` macro emits stays an error with a stable, useful message.
//! Expected outputs live in `tests/ui/*.stderr` (regenerate with
//! `TRYBUILD=overwrite cargo test -p turbomcp --test macro_compile_fail`).

#[test]
fn macro_misuse_fails_to_compile() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
