//! Postgres Row-Level Security helpers.
//!
//! Tenant isolation is enforced at the database engine level: every relevant table gets
//! a policy that compares its tenant column to the session setting `app.tenant_id`, and
//! the middleware issues `SET LOCAL app.tenant_id = <uuid>` at the start of each
//! transaction. A mistakenly cross-tenant query is therefore rejected by Postgres itself,
//! not by application code.

use crate::TenantId;

/// The GUC (global user config) key used to carry the active tenant for the session.
pub const TENANT_GUC: &str = "app.tenant_id";

/// Build a `SET LOCAL` command that scopes the current transaction to `tenant_id`.
///
/// The value is the tenant's UUID (always an alphanumeric hyphenated string), so there
/// is no SQL-injection surface here.
pub fn set_tenant_command(tenant_id: &TenantId) -> String {
    format!("SET LOCAL {TENANT_GUC} = '{id}'", id = tenant_id.as_str())
}

/// Build a row-level-security policy for `table`, restricting rows to those whose
/// `tenant_column` equals the session tenant. `name` identifies the policy.
pub fn rls_policy(table: &str, tenant_column: &str, name: &str) -> String {
    format!(
        "CREATE POLICY {name} ON {table} FOR ALL USING ({tenant_column} = current_setting('{guc}')::uuid)",
        guc = TENANT_GUC
    )
}

/// Enable RLS on a table (policies are inert until RLS is turned on).
pub fn enable_rls(table: &str) -> String {
    format!("ALTER TABLE {table} ENABLE ROW LEVEL SECURITY")
}

/// Disable RLS (used in tests / migrations rollbacks).
pub fn disable_rls(table: &str) -> String {
    format!("ALTER TABLE {table} DISABLE ROW LEVEL SECURITY")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_command_contains_uuid() {
        let id = TenantId::new();
        let cmd = set_tenant_command(&id);
        assert!(cmd.starts_with(&format!("SET LOCAL {TENANT_GUC} = '")));
        assert!(cmd.ends_with("'"));
        assert!(cmd.contains(&id.as_str()));
    }

    #[test]
    fn policy_templates_are_well_formed() {
        let policy = rls_policy("orders", "tenant_id", "orders_tenant");
        assert_eq!(
            policy,
            "CREATE POLICY orders_tenant ON orders FOR ALL USING (tenant_id = current_setting('app.tenant_id')::uuid)"
        );
        assert_eq!(
            enable_rls("orders"),
            "ALTER TABLE orders ENABLE ROW LEVEL SECURITY"
        );
        assert_eq!(
            disable_rls("orders"),
            "ALTER TABLE orders DISABLE ROW LEVEL SECURITY"
        );
    }
}
