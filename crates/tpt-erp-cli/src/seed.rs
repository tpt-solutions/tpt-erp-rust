//! `tpt seed-demo` — generate a small, deterministic sample dataset so evaluators can
//! try the platform without hand-authoring data. Writes JSON files (customers, products,
//! orders, and a cross-vertical summary) into an output directory.

use anyhow::Context;
use clap::Parser;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
pub(crate) struct SeedCommand {
    /// Output directory for the generated JSON files.
    #[arg(long, default_value = "./demo-data")]
    out: PathBuf,
    /// Number of customers / products / orders to generate.
    #[arg(long, default_value_t = 20)]
    count: u32,
    /// Tenant slug the demo data is scoped to.
    #[arg(long, default_value = "demo")]
    tenant: String,
}

#[derive(Serialize)]
struct Customer {
    id: String,
    tenant: String,
    name: String,
    email: String,
}

#[derive(Serialize)]
struct Product {
    id: String,
    tenant: String,
    sku: String,
    name: String,
    price_cents: u64,
}

#[derive(Serialize)]
struct OrderLine {
    product_id: String,
    qty: u32,
    price_cents: u64,
}

#[derive(Serialize)]
struct Order {
    id: String,
    tenant: String,
    customer_id: String,
    lines: Vec<OrderLine>,
    status: String,
}

const FIRST: &[&str] = &["Ada", "Grace", "Linus", "Margaret", "Alan", "Katherine", "Dennis", "Barbara", "Ken", "Radia"];
const LAST: &[&str] = &["Lovelace", "Hopper", "Torvalds", "Hamilton", "Turing", "Johnson", "Ritchie", "Liskov", "Thompson", "Bentley"];

fn product_name(i: u32) -> &'static str {
    const NAMES: &[&str] = &[
        "Widget", "Gadget", "Sprocket", "Bracket", "Coupler", "Valve", "Sensor", "Actuator",
        "Panel", "Cable", "Fastener", "Manifold", "Bearing", "Filter", "Nozzle", "Relay",
        "Housing", "Gasket", "Spindle", "Conduit",
    ];
    NAMES[(i as usize) % NAMES.len()]
}

pub(crate) fn run(cmd: SeedCommand) -> anyhow::Result<()> {
    fs::create_dir_all(&cmd.out)
        .with_context(|| format!("creating output dir {}", cmd.out.display()))?;

    let mut customers = Vec::new();
    let mut products = Vec::new();
    let mut orders = Vec::new();

    for i in 0..cmd.count {
        let first = FIRST[(i as usize) % FIRST.len()];
        let last = LAST[(i as usize) % LAST.len()];
        let cid = format!("cus_{i:04}");
        customers.push(Customer {
            id: cid.clone(),
            tenant: cmd.tenant.clone(),
            name: format!("{first} {last}"),
            email: format!("{}.{}@example.com", first.to_lowercase(), last.to_lowercase()),
        });

        let pid = format!("sku_{i:04}");
        let price: u64 = 1000 + (i as u64 * 137) % 9000;
        products.push(Product {
            id: pid.clone(),
            tenant: cmd.tenant.clone(),
            sku: pid.clone(),
            name: product_name(i).to_string(),
            price_cents: price,
        });

        // Each customer gets one order referencing their own product for simplicity.
        let qty = 1 + (i % 5);
        orders.push(Order {
            id: format!("ord_{i:04}"),
            tenant: cmd.tenant.clone(),
            customer_id: cid,
            lines: vec![OrderLine {
                product_id: pid,
                qty,
                price_cents: price,
            }],
            status: "created".to_string(),
        });
    }

    write_json(&cmd.out.join("customers.json"), &customers)?;
    write_json(&cmd.out.join("products.json"), &products)?;
    write_json(&cmd.out.join("orders.json"), &orders)?;

    println!(
        "Seeded {} customers, {} products, {} orders into {} (tenant: {})",
        customers.len(),
        products.len(),
        orders.len(),
        cmd.out.display(),
        cmd.tenant
    );
    println!("Load customers.json / products.json / orders.json into your vertical of choice.");
    Ok(())
}

fn write_json(path: &PathBuf, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("serializing demo data")?;
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
