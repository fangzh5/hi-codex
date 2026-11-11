//! In-memory quota rows; keys are account identities, never emails or row numbers.
use crate::{
    accounts::{Account, AccountList},
    quota::UsageSnapshot,
};

#[derive(Clone, Debug)]
pub struct Row {
    pub account: Account,
    pub active: bool,
    pub usage: Option<UsageSnapshot>,
    pub updated_at: Option<i64>,
    pub error: Option<String>,
    pub refreshing: bool,
}

impl Row {
    #[cfg(test)]
    pub fn status(&self, now: i64) -> String {
        if self.refreshing {
            return "Refreshing...".into();
        }
        if self.error.is_some() {
            return if self.usage.is_some() {
                "Refresh failed · cached".into()
            } else {
                "Refresh failed".into()
            };
        }
        match self.updated_at {
            Some(at) if now.saturating_sub(at) < 60 => "Updated just now".into(),
            Some(at) => format!("Updated {}m ago", now.saturating_sub(at) / 60),
            None if self.active => "Current account".into(),
            None => "Not checked yet".into(),
        }
    }
}

pub fn reconcile(rows: &mut Vec<Row>, list: AccountList) {
    let mut old = std::mem::take(rows);
    *rows = list
        .accounts
        .into_iter()
        .map(|account| {
            let active = list.active_key.as_deref() == Some(&account.key);
            if let Some(index) = old.iter().position(|row| row.account.key == account.key) {
                let mut row = old.remove(index);
                row.account = account;
                row.active = active;
                row.refreshing = false;
                row
            } else {
                Row {
                    account,
                    active,
                    usage: None,
                    updated_at: None,
                    error: None,
                    refreshing: false,
                }
            }
        })
        .collect();
}

pub fn display_label(row: &Row, index: usize, visible: bool) -> String {
    if visible {
        let name = if row.account.alias.is_empty() {
            &row.account.email
        } else {
            &row.account.alias
        };
        format!("Account {} · {}", index + 1, name)
    } else {
        format!("Account {}", index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account(key: &str) -> Account {
        Account {
            key: key.into(),
            email: "private@example.invalid".into(),
            alias: "Private alias".into(),
            plan: "Plus".into(),
        }
    }
    #[test]
    fn cache_follows_stable_identity_when_rows_reorder_and_active_account_changes() {
        let mut rows = Vec::new();
        reconcile(
            &mut rows,
            AccountList {
                accounts: vec![account("a"), account("b")],
                active_key: Some("a".into()),
                can_restore: false,
            },
        );
        rows[0].updated_at = Some(123);
        rows[0].usage = Some(UsageSnapshot::default());
        reconcile(
            &mut rows,
            AccountList {
                accounts: vec![account("b"), account("a")],
                active_key: Some("b".into()),
                can_restore: false,
            },
        );
        assert!(rows[0].active);
        assert!(!rows[1].active);
        assert_eq!(rows[1].updated_at, Some(123));
        assert!(rows[0].usage.is_none());
        assert!(!display_label(&rows[1], 1, false).contains("private"));
        assert!(!display_label(&rows[1], 1, false).contains("Private alias"));
    }
    #[test]
    fn failed_refresh_preserves_cached_values_and_is_not_reported_as_zero() {
        let row = Row {
            account: account("a"),
            active: false,
            usage: Some(UsageSnapshot::default()),
            updated_at: Some(100),
            error: Some("failed".into()),
            refreshing: false,
        };
        assert_eq!(row.status(200), "Refresh failed · cached");
        assert!(row.usage.is_some());
    }
}
