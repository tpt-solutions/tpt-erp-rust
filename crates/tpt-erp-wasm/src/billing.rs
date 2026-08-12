//! Per-tenant usage metering driven by the wasm runtime's fuel counter.
//!
//! [`PluginRuntime`] already meters WebAssembly *fuel* — a deterministic proxy for CPU
//! instructions executed — as its primary sandbox barrier. That same meter is a natural
//! billing signal: each `run` consumes a known amount of fuel, so recording fuel-per-tenant
//! yields a fair, resource-accurate usage bill without any extra instrumentation in the
//! guest. [`UsageMeter`] is a lock-free-ish (mutex-guarded) accumulator shared across every
//! plugin a tenant runs; [`BillingReport`] is the operator-facing rollup.

use std::collections::HashMap;
use parking_lot::Mutex;
use tpt_erp_tenant::TenantId;

/// Accumulated usage for a single tenant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageRecord {
    /// Number of plugin `run` invocations attributed to the tenant.
    pub calls: u64,
    /// Total wasm fuel consumed across those calls (a proxy for CPU work).
    pub fuel_consumed: u64,
}

/// A live, shared meter of per-tenant plugin usage.
///
/// One instance is owned by the [`crate::PluginRuntime`] and consulted on every plugin call,
/// so a single tenant running many plugins/handles still accrues to one bill.
#[derive(Debug, Default)]
pub struct UsageMeter {
    per_tenant: Mutex<HashMap<TenantId, UsageRecord>>,
}

impl UsageMeter {
    /// A fresh, empty meter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attribute one plugin call of `fuel` fuel to `tenant`.
    pub fn record_call(&self, tenant: TenantId, fuel: u64) {
        let mut map = self.per_tenant.lock();
        let r = map.entry(tenant).or_default();
        r.calls += 1;
        r.fuel_consumed += fuel;
    }

    /// The accumulated record for `tenant` (zero if unseen).
    pub fn for_tenant(&self, tenant: TenantId) -> UsageRecord {
        self.per_tenant
            .lock()
            .get(&tenant)
            .copied()
            .unwrap_or_default()
    }

    /// A snapshot of every tenant's usage, sorted by tenant id for stable output.
    pub fn report(&self) -> BillingReport {
        let mut rows: Vec<(TenantId, UsageRecord)> =
            self.per_tenant.lock().iter().map(|(k, v)| (*k, *v)).collect();
        rows.sort_by_key(|(k, _)| k.as_str().to_string());
        let total_calls = rows.iter().map(|(_, r)| r.calls).sum();
        let total_fuel = rows.iter().map(|(_, r)| r.fuel_consumed).sum();
        BillingReport {
            rows,
            total_calls,
            total_fuel,
        }
    }
}

/// Operator-facing rollup of [`UsageMeter`] state.
#[derive(Debug, Clone, Default)]
pub struct BillingReport {
    /// Per-tenant usage rows.
    pub rows: Vec<(TenantId, UsageRecord)>,
    /// Sum of calls across all tenants.
    pub total_calls: u64,
    /// Sum of fuel across all tenants.
    pub total_fuel: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_calls_and_fuel_per_tenant() {
        let meter = UsageMeter::new();
        let a = TenantId::new();
        let b = TenantId::new();

        meter.record_call(a, 1_000);
        meter.record_call(a, 2_500);
        meter.record_call(b, 500);

        assert_eq!(meter.for_tenant(a), UsageRecord { calls: 2, fuel_consumed: 3_500 });
        assert_eq!(meter.for_tenant(b), UsageRecord { calls: 1, fuel_consumed: 500 });
        // An unseen tenant reads as zero.
        assert_eq!(meter.for_tenant(TenantId::new()), UsageRecord::default());
    }

    #[test]
    fn report_aggregates_totals() {
        let meter = UsageMeter::new();
        let a = TenantId::new();
        let b = TenantId::new();
        meter.record_call(a, 100);
        meter.record_call(b, 200);
        meter.record_call(a, 300);

        let r = meter.report();
        assert_eq!(r.total_calls, 3);
        assert_eq!(r.total_fuel, 600);
        assert_eq!(r.rows.len(), 2);
    }
}
