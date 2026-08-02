//! Connection specs + credential resolution.
//!
//! A `ConnectionSpec` describes *how to reach* an engine — host,
//! port, user, database — plus a `CredsSource` that says where to
//! fetch the password from at connect time. Passwords are never
//! stored in the config file; they resolve from an env var, the
//! macOS keychain, or (later phases) an AWS profile.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
// tester 2026-07-31 SEV-3 — reject typo'd field names so `hots =
// "…"` doesn't silently parse (falling back to the driver's
// hardcoded default host). Callers get a clean parse error naming
// the unknown field instead of a mysterious "-@-:-/-" line in
// `--check`.
#[serde(deny_unknown_fields)]
pub struct ConnectionSpec {
    /// Stable id used as the label in the connection switcher and
    /// as the filename fragment for per-connection history.
    pub id: String,
    /// Human-readable label shown in the header + picker; falls
    /// back to `id` when omitted.
    #[serde(default)]
    pub label: Option<String>,
    /// One of the values returned by `drivers::supported_engines()`.
    pub engine: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: Option<String>,
    /// Database name — Postgres schema is separate, this is the
    /// top-level database on Postgres and the numeric db index on
    /// Redis (default `0`).
    #[serde(default)]
    pub database: Option<String>,
    /// Free-form extra params; not consumed by v0.1 drivers but
    /// preserved on round-trip.
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, String>,
    /// Where the password comes from. Optional for engines that
    /// don't need one (a Redis with `requirepass` unset, a
    /// Postgres with `trust` in pg_hba).
    #[serde(default)]
    pub creds: Option<CredsSource>,
}

impl ConnectionSpec {
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }

    /// Reject a spec that has anything smelling like a plaintext
    /// password (`password = "..."` at the top level, or
    /// `creds.type = "plaintext"`) — v0.1 refuses to load them.
    pub fn validate_no_plaintext_password(&self, raw: &toml::Value) -> Result<()> {
        Self::validate_no_plaintext_password_raw(raw)
    }

    /// Raw-value variant that runs BEFORE the strict deserialize
    /// (tester 2026-07-31 SEV-3 — otherwise `deny_unknown_fields`
    /// rejects `password` with a confusing "unknown field" toast
    /// instead of our intended "plaintext refused" message). Reads
    /// the id from the raw table so we can still name the offender.
    pub fn validate_no_plaintext_password_raw(raw: &toml::Value) -> Result<()> {
        let id = raw
            .as_table()
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>");
        if let Some(table) = raw.as_table() {
            for key in ["password", "pass", "secret"] {
                if table.contains_key(key) {
                    return Err(anyhow!(
                        "connection `{}`: plaintext `{}` field is not allowed — use `[connection.creds]` with type = \"env\" | \"keychain\"",
                        id,
                        key
                    ));
                }
            }
            // creds.type = "plaintext" — inspect the raw sub-table
            // rather than deserialize (which would fail on unknown
            // creds fields too, defeating the intent).
            if let Some(creds) = table.get("creds").and_then(|v| v.as_table())
                && creds.get("type").and_then(|v| v.as_str()) == Some("plaintext")
            {
                return Err(anyhow!(
                    "connection `{}`: creds.type = \"plaintext\" is not allowed",
                    id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredsSource {
    /// Password lives in the named env var. The `user` field is
    /// optional and overrides `ConnectionSpec::user` when set —
    /// useful when both come from env.
    Env {
        #[serde(default)]
        user: Option<String>,
        password: String,
    },
    /// macOS keychain lookup — `security find-generic-password -a
    /// <account> -s <service> -w` under the hood.
    Keychain { service: String, account: String },
    /// AWS profile — Phase 2+ (IAM auth for RDS). Loading a spec
    /// with this variant now surfaces a clear "not yet supported"
    /// error, but the parser accepts it so a user can prepare their
    /// config ahead of time.
    AwsProfile {
        profile: String,
        #[serde(default)]
        region: Option<String>,
    },
    /// Explicitly rejected by `validate_no_plaintext_password`.
    /// Present only so a misconfigured file parses far enough to
    /// hit the validator and get a clear error.
    Plaintext {
        #[serde(default)]
        user: Option<String>,
        password: String,
    },
}

/// Resolve the password for a spec. Returns `Ok("")` for a spec
/// with no creds set (host is expected to accept unauthenticated
/// connections).
pub fn resolve_password(spec: &ConnectionSpec) -> Result<String> {
    let Some(creds) = &spec.creds else {
        return Ok(String::new());
    };
    match creds {
        CredsSource::Env { password, .. } => std::env::var(password).map_err(|_| {
            anyhow!(
                "connection `{}`: env var `${}` is not set",
                spec.id,
                password
            )
        }),
        CredsSource::Keychain { service, account } => {
            let out = std::process::Command::new("security")
                .arg("find-generic-password")
                .arg("-a")
                .arg(account)
                .arg("-s")
                .arg(service)
                .arg("-w")
                .output()
                .map_err(|e| anyhow!("keychain lookup failed: {e}"))?;
            if !out.status.success() {
                return Err(anyhow!(
                    "connection `{}`: keychain has no entry for service=`{}` account=`{}`",
                    spec.id,
                    service,
                    account
                ));
            }
            let mut s = String::from_utf8_lossy(&out.stdout).to_string();
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        }
        CredsSource::AwsProfile { .. } => Err(anyhow!(
            "connection `{}`: aws_profile creds not supported in v0.1 — use env or keychain",
            spec.id
        )),
        CredsSource::Plaintext { .. } => Err(anyhow!(
            "connection `{}`: plaintext creds are not allowed",
            spec.id
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_label_falls_back_to_id() {
        let s = ConnectionSpec {
            id: "abc".into(),
            label: None,
            engine: "postgres".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: None,
        };
        assert_eq!(s.display_label(), "abc");
    }

    #[test]
    fn plaintext_creds_rejected_by_resolve() {
        let s = ConnectionSpec {
            id: "x".into(),
            label: None,
            engine: "postgres".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: Some(CredsSource::Plaintext {
                user: None,
                password: "hunter2".into(),
            }),
        };
        assert!(resolve_password(&s).is_err());
    }

    #[test]
    fn env_creds_read_from_environment() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("MNML_DB_TEST_PW_X", "letmein") };
        let s = ConnectionSpec {
            id: "y".into(),
            label: None,
            engine: "postgres".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: Some(CredsSource::Env {
                user: None,
                password: "MNML_DB_TEST_PW_X".into(),
            }),
        };
        let got = resolve_password(&s).unwrap();
        assert_eq!(got, "letmein");
        unsafe { std::env::remove_var("MNML_DB_TEST_PW_X") };
    }

    #[test]
    fn env_creds_missing_var_errors_clearly() {
        let s = ConnectionSpec {
            id: "z".into(),
            label: None,
            engine: "postgres".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: Some(CredsSource::Env {
                user: None,
                password: "DEFINITELY_UNSET_zzz".into(),
            }),
        };
        let err = resolve_password(&s).unwrap_err().to_string();
        assert!(err.contains("DEFINITELY_UNSET_zzz"), "err: {err}");
    }

    #[test]
    fn no_creds_returns_empty_password() {
        let s = ConnectionSpec {
            id: "w".into(),
            label: None,
            engine: "redis".into(),
            host: None,
            port: None,
            user: None,
            database: None,
            params: Default::default(),
            creds: None,
        };
        assert_eq!(resolve_password(&s).unwrap(), "");
    }
}
