// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! MCP tokens (ADR 15): issued to a read-only account, shown once and kept as
//! their SHA-256, and the bearer lookup the MCP entry point signs in with.

use super::*;

impl Accounts {
    /// The account, and the name of the token, an `Authorization: Bearer …`
    /// header signs in as — at the MCP entry point, the one place that asks.
    /// A token past its expiry, one no account lists, or one of an account
    /// that is disabled or no longer read-only signs in nobody.
    pub fn account_for_token(&self, header: Option<&str>) -> Option<(String, String)> {
        self.account_for_token_at(header, now_secs())
    }

    fn account_for_token_at(&self, header: Option<&str>, now: u64) -> Option<(String, String)> {
        let token = bearer_token(header)?;
        let digest = Sha256::digest(token.as_bytes());
        let state = self.read();
        state.accounts.values().find_map(|entry| {
            if !entry.live() || entry.record.role != Role::ReadOnly {
                return None;
            }
            entry
                .tokens
                .iter()
                .find(|listed| same(&digest, &listed.sha256))
                .filter(|listed| listed.expires_at.is_none_or(|at| now < at))
                .map(|listed| (entry.record.name.clone(), listed.name.clone()))
        })
    }

    /// A new MCP token for `name`, which must be read-only; shown once.
    /// Answers the token.
    pub async fn issue_token(
        &self,
        name: &str,
        token_name: &str,
        expires_at: Option<u64>,
    ) -> Result<String, AccountError> {
        let token_name = token_name.trim();
        if token_name.is_empty() || token_name.chars().count() > MAX_NAME_CHARS {
            return Err(AccountError::BadName(format!(
                "a token's name has 1 to {MAX_NAME_CHARS} characters"
            )));
        }
        self.change(|state| {
            let entry = state.live(name).ok_or(AccountError::NotFound)?;
            if entry.record.role != Role::ReadOnly {
                return Err(AccountError::NotReadOnly);
            }
            if entry.record.tokens.iter().any(|t| t.name == token_name) {
                return Err(AccountError::TokenNameTaken);
            }
            let (token, sha256) = new_mcp_token();
            let mut record = entry.record.clone();
            record.tokens.push(TokenRecord {
                name: token_name.to_string(),
                sha256,
                expires_at,
                created_at: now_secs(),
            });
            Ok((vec![AccountOp::PutAccount(record)], token))
        })
        .await
    }

    /// Revoke `name`'s token called `token_name`.
    pub async fn revoke_token(&self, name: &str, token_name: &str) -> Result<(), AccountError> {
        self.change(|state| {
            let entry = state.accounts.get(name).ok_or(AccountError::NotFound)?;
            let mut record = entry.record.clone();
            let before = record.tokens.len();
            record.tokens.retain(|t| t.name != token_name);
            if record.tokens.len() == before {
                return Err(AccountError::NotFound);
            }
            Ok((vec![AccountOp::PutAccount(record)], ()))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::tests::with_admin;

    /// A token signs its read-only account in by its hash, until the first
    /// second of its expiry day, and names itself for the audit log. It is
    /// no password (ADR 15).
    #[tokio::test]
    async fn a_token_signs_in_its_account_until_it_expires() {
        let (token, sha256) = new_mcp_token();
        let (expiring, expiring_sha256) = new_mcp_token();
        let accounts = Accounts::from_toml(&format!(
            r#"
            [[users]]
            name = "ai"
            read_only = true
            tokens = [
              {{ name = "claude-laptop", sha256 = "{sha256}" }},
              {{ name = "ci", sha256 = "{expiring_sha256}", expires = "2027-01-01" }},
            ]
            "#
        ))
        .expect("accounts");
        let bearer = |token: &str| format!("Bearer {token}");
        let signed_in = |account: &str, token: &str| Some((account.to_string(), token.to_string()));
        assert_eq!(accounts.mcp_token_count(), 2);
        assert_eq!(
            accounts.account_for_token(Some(&bearer(&token))),
            signed_in("ai", "claude-laptop")
        );
        let expires_at = parse_expiry("2027-01-01").expect("a date");
        assert_eq!(
            accounts.account_for_token_at(Some(&bearer(&expiring)), expires_at - 1),
            signed_in("ai", "ci")
        );
        assert_eq!(
            accounts.account_for_token_at(Some(&bearer(&expiring)), expires_at),
            None
        );
        assert_eq!(accounts.account_for_token(Some("Bearer zdt_not-a-listed-token")), None);
        assert_eq!(accounts.account_for_token(None), None);
        assert_eq!(
            accounts.check_basic_credentials("ai".to_string(), token).await,
            Basic::Refused("ai".to_string())
        );
    }

    /// A token is a read-only account's, shown once, kept as its hash, and
    /// signs in at once; it holds the account read-only while it exists.
    #[tokio::test]
    async fn a_token_is_issued_to_a_read_only_account_and_revoked() {
        let accounts = with_admin("alice@secret,ai:ro@pa55word").await;
        assert!(matches!(
            accounts.issue_token("alice", "t", None).await,
            Err(AccountError::NotReadOnly)
        ));
        let token = accounts.issue_token("ai", "claude", None).await.expect("issued");
        assert_eq!(
            accounts.account_for_token(Some(&format!("Bearer {token}"))),
            Some(("ai".to_string(), "claude".to_string()))
        );
        assert!(matches!(
            accounts.issue_token("ai", "claude", None).await,
            Err(AccountError::TokenNameTaken)
        ));
        assert!(
            !format!("{:?}", accounts.view("ai")).contains(&token),
            "the token itself is not kept"
        );
        assert!(matches!(
            accounts
                .update(
                    "ai",
                    AccountChange {
                        role: Some(Role::Full),
                        ..AccountChange::default()
                    }
                )
                .await,
            Err(AccountError::NotReadOnly)
        ));
        accounts.revoke_token("ai", "claude").await.expect("revoked");
        assert_eq!(accounts.account_for_token(Some(&format!("Bearer {token}"))), None);
        assert!(matches!(
            accounts.revoke_token("ai", "claude").await,
            Err(AccountError::NotFound)
        ));
    }
}
