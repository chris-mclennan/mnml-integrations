//! `aws cognito-idp list-user-pools` / `list-users` shell-outs +
//! structured response models. Pure CLI — no SDK dep.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPool {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Status", default)]
    pub status: Option<String>,
    #[serde(rename = "LambdaConfig", default)]
    pub lambda_config: Option<serde_json::Value>,
    #[serde(rename = "CreationDate", default)]
    pub creation_date: Option<f64>,
    #[serde(rename = "LastModifiedDate", default)]
    pub last_modified_date: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeType {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Value", default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Attributes", default)]
    pub attributes: Vec<AttributeType>,
    #[serde(rename = "UserCreateDate", default)]
    pub create_date: Option<f64>,
    #[serde(rename = "UserLastModifiedDate", default)]
    pub last_modified_date: Option<f64>,
    #[serde(rename = "Enabled", default)]
    pub enabled: Option<bool>,
    #[serde(rename = "UserStatus", default)]
    pub status: Option<String>,
}

impl User {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .and_then(|a| a.value.as_deref())
    }
    pub fn email(&self) -> Option<&str> {
        self.attribute("email")
    }
    pub fn sub(&self) -> Option<&str> {
        self.attribute("sub")
    }
}

#[derive(Debug, Deserialize)]
struct ListUserPoolsResponse {
    #[serde(rename = "UserPools")]
    user_pools: Vec<UserPoolDescriptionType>,
    #[serde(rename = "NextToken", default)]
    next_token: Option<String>,
}

/// `list-user-pools` returns a leaner shape than full pool detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPoolDescriptionType {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Status", default)]
    pub status: Option<String>,
    #[serde(rename = "LambdaConfig", default)]
    pub lambda_config: Option<serde_json::Value>,
    #[serde(rename = "CreationDate", default)]
    pub creation_date: Option<f64>,
    #[serde(rename = "LastModifiedDate", default)]
    pub last_modified_date: Option<f64>,
}

impl UserPoolDescriptionType {
    pub fn into_pool(self) -> UserPool {
        UserPool {
            id: self.id,
            name: self.name,
            status: self.status,
            lambda_config: self.lambda_config,
            creation_date: self.creation_date,
            last_modified_date: self.last_modified_date,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListUsersResponse {
    #[serde(rename = "Users")]
    users: Vec<User>,
    #[serde(rename = "PaginationToken", default)]
    pagination_token: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Pool(UserPool),
    User(User),
}

impl Item {
    pub fn primary_label(&self) -> &str {
        match self {
            Item::Pool(p) => &p.name,
            Item::User(u) => u.email().unwrap_or(&u.username),
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Pool(p) => {
                let status = p.status.as_deref().unwrap_or("?");
                format!("{} · {}", status, p.id)
            }
            Item::User(u) => {
                let status = u.status.as_deref().unwrap_or("?");
                let enabled = match u.enabled {
                    Some(true) => "",
                    Some(false) => " · DISABLED",
                    None => "",
                };
                format!("{status}{enabled}")
            }
        }
    }
}

pub fn list_user_pools(region: Option<&str>) -> Result<Vec<UserPool>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut cmd = Command::new("aws");
        cmd.args([
            "cognito-idp",
            "list-user-pools",
            "--max-results",
            "60",
            "--output",
            "json",
        ]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(t) = &token {
            cmd.args(["--next-token", t]);
        }
        let output = cmd
            .output()
            .with_context(|| "spawn `aws cognito-idp list-user-pools`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws cognito-idp list-user-pools failed: {}",
                stderr.trim()
            ));
        }
        let resp: ListUserPoolsResponse =
            serde_json::from_slice(&output.stdout).with_context(|| "parse list-user-pools JSON")?;
        all.extend(
            resp.user_pools
                .into_iter()
                .map(UserPoolDescriptionType::into_pool),
        );
        match resp.next_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    all.sort_by_key(|p| p.name.to_lowercase());
    Ok(all)
}

pub fn list_users(
    user_pool_id: &str,
    limit: u32,
    region: Option<&str>,
    filter: Option<&str>,
) -> Result<Vec<User>> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    let per_page = limit.min(60);
    while (all.len() as u32) < limit {
        let mut cmd = Command::new("aws");
        cmd.args([
            "cognito-idp",
            "list-users",
            "--user-pool-id",
            user_pool_id,
            "--limit",
            &per_page.to_string(),
            "--output",
            "json",
        ]);
        if let Some(f) = filter
            && !f.is_empty()
        {
            cmd.args(["--filter", f]);
        }
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(t) = &token {
            cmd.args(["--pagination-token", t]);
        }
        let output = cmd.output().with_context(|| {
            format!("spawn `aws cognito-idp list-users` for pool {user_pool_id}")
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws cognito-idp list-users failed for pool {user_pool_id}: {}",
                stderr.trim()
            ));
        }
        let resp: ListUsersResponse =
            serde_json::from_slice(&output.stdout).with_context(|| "parse list-users JSON")?;
        all.extend(resp.users);
        match resp.pagination_token {
            Some(t) if !t.is_empty() => token = Some(t),
            _ => break,
        }
    }
    // Newest users first — `created_date` desc.
    all.sort_by(|a, b| {
        b.create_date
            .partial_cmp(&a.create_date)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all.truncate(limit as usize);
    Ok(all)
}

/// Build a Cognito `--filter` expression from a freeform query string.
/// We try to be smart about the user's intent:
/// - Contains `@` → treated as an email; uses `email ^= "<q>"`
/// - Looks like a UUID (8-4-4-4-12 hex) → treated as a sub;
///   uses `sub = "<q>"` (exact match — sub is always exact)
/// - Otherwise → treated as a username prefix; `username ^= "<q>"`
///
/// Returns the bare filter expression — no `"--filter "` prefix.
pub fn build_user_filter(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        return String::new();
    }
    // Escape any embedded `"` to be safe.
    let escaped = q.replace('"', "\\\"");
    if q.contains('@') {
        format!("email ^= \"{escaped}\"")
    } else if looks_like_uuid(q) {
        format!("sub = \"{escaped}\"")
    } else {
        format!("username ^= \"{escaped}\"")
    }
}

fn looks_like_uuid(s: &str) -> bool {
    // 36 chars, dashes at 8, 13, 18, 23
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && s.chars()
            .enumerate()
            .all(|(i, c)| matches!(i, 8 | 13 | 18 | 23) == (c == '-'))
        && s.chars()
            .filter(|c| *c != '-')
            .all(|c| c.is_ascii_hexdigit())
}

/// Trim a `2026-06-06T18:30:00.123Z` ISO timestamp to `2026-06-06 18:30`.
/// Also handles unix-epoch floats (Cognito uses these in JSON).
pub fn fmt_epoch(f: f64) -> String {
    use chrono::DateTime;
    DateTime::from_timestamp(f as i64, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| format!("{f}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_user_pools_response() {
        let json = r#"{
            "UserPools": [
                {
                    "Id": "us-east-1_abc123",
                    "Name": "prod-users",
                    "Status": "Enabled",
                    "LambdaConfig": {"PreSignUp": "arn:aws:lambda:…"},
                    "CreationDate": 1700000000.0
                }
            ]
        }"#;
        let resp: ListUserPoolsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.user_pools.len(), 1);
        assert_eq!(resp.user_pools[0].name, "prod-users");
    }

    #[test]
    fn parses_list_users_response() {
        let json = r#"{
            "Users": [
                {
                    "Username": "f4a1b2c3-…",
                    "Attributes": [
                        {"Name": "sub", "Value": "f4a1b2c3-…"},
                        {"Name": "email", "Value": "user@example.com"},
                        {"Name": "email_verified", "Value": "true"}
                    ],
                    "UserCreateDate": 1700100000.0,
                    "UserLastModifiedDate": 1700200000.0,
                    "Enabled": true,
                    "UserStatus": "CONFIRMED"
                }
            ]
        }"#;
        let resp: ListUsersResponse = serde_json::from_str(json).unwrap();
        let u = &resp.users[0];
        assert_eq!(u.email(), Some("user@example.com"));
        assert_eq!(u.attribute("email_verified"), Some("true"));
        assert_eq!(u.enabled, Some(true));
        assert_eq!(u.status.as_deref(), Some("CONFIRMED"));
    }

    #[test]
    fn user_attribute_misses_return_none() {
        let u = User {
            username: "x".into(),
            attributes: vec![],
            create_date: None,
            last_modified_date: None,
            enabled: None,
            status: None,
        };
        assert!(u.email().is_none());
        assert!(u.attribute("custom:role").is_none());
    }

    #[test]
    fn item_secondary_label_for_user_marks_disabled() {
        let u = User {
            username: "x".into(),
            attributes: vec![],
            create_date: None,
            last_modified_date: None,
            enabled: Some(false),
            status: Some("CONFIRMED".into()),
        };
        let label = Item::User(u).secondary_label();
        assert!(label.contains("DISABLED"));
    }

    #[test]
    fn fmt_epoch_formats_known_date() {
        let out = fmt_epoch(1_704_067_200.0); // 2024-01-01 00:00 UTC
        assert!(out.starts_with("2024-01-01"));
    }

    #[test]
    fn build_filter_routes_by_query_shape() {
        assert_eq!(
            build_user_filter("ada@example.com"),
            r#"email ^= "ada@example.com""#
        );
        assert_eq!(build_user_filter("ada"), r#"username ^= "ada""#);
        assert_eq!(
            build_user_filter("f47ac10b-58cc-4372-a567-0e02b2c3d479"),
            r#"sub = "f47ac10b-58cc-4372-a567-0e02b2c3d479""#
        );
        assert_eq!(build_user_filter("  "), "");
    }

    #[test]
    fn looks_like_uuid_accepts_canonical_form() {
        assert!(looks_like_uuid("f47ac10b-58cc-4372-a567-0e02b2c3d479"));
        assert!(!looks_like_uuid("not-a-uuid"));
        assert!(!looks_like_uuid("f47ac10b58cc4372a5670e02b2c3d479")); // no dashes
        assert!(!looks_like_uuid("f47ac10b-58cc-4372-a567-0e02b2c3d479X")); // too long
    }

    #[test]
    fn build_filter_escapes_quotes() {
        let out = build_user_filter(r#"foo"bar"#);
        assert!(out.contains(r#"\"bar"#));
    }
}
