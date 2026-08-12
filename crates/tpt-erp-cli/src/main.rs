//! `tpt` — the TPT ERP command-line interface.
//!
//! Currently exposes the `tpt plugin` family of commands for working with
//! the WIT-based WebAssembly plugin runtime ([`tpt_erp_wasm`]):
//!
//! - `tpt plugin new <name>` — scaffold a new computation-only plugin.
//! - `tpt plugin build <path>` — compile + componentize a plugin.
//! - `tpt plugin validate <wasm>` — confirm it satisfies the contract.
//! - `tpt plugin run <wasm> <input>` — execute a plugin's `run`.
//! - `tpt seed-demo` — generate sample customers/products/orders for trying the platform.

mod plugin;
mod seed;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tpt", version, about = "TPT ERP command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Work with TPT ERP WebAssembly plugins.
    Plugin(plugin::PluginCommand),
    /// Generate a small sample dataset for evaluating the platform.
    SeedDemo(seed::SeedCommand),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Plugin(cmd) => plugin::run(cmd),
        Command::SeedDemo(cmd) => seed::run(cmd),
    }
}
