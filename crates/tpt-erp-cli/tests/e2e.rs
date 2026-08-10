//! End-to-end test for the `tpt plugin` CLI against the `pricing`
//! example plugin.
//!
//! This is `#[ignore]`d by default because it compiles a WebAssembly
//! guest (`wasm32-unknown-unknown` must be installed and crates fetched).
//! Run it on demand with:
//!
//! ```sh
//! cargo test -p tpt-erp-cli --test e2e -- --ignored
//! ```

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // tests/ lives in the crate; walk up to the workspace root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|p| p.join("Cargo.toml").exists() && p.join("crates").exists())
        .expect("workspace root")
        .to_path_buf()
}

fn tpt_bin() -> PathBuf {
    repo_root()
        .join("target")
        .join("debug")
        .join(if cfg!(windows) { "tpt.exe" } else { "tpt" })
}

#[test]
#[ignore = "compiles a wasm32-unknown-unknown guest; needs the target + network"]
fn build_validate_run_pricing_plugin() {
    let root = repo_root();
    let tpt = tpt_bin();
    assert!(tpt.exists(), "build the CLI first: cargo build -p tpt-erp-cli");

    let plugin = root.join("examples/plugins/pricing");
    let wasm = plugin.join("pricing.wasm");

    // Build + componentize.
    let status = Command::new(&tpt)
        .arg("plugin")
        .arg("build")
        .arg(&plugin)
        .current_dir(&plugin)
        .status()
        .expect("run tpt plugin build");
    assert!(status.success(), "tpt plugin build failed");
    assert!(wasm.exists(), "component not produced");

    // Validate against the contract.
    let out = Command::new(&tpt)
        .arg("plugin")
        .arg("validate")
        .arg(&wasm)
        .current_dir(&plugin)
        .output()
        .expect("run tpt plugin validate");
    assert!(
        out.status.success(),
        "validation failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Run with host data; expect the discount logic to fire.
    let out = Command::new(&tpt)
        .arg("plugin")
        .arg("run")
        .arg(&wasm)
        .arg(r#"{"account":"acc-1","amount":10000}"#)
        .arg("--data")
        .arg(plugin.join("data.json"))
        .arg("--tenant")
        .arg("acme")
        .current_dir(&plugin)
        .output()
        .expect("run tpt plugin run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "run failed: {}", stdout);
    assert!(
        stdout.contains("\"final_amount\":9000"),
        "unexpected plugin output: {stdout}"
    );
}
