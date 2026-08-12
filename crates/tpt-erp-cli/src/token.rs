//! `tpt token` — mint an HS256 JWT for local/dev use against a TPT ERP server that has
//! authentication enabled (see `tpt-erp-tenant`'s `auth` module and `TPT_JWT_SECRET`).
//!
//! This is a developer convenience for standing up a local trial; it is **not** a
//! production token issuer. In production, tokens should be minted by your IdP and
//! verified by the server.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use tpt_erp_tenant::auth::{Claims, JwtConfig};

#[derive(Parser)]
pub(crate) struct TokenCommand {
    #[command(subcommand)]
    pub(crate) command: TokenSubcommand,
}

#[derive(Subcommand)]
pub(crate) enum TokenSubcommand {
    /// Mint a JWT for local/dev use.
    Mint(TokenMint),
}

#[derive(Args)]
pub(crate) struct TokenMint {
    /// Shared secret (HS256) the server uses to verify tokens.
    #[arg(long, env = "TPT_JWT_SECRET")]
    secret: Option<String>,
    /// Subject (user id) written to the `sub` claim.
    #[arg(long, default_value = "dev-user")]
    sub: String,
    /// Tenant slug the token is scoped to (the `tenant` claim).
    #[arg(long, default_value = "acme")]
    tenant: String,
    /// Roles (comma-separated) written to the `roles` claim.
    #[arg(long, value_delimiter = ',')]
    roles: Vec<String>,
    /// Token lifetime in seconds (written to the `exp` claim).
    #[arg(long, default_value_t = 3600)]
    ttl: i64,
    /// Optional issuer claim (`iss`).
    #[arg(long)]
    iss: Option<String>,
    /// Optional audience claim (`aud`).
    #[arg(long)]
    aud: Option<String>,
    /// Emit a ready-to-paste `Authorization: Bearer <token>` header line.
    #[arg(long)]
    curl: bool,
}

pub(crate) fn run(cmd: TokenCommand) -> Result<()> {
    match cmd.command {
        TokenSubcommand::Mint(m) => {
            let secret = m.secret.unwrap_or_else(|| "dev-secret".into());
            let mut claims = Claims::new(m.sub, m.tenant, m.roles).with_ttl_secs(m.ttl);
            claims.iss = m.iss;
            claims.aud = m.aud;
            let config = JwtConfig::new(secret);
            let token = config.issue(&claims)?;
            if m.curl {
                println!("Authorization: Bearer {token}");
            } else {
                println!("{token}");
            }
            Ok(())
        }
    }
}
