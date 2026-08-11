//! `tpt plugin` subcommands: new, build, validate, run.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};

use tpt_erp_wasm::host::HostContext;
use tpt_erp_wasm::{Money, PluginRuntime, RuntimeConfig};

/// The exact WIT contract a plugin must satisfy, embedded so the CLI can
/// scaffold it without locating the crate on disk.
const ERP_WIT: &str = include_str!("../../tpt-erp-wasm/wit/erp.wit");

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
    /// Natural-language description of what the plugin should do. The CLI picks a
    /// domain-appropriate guest template (pricing/tax/qc/dispatch/inventory) so a
    /// developer can go from a sentence to a compiling plugin in one step.
    #[arg(long)]
    describe: Option<String>,
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

/// The default guest body used when `tpt plugin new` is given no `--describe`.
/// It echoes the input back as JSON — a minimal, compiling starting point.
const DEFAULT_GUEST: &str = r#"// TPT ERP plugin — computation-only by contract.
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
"#;

/// Pick a guest domain from a natural-language description by keyword match.
fn detect_domain(desc: &str) -> &'static str {
    let d = desc.to_ascii_lowercase();
    if d.contains("tax") || d.contains("vat") {
        "tax"
    } else if d.contains("price")
        || d.contains("discount")
        || d.contains("promo")
        || d.contains("pricing")
    {
        "pricing"
    } else if d.contains("qc") || d.contains("quality") || d.contains("inspect") {
        "qc"
    } else if d.contains("route")
        || d.contains("dispatch")
        || d.contains("deliver")
        || d.contains("fleet")
    {
        "dispatch"
    } else if d.contains("inventor")
        || d.contains("stock")
        || d.contains("warehouse")
        || d.contains("wms")
    {
        "inventory"
    } else {
        "generic"
    }
}

/// Build a domain-tailored guest `src/lib.rs` from a natural-language description.
///
/// The description selects one of the bundled templates; each template is a complete,
/// compiling plugin that reads ERP data through the `erp` host interface and returns a
/// structured JSON result. This is the "natural-language to plugin" on-ramp: a developer
/// describes intent in a sentence and gets a working starting point.
fn scaffold_guest(desc: &str) -> String {
    let domain = detect_domain(desc);
    let body = match domain {
        "pricing" => r#"
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let account = v.get("account").and_then(|x| x.as_str()).unwrap_or("");
        let amount = v.get("amount").and_then(|x| x.as_i64()).unwrap_or(0);
        // Balance-tiered discount: read the customer's loyalty balance from the host.
        let tier = match erp::get_account_balance(account) {
            Ok(bal) => {
                let points = bal.major;
                if points >= 1000 { 15 } else if points >= 500 { 10 } else if points >= 100 { 5 } else { 0 }
            }
            Err(_) => 0,
        };
        let discounted = amount - amount * tier / 100;
        let output = serde_json::json!({
            "account": account,
            "tier": tier,
            "final_amount": discounted,
        });
        Ok(output.to_string())"#,
        "tax" => r#"
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let amount = v.get("amount").and_then(|x| x.as_i64()).unwrap_or(0);
        let jurisdiction = v.get("jurisdiction").and_then(|x| x.as_str()).unwrap_or("");
        // Flat demo tier: EU/UK 20%, US 7%, else 0.
        let rate = match jurisdiction {
            "eu-standard" | "uk-standard" => 20,
            "us-standard" => 7,
            _ => 0,
        };
        let tax = amount * rate / 100;
        let output = serde_json::json!({
            "jurisdiction": jurisdiction,
            "estimated_tax": tax,
        });
        Ok(output.to_string())"#,
        "qc" => r#"
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let measurement = v.get("measurement").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let tolerance = v.get("tolerance").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let pass = (measurement - tolerance).abs() <= tolerance;
        let output = serde_json::json!({
            "measurement": measurement,
            "tolerance": tolerance,
            "pass": pass,
        });
        Ok(output.to_string())"#,
        "dispatch" => r#"
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let demand = v.get("demand").and_then(|x| x.as_u64()).unwrap_or(0);
        // Prioritize high-demand stops.
        let priority = if demand >= 100 { "high" } else if demand >= 20 { "medium" } else { "low" };
        let output = serde_json::json!({
            "demand": demand,
            "priority": priority,
        });
        Ok(output.to_string())"#,
        "inventory" => r#"
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let sku = v.get("sku").and_then(|x| x.as_str()).unwrap_or("");
        let on_hand = match erp::get_stock_level(sku) {
            Ok(q) => q,
            Err(_) => 0,
        };
        let reorder = on_hand < 50;
        let output = serde_json::json!({
            "sku": sku,
            "on_hand": on_hand,
            "reorder": reorder,
        });
        Ok(output.to_string())"#,
        _ => r#"
        let _: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let output = serde_json::json!({
            "received": input,
            "note": "auto-scaffolded from description; replace run() with real logic",
        });
        Ok(output.to_string())"#,
    };

    format!(
        r#"// TPT ERP plugin — computation-only by contract.
// Auto-scaffolded from description: "{desc}"
// Detected domain: {domain}
//
// This guest imports only `erp` (read-only ERP data) and exports `run`.
// It has no access to files, sockets, or the host clock: the host never
// links WASI, so a plugin can only *compute*.

wit_bindgen::generate!({{ world: "plugin" }});

use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {{
    fn run(input: String) -> Result<String, String> {{{body}
    }}
}}

export!(Component);
"#,
        desc = desc,
        domain = domain,
        body = body,
    )
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

    let guest = match &args.describe {
        Some(d) => scaffold_guest(d),
        None => DEFAULT_GUEST.to_string(),
    };
    std::fs::write(dir.join("src/lib.rs"), guest)?;

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
    match rt.load("validate", tenant_id("cli"), &bytes, Box::new(CliHost::default())) {
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

fn tenant_id(_tenant: &str) -> tpt_erp_wasm::TenantId {
    tpt_erp_wasm::TenantId::new()
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
        .load("run", tenant_id(&args.tenant), &bytes, Box::new(host))
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

    #[test]
    fn detect_domain_picks_by_keyword() {
        assert_eq!(detect_domain("compute a volume discount"), "pricing");
        assert_eq!(detect_domain("EU VAT tax tier"), "tax");
        assert_eq!(detect_domain("QC tolerance check"), "qc");
        assert_eq!(detect_domain("fleet dispatch priority"), "dispatch");
        assert_eq!(detect_domain("warehouse stock reorder"), "inventory");
        assert_eq!(detect_domain("do some arbitrary thing"), "generic");
    }

    #[test]
    fn scaffold_guest_is_a_compiling_plugin_shape() {
        // Every template must produce a complete `Guest` impl with a `run` export.
        for desc in [
            "volume discount pricing",
            "tax vat calculation",
            "quality qc inspection",
            "route dispatch optimization",
            "inventory stock level",
            "mystery plugin",
        ] {
            let src = scaffold_guest(desc);
            assert!(src.contains("impl Guest for Component"), "no Guest impl for {desc}");
            assert!(src.contains("fn run(input: String)"), "no run for {desc}");
            assert!(src.contains("export!(Component)"), "no export for {desc}");
            assert!(src.contains("wit_bindgen::generate"), "no bindings for {desc}");
        }
    }

    #[test]
    fn scaffold_guest_embeds_domain_logic() {
        let pricing = scaffold_guest("loyalty pricing discount");
        assert!(pricing.contains("final_amount"));
        assert!(pricing.contains("get_account_balance"));

        let tax = scaffold_guest("sales tax tier");
        assert!(tax.contains("estimated_tax"));

        let qc = scaffold_guest("qc tolerance");
        assert!(qc.contains("pass"));

        let dispatch = scaffold_guest("dispatch priority");
        assert!(dispatch.contains("priority"));

        let inventory = scaffold_guest("warehouse stock");
        assert!(inventory.contains("reorder"));
    }

    #[test]
    fn scaffold_guest_records_description_and_domain() {
        let src = scaffold_guest("compute a tax");
        assert!(src.contains("Auto-scaffolded from description: \"compute a tax\""));
        assert!(src.contains("Detected domain: tax"));
    }
}
