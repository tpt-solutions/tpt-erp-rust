//! Compile-fail tests proving that cross-entity `Id` mixups do not compile.
//!
//! Run with `TRYBUILD=overwrite cargo test` after a deliberate change to refresh the
//! expected stderr snapshots, then review them before committing.

#[test]
fn id_mixup_compile_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail/*.rs");
}
