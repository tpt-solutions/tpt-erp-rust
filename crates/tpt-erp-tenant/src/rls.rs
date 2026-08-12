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

/// Parameterized query that scopes the current transaction to `tenant_id` via Postgres's
/// `set_config(setting, value, is_local)`. This is the single source of truth for the
/// "set the active tenant" command: both the axum [`crate::web::TenantDb`] and the
/// [`crate::db::tenant_db_middleware`] execute exactly this query (with `.bind(tenant)`).
///
/// We deliberately use `set_config(.., true)` rather than a raw `SET LOCAL app.tenant_id =
/// $1`: the `SET`/`SET LOCAL` syntax does not reliably accept bind parameters across
/// drivers and protocol versions, whereas `set_config` is an ordinary function call and
/// works with bound parameters everywhere. The value is bound (never interpolated), so
/// there is no SQL-injection surface.
pub const SET_TENANT_QUERY: &str = "SELECT set_config('app.tenant_id', $1, true)";

/// Quote a SQL identifier safely: wrap in double quotes and escape any embedded double
/// quotes by doubling them, so table/column names cannot break out of the identifier context.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

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
        guc = TENANT_GUC,
        table = quote_ident(table),
        name = quote_ident(name),
        tenant_column = quote_ident(tenant_column),
    )
}

/// Enable RLS on a table (policies are inert until RLS is turned on).
pub fn enable_rls(table: &str) -> String {
    format!(
        "ALTER TABLE {} ENABLE ROW LEVEL SECURITY",
        quote_ident(table)
    )
}

/// Disable RLS (used in tests / migrations rollbacks).
pub fn disable_rls(table: &str) -> String {
    format!(
        "ALTER TABLE {} DISABLE ROW LEVEL SECURITY",
        quote_ident(table)
    )
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
            "CREATE POLICY \"orders_tenant\" ON \"orders\" FOR ALL USING (\"tenant_id\" = current_setting('app.tenant_id')::uuid)"
        );
        assert_eq!(
            enable_rls("orders"),
            "ALTER TABLE \"orders\" ENABLE ROW LEVEL SECURITY"
        );
        assert_eq!(
            disable_rls("orders"),
            "ALTER TABLE \"orders\" DISABLE ROW LEVEL SECURITY"
        );
    }
}
