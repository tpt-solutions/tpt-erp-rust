//! `tpt plugin` subcommands: new, build, validate, run.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};

use tpt_wasm::host::HostContext;
use tpt_wasm::{Money, PluginRuntime, RuntimeConfig};

/// The exact WIT contract a plugin must satisfy, embedded so the CLI can
/// scaffold it without locating the crate on disk.
const ERP_WIT: &str = include_str!("../../tpt-wasm/wit/erp.wit");

#[derive(Parser)]
pub(crate) struct PluginCommand {
    #[command(subcommand)]
    pub command: PluginSubcommand,
}

#[derive(Subcommand)]
pub(crate) enum PluginSubcommand {
    /// Scaffold a new computation-only plugin crate.
    New(NewArgs),
    /// Compile a plugin crate and componentize it against the contract.
    Build(BuildArgs),
    /// Validate a compiled `.wasm` satisfies the `plugin` world.
    Validate(ValidateArgs),
    /// Execute a plugin's `run` with a JSON input string.
    Run(RunArgs),
}

#[derive(Args)]
pub(crate) struct NewArgs {
    /// Directory name (and crate name) of the new plugin.
    name: PathBuf,
    /// Overwrite an existing directory.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub(crate) struct BuildArgs {
    /// Path to the plugin crate directory (defaults to `.`).
    path: Option<PathBuf>,
    /// Cargo target triple to build for (default: wasm32-unknown-unknown).
    #[arg(long, default_value = "wasm32-unknown-unknown")]
    target: String,
    /// Output component file (default: `<crate>/<crate>.wasm`).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct ValidateArgs {
    /// Compiled plugin component (`.wasm`).
    wasm: PathBuf,
}

#[derive(Args)]
pub(crate) struct RunArgs {
    /// Compiled plugin component (`.wasm`).
    wasm: PathBuf,
    /// JSON input string passed to `run`.
    input: String,
    /// JSON file mapping account ids / SKUs to data for the host context.
    #[arg(long)]
    data: Option<PathBuf>,
    /// Tenant label reported to the plugin via `current-tenant`.
    #[arg(long, default_value = "cli")]
    tenant: String,
}

pub(crate) fn run(cmd: PluginCommand) -> anyhow::Result<()> {
    match cmd.command {
        PluginSubcommand::New(args) => new(args),
        PluginSubcommand::Build(args) => build(args),
        PluginSubcommand::Validate(args) => validate(args),
        PluginSubcommand::Run(args) => run_plugin(args),
    }
}

/// A host context backed by in-memory maps, optionally loaded from JSON.
#[derive(Clone, Default)]
struct CliHost {
    balances: std::collections::HashMap<String, Money>,
    stock: std::collections::HashMap<String, u64>,
    tenant: String,
}

impl CliHost {
    fn from_json(path: &Path, tenant: String) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading host data {}", path.display()))?;
        let raw: serde_json::Value = serde_json::from_str(&text)?;
        let mut host = CliHost {
            tenant,
            ..Default::default()
        };
        if let Some(map) = raw.get("accounts").and_then(|v| v.as_object()) {
            for (k, v) in map {
                let major = v.get("major").and_then(|n| n.as_i64()).unwrap_or(0);
                let minor = v.get("minor").and_then(|n| n.as_i64()).unwrap_or(0);
                host.balances.insert(k.clone(), Money::new(major, minor));
            }
        }
        if let Some(map) = raw.get("stock").and_then(|v| v.as_object()) {
            for (k, v) in map {
                let q = v.as_u64().unwrap_or(0);
                host.stock.insert(k.clone(), q);
            }
        }
        Ok(host)
    }
}

impl HostContext for CliHost {
    fn account_balance(&self, account: &str) -> Option<Money> {
        self.balances.get(account).copied()
    }
    fn stock_level(&self, sku: &str) -> Option<u64> {
        self.stock.get(sku).copied()
    }
    fn current_tenant(&self) -> String {
        self.tenant.clone()
    }
    fn clone_box(&self) -> Box<dyn HostContext> {
        Box::new(self.clone())
    }
}

fn new(args: NewArgs) -> anyhow::Result<()> {
    let dir = &args.name;
    if dir.exists() && !args.force {
        bail!(
            "{} already exists (use --force to overwrite)",
            dir.display()
        );
    }
    let name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .context("plugin name must be a valid directory name")?;

    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::create_dir_all(dir.join("wit"))?;

    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.36"
serde_json = "1"
wee_alloc = "0.4"

[workspace]
"#,
        name = name
    );
    std::fs::write(dir.join("Cargo.toml"), cargo)?;

    std::fs::write(dir.join("wit/erp.wit"), ERP_WIT)?;

    std::fs::write(
        dir.join("src/lib.rs"),
        r#"// TPT ERP plugin — computation-only by contract.
//
// This guest imports only `erp` (read-only ERP data) and exports `run`.
// It has no access to files, sockets, or the host clock: the host never
// links WASI, so a plugin can only *compute*.

wit_bindgen::generate!({ world: "plugin" });

use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {
    fn run(input: String) -> Result<String, String> {
        // Example: echo the input back as structured JSON. Replace this
        // with real business logic — pricing, routing, QC, etc.
        let _: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let output = serde_json::json!({
            "received": input,
            "note": "computed by a TPT ERP plugin",
        });
        Ok(output.to_string())
    }
}

export!(Component);
"#,
    )?;

    std::fs::write(dir.join(".gitignore"), "/target\nCargo.lock\n*.wasm\n")?;

    println!(
        "Scaffolded plugin `{name}` at {dir}\n\nNext:\n  cd {name}\n  tpt plugin build\n  tpt plugin run {name}.wasm '\"hello\"'",
        name = name,
        dir = dir.display(),
    );
    Ok(())
}

fn build(args: BuildArgs) -> anyhow::Result<()> {
    let crate_dir = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let name = crate_dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().and_then(|s| s.to_str()).map(str::to_string))
        .or_else(|| {
            crate_dir
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .context("could not determine plugin crate name")?;

    println!("→ building {name} for {}", args.target);
    let status = std::process::Command::new("cargo")
        .args(["build", "--target", &args.target, "--release"])
        .current_dir(&crate_dir)
        .status()
        .context("spawning cargo (is the wasm target installed?)")?;
    if !status.success() {
        bail!("cargo build failed");
    }

    let core = crate_dir
        .join("target")
        .join(&args.target)
        .join("release")
        .join(format!("{name}.wasm"));
    let bytes =
        std::fs::read(&core).with_context(|| format!("reading built module {}", core.display()))?;

    println!(
        "→ componentizing {} (validating against WIT contract)",
        core.display()
    );
    let component = wit_component::ComponentEncoder::default()
        .module(&bytes)
        .and_then(|mut e| e.encode())
        .map_err(|e| anyhow::anyhow!("failed to componentize plugin: {e}"))?;

    let out = args
        .out
        .unwrap_or_else(|| crate_dir.join(format!("{name}.wasm")));
    std::fs::write(&out, &component)?;
    println!(
        "✓ wrote component {} ({} bytes)",
        out.display(),
        component.len()
    );
    Ok(())
}

fn load_runtime() -> anyhow::Result<PluginRuntime> {
    PluginRuntime::new(RuntimeConfig::default()).context("creating plugin runtime")
}

fn validate(args: ValidateArgs) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(&args.wasm).with_context(|| format!("reading {}", args.wasm.display()))?;
    let rt = load_runtime()?;
    match rt.load("validate", &bytes, Box::new(CliHost::default())) {
        Ok(_) => {
            println!(
                "✓ {} satisfies the tpt:erp `plugin` world",
                args.wasm.display()
            );
            Ok(())
        }
        Err(e) => bail!("{} is NOT a valid plugin: {e}", args.wasm.display()),
    }
}

fn run_plugin(args: RunArgs) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(&args.wasm).with_context(|| format!("reading {}", args.wasm.display()))?;
    let rt = load_runtime()?;
    let host = match &args.data {
        Some(p) => CliHost::from_json(p, args.tenant.clone())?,
        None => CliHost {
            tenant: args.tenant.clone(),
            ..Default::default()
        },
    };
    let mut plugin = rt
        .load("run", &bytes, Box::new(host))
        .map_err(|e| anyhow::anyhow!("{} is not a valid plugin: {e}", args.wasm.display()))?;
    let out = plugin
        .run(&args.input)
        .map_err(|e| anyhow::anyhow!("plugin run failed: {e}"))?;
    println!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_wit_declares_plugin_world() {
        assert!(ERP_WIT.contains("world plugin"));
        assert!(ERP_WIT.contains("interface erp"));
        assert!(ERP_WIT.contains("export run"));
    }

    #[test]
    fn cli_host_reads_from_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.json");
        std::fs::write(
            &path,
            r#"{"accounts":{"a1":{"major":3,"minor":500}},"stock":{"s1":9}}"#,
        )
        .unwrap();
        let host = CliHost::from_json(&path, "tenant-x".into()).unwrap();

        assert_eq!(host.current_tenant(), "tenant-x");
        let bal = host.account_balance("a1").unwrap();
        assert_eq!((bal.major, bal.minor), (3, 500));
        assert_eq!(host.stock_level("s1"), Some(9));
        // Unknown entity -> None, never a panic.
        assert_eq!(host.account_balance("ghost"), None);
    }

    #[test]
    fn cli_host_default_is_empty() {
        let host = CliHost::default();
        assert_eq!(host.current_tenant(), "");
        assert_eq!(host.account_balance("anything"), None);
    }
}
