//! Opt-in, independent account snapshots. Never writes codex-auth's registry.
//! JWT claims are used only as local labels; authentication remains Codex's job.
use crate::credential_storage;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 100;

#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    pub key: String,
    pub email: String,
    pub plan: String,
    pub alias: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("identity", &"*****")
            .finish()
    }
}

impl Account {
    pub fn label(&self, visible: bool, index: usize) -> String {
        let mut label = format!("Account {}", index + 1);
        if visible {
            let name = if self.alias.is_empty() {
                &self.email
            } else {
                &self.alias
            };
            let workspace: String = self
                .key
                .rsplit_once("::")
                .map(|(_, workspace)| workspace)
                .unwrap_or_default()
                .chars()
                .take(8)
                .collect();
            label.push_str(&format!(" — {name} [{workspace}]"));
        }
        // Win32 menus interpret ampersands as accelerators.
        format!(
            "{} ({})",
            label.replace('&', "&&"),
            self.plan.replace('&', "&&")
        )
    }
}

pub struct AccountList {
    pub accounts: Vec<Account>,
    pub active_key: Option<String>,
    pub can_restore: bool,
}

pub struct ImportReport {
    pub added: usize,
    pub skipped: usize,
}

pub struct AccountManager {
    home: PathBuf,
}

pub fn codex_home() -> Result<PathBuf, String> {
    let path = env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|value| PathBuf::from(value).join(".codex")))
        .ok_or("Could not determine CODEX_HOME")?;
    fs::canonicalize(&path).map_err(|_| "CODEX_HOME does not exist or is inaccessible".into())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    read_optional_bounded(path)?.ok_or_else(|| "Account file is missing".into())
}

fn read_optional_bounded(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Account file is inaccessible".into()),
    };
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
        return Err("Account files must be regular files, not links or reparse points".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_FILE + 1).read_to_end(&mut bytes))
        .map_err(|_| "Could not read account file")?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("Account file exceeds the size limit".into());
    }
    Ok(Some(bytes))
}

fn json_bytes(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
        .map_err(|_| "Invalid account JSON; no credentials were changed".into())
}

fn text(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn claims(jwt: &str) -> Result<Value, String> {
    let parts: Vec<_> = jwt.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err("Invalid ChatGPT identity token".into());
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| URL_SAFE.decode(parts[1]))
        .map_err(|_| "Invalid ChatGPT identity token")?;
    json_bytes(&decoded)
}

fn identity(auth: &Value) -> Result<Account, String> {
    if text(auth, "OPENAI_API_KEY").is_some()
        || text(auth, "auth_mode").is_some_and(|mode| mode != "chatgpt")
    {
        return Err(
            "Experimental account switching currently supports ChatGPT file credentials only"
                .into(),
        );
    }
    let tokens = auth
        .get("tokens")
        .ok_or("No ChatGPT credentials in this file")?;
    for field in ["access_token", "refresh_token", "id_token"] {
        if text(tokens, field).is_none() {
            return Err("Incomplete ChatGPT credentials; sign in again in Codex".into());
        }
    }
    let id_claims = claims(&text(tokens, "id_token").unwrap())?;
    let auth_claims = id_claims
        .get("https://api.openai.com/auth")
        .unwrap_or(&Value::Null);
    let user = text(auth_claims, "chatgpt_user_id")
        .or_else(|| text(&id_claims, "sub"))
        .ok_or("Identity token has no user identifier")?;
    let workspace = text(tokens, "account_id")
        .or_else(|| text(auth_claims, "chatgpt_account_id"))
        .ok_or("Identity token has no account identifier")?;
    let email = text(&id_claims, "email").unwrap_or_else(|| "Phone login".into());
    let plan = text(auth_claims, "chatgpt_plan_type").unwrap_or_else(|| "ChatGPT".into());
    if [&user, &workspace, &email, &plan]
        .iter()
        .any(|value| value.len() > 512 || value.chars().any(char::is_control))
    {
        return Err("Invalid account identity fields".into());
    }
    Ok(Account {
        key: format!("{user}::{workspace}"),
        email,
        plan,
        alias: String::new(),
    })
}

fn fresh_store() -> Value {
    json!({"schema_version": 1, "accounts": [], "backup": null})
}

fn entry(auth: Value, alias: &str) -> Result<Value, String> {
    let account = identity(&auth)?;
    let alias = alias.trim();
    if alias.len() > 128 || alias.chars().any(char::is_control) {
        return Err("Invalid account alias".into());
    }
    Ok(json!({"key": account.key, "alias": alias, "auth": auth}))
}

fn upsert(store: &mut Value, incoming: Value, replace: bool) -> Result<bool, String> {
    let accounts = store
        .get_mut("accounts")
        .and_then(Value::as_array_mut)
        .ok_or("Invalid account storage")?;
    if let Some(existing) = accounts
        .iter_mut()
        .find(|account| account["key"] == incoming["key"])
    {
        if replace {
            let alias = existing["alias"].clone();
            *existing = incoming;
            existing["alias"] = alias;
        }
        return Ok(false);
    }
    if accounts.len() >= MAX_ACCOUNTS {
        return Err("The experimental account limit is 100".into());
    }
    accounts.push(incoming);
    Ok(true)
}

fn require_file_config(config: &toml::Value) -> Result<(), String> {
    if let Some(table) = config.as_table() {
        if let Some(mode) = table.get("cli_auth_credentials_store") {
            if mode.as_str() != Some("file") {
                return Err("Account switching requires file credentials. Keyring, auto and ephemeral modes are unsupported; usage viewing is unaffected.".into());
            }
        }
        for value in table.values() {
            require_file_config(value)?;
        }
    }
    Ok(())
}

impl AccountManager {
    pub fn current() -> Result<Self, String> {
        Ok(Self {
            home: codex_home()?,
        })
    }

    fn directory(&self) -> PathBuf {
        self.home.join("hi-codex")
    }
    fn storage_path(&self) -> PathBuf {
        self.directory().join("accounts.dpapi")
    }
    fn auth_path(&self) -> PathBuf {
        self.home.join("auth.json")
    }

    fn load(&self) -> Result<Value, String> {
        let path = self.storage_path();
        let encrypted = match read_optional_bounded(&path)? {
            Some(bytes) => bytes,
            None => match read_optional_bounded(&credential_storage::recovery_path(&path))? {
                Some(bytes) => bytes,
                None => return Ok(fresh_store()),
            },
        };
        let plain = credential_storage::unprotect(&encrypted)?;
        let store = json_bytes(&plain)?;
        if store["schema_version"].as_u64() != Some(1) {
            return Err("Unsupported HiCodex account storage version".into());
        }
        let accounts = store["accounts"]
            .as_array()
            .ok_or("Invalid account storage")?;
        if accounts.len() > MAX_ACCOUNTS {
            return Err("Too many stored accounts".into());
        }
        let mut keys = std::collections::HashSet::new();
        for account in accounts {
            let parsed = identity(&account["auth"])?;
            if account["key"].as_str() != Some(&parsed.key)
                || !keys.insert(parsed.key)
                || account["alias"].as_str().is_none()
            {
                return Err("Account storage contains an invalid record".into());
            }
        }
        Ok(store)
    }

    fn save(&self, store: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(store).map_err(|_| "Could not encode account storage")?;
        if bytes.len() as u64 > MAX_FILE / 2 {
            return Err("Account storage is too large".into());
        }
        let encrypted = credential_storage::protect(&bytes)?;
        credential_storage::atomic_write(&self.storage_path(), &encrypted)
    }

    fn operation_lock(&self) -> Result<File, String> {
        fs::create_dir_all(self.directory())
            .map_err(|_| "Could not create account storage directory")?;
        let metadata = fs::symlink_metadata(self.directory())
            .map_err(|_| "Could not inspect storage directory")?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Account storage directory must not be a link".into());
        }
        credential_storage::lock(&self.directory().join("operation.lock"))
    }

    fn check_switch_config(&self, auth: &Value) -> Result<(), String> {
        for name in [
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "OPENAI_IDENTITY_TOKEN_FILE",
            "OPENAI_WORKLOAD_IDENTITY_TOKEN_FILE",
        ] {
            if env::var_os(name).is_some_and(|value| !value.is_empty()) {
                return Err("An environment credential override is active; file account switching is unavailable".into());
            }
        }
        for name in ["config.toml", "requirements.toml", "managed_config.toml"] {
            let path = self.home.join(name);
            if !path
                .try_exists()
                .map_err(|_| "Could not inspect Codex configuration")?
            {
                continue;
            }
            let bytes = read_bounded(&path)?;
            let source = std::str::from_utf8(&bytes).map_err(|_| "Invalid Codex configuration")?;
            let config: toml::Value = toml::from_str(source.trim_start_matches('\u{feff}'))
                .map_err(|_| "Could not parse Codex configuration; switching was blocked")?;
            require_file_config(&config)?;
            if config
                .get("desktop")
                .and_then(|desktop| desktop.get("runCodexInWindowsSubsystemForLinux"))
                .and_then(toml::Value::as_bool)
                == Some(true)
            {
                return Err(
                    "This experimental switcher supports Windows-native Codex only, not WSL".into(),
                );
            }
            if config
                .get("forced_login_method")
                .and_then(toml::Value::as_str)
                .is_some_and(|value| value != "chatgpt")
            {
                return Err("Codex configuration restricts the login method".into());
            }
            if let Some(forced) = config
                .get("forced_chatgpt_workspace_id")
                .and_then(toml::Value::as_str)
            {
                if auth["tokens"]["account_id"].as_str() != Some(forced) {
                    return Err(
                        "The target account does not match the required Codex workspace".into(),
                    );
                }
            }
        }
        Ok(())
    }

    pub fn list(&self) -> Result<AccountList, String> {
        let store = self.load()?;
        let active_key = read_bounded(&self.auth_path())
            .ok()
            .and_then(|bytes| json_bytes(&bytes).ok())
            .and_then(|auth| identity(&auth).ok())
            .map(|account| account.key);
        let accounts = store["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| {
                let mut account = identity(&record["auth"]).expect("validated store");
                account.alias = record["alias"].as_str().unwrap().to_owned();
                account
            })
            .collect();
        Ok(AccountList {
            accounts,
            active_key,
            can_restore: !store["backup"].is_null(),
        })
    }

    pub fn save_current(&self) -> Result<(), String> {
        let _lock = self.operation_lock()?;
        let auth = json_bytes(&read_bounded(&self.auth_path())?)?;
        self.check_switch_config(&auth)?;
        let mut store = self.load()?;
        upsert(&mut store, entry(auth, "")?, true)?;
        self.save(&store)
    }

    pub fn import_file(&self, path: &Path) -> Result<usize, String> {
        let auth = json_bytes(&read_bounded(path)?)?;
        let values: Vec<Value> = if let Some(array) = auth.as_array() {
            array.clone()
        } else {
            vec![auth]
        };
        let incoming = values
            .into_iter()
            .map(|auth| entry(auth, ""))
            .collect::<Result<Vec<_>, _>>()?;
        self.import_entries(incoming)
    }

    fn import_entries(&self, incoming: Vec<Value>) -> Result<usize, String> {
        for account in &incoming {
            let validated = entry(
                account["auth"].clone(),
                account["alias"].as_str().unwrap_or(""),
            )?;
            if validated["key"] != account["key"] {
                return Err("Imported account identity does not match its credentials".into());
            }
        }
        let _lock = self.operation_lock()?;
        let mut store = self.load()?;
        let mut count = 0;
        for account in incoming {
            count += usize::from(upsert(&mut store, account, false)?);
        }
        if count > 0 {
            self.save(&store)?;
        }
        Ok(count)
    }

    pub fn import_codex_auth(&self) -> Result<ImportReport, String> {
        let folder = self.home.join("accounts");
        let registry = json_bytes(&read_bounded(&folder.join("registry.json"))?)?;
        if !matches!(registry["schema_version"].as_u64(), Some(2..=4)) {
            return Err("Unsupported codex-auth registry version; export auth JSON files and import them instead".into());
        }
        let records = registry["accounts"]
            .as_array()
            .ok_or("Invalid codex-auth registry")?;
        let mut files = fs::read_dir(&folder)
            .map_err(|_| "Could not read codex-auth accounts")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Could not read codex-auth accounts")?;
        files.sort_by_key(|file| file.file_name());
        let mut incoming = Vec::new();
        let mut skipped = 0;
        for file in files {
            if !file.file_name().to_string_lossy().ends_with(".auth.json") {
                continue;
            }
            let candidate = (|| {
                let auth = json_bytes(&read_bounded(&file.path())?)?;
                let account = identity(&auth)?;
                let record = records
                    .iter()
                    .find(|record| record["account_key"].as_str() == Some(&account.key));
                // Legacy records may be email-keyed. Never import unregistered snapshots.
                let record = record.or_else(|| {
                    records.iter().find(|record| {
                        record["email"].as_str() == Some(&account.email)
                            && record["chatgpt_account_id"]
                                .as_str()
                                .is_some_and(|workspace| {
                                    auth["tokens"]["account_id"].as_str() == Some(workspace)
                                })
                    })
                });
                let record = record.ok_or("Snapshot is not registered")?;
                entry(auth, record["alias"].as_str().unwrap_or(""))
            })();
            match candidate {
                Ok(account) => incoming.push(account),
                Err(_) => skipped += 1,
            }
        }
        if incoming.is_empty() {
            return Err("No readable, registered ChatGPT snapshots found in codex-auth. Sign in again there or import a supported auth JSON file.".into());
        }
        let candidates = incoming.len();
        let added = self.import_entries(incoming)?;
        Ok(ImportReport {
            added,
            skipped: skipped + candidates - added,
        })
    }

    pub fn switch(&self, key: &str) -> Result<bool, String> {
        let _lock = self.operation_lock()?;
        let mut store = self.load()?;
        let target = store["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["key"].as_str() == Some(key))
            .ok_or("Account no longer exists")?["auth"]
            .clone();
        self.check_switch_config(&target)?;
        let original = read_bounded(&self.auth_path())?;
        let current = json_bytes(&original)?;
        let current_key = identity(&current)?.key;
        if current_key == key {
            return Ok(false);
        }
        // Capture refresh-token rotation BEFORE leaving the current account.
        upsert(&mut store, entry(current.clone(), "")?, true)?;
        store["backup"] = json!({"auth": current, "target": target});
        self.save(&store)?;
        self.activate_if_unchanged(Some(&original), &target)?;
        Ok(true)
    }

    fn activate_if_unchanged(&self, original: Option<&[u8]>, target: &Value) -> Result<(), String> {
        if read_optional_bounded(&self.auth_path())?.as_deref() != original {
            return Err("Current credentials changed during the operation. Close Codex and retry; no switch was performed.".into());
        }
        let bytes =
            serde_json::to_vec_pretty(target).map_err(|_| "Could not encode target credentials")?;
        if original.is_some() {
            credential_storage::atomic_write(&self.auth_path(), &bytes)?;
        } else {
            credential_storage::atomic_create(&self.auth_path(), &bytes)?;
        }
        if json_bytes(&read_bounded(&self.auth_path())?)? != *target {
            return Err("Credentials changed immediately after switching. State is uncertain; close other account tools and check the active account.".into());
        }
        Ok(())
    }

    pub fn restore(&self) -> Result<(), String> {
        let _lock = self.operation_lock()?;
        let mut store = self.load()?;
        let backup = store["backup"]["auth"].clone();
        identity(&backup)?;
        self.check_switch_config(&backup)?;
        let original = read_optional_bounded(&self.auth_path())?;
        if let Some(current) = original.as_deref().and_then(|bytes| json_bytes(bytes).ok()) {
            if current != store["backup"]["target"] && current != backup {
                return Err("Credentials have changed since the last switch; recovery will not overwrite newer credentials. Use the account picker instead.".into());
            }
        }
        // Missing or syntactically corrupt credentials can be recovered explicitly.
        // Recheck even if already restored, so a prior metadata-save failure is retryable.
        self.activate_if_unchanged(original.as_deref(), &backup)?;
        store["backup"] = Value::Null;
        self.save(&store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        manager: AccountManager,
    }
    impl Fixture {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "hicodex-account-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self {
                manager: AccountManager { home: path },
            }
        }
        fn active(&self, auth: &Value) {
            credential_storage::atomic_write(
                &self.manager.auth_path(),
                &serde_json::to_vec(auth).unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.manager.home);
        }
    }

    fn auth(user: &str, workspace: &str, refresh: &str) -> Value {
        let payload = json!({"sub": user, "email": "fixture@example.com", "https://api.openai.com/auth": {
            "chatgpt_user_id": user, "chatgpt_account_id": workspace, "chatgpt_plan_type": "plus"
        }});
        let jwt = format!(
            "e30.{}.c2ln",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );
        json!({"auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {
            "id_token": jwt, "access_token": "synthetic-access", "refresh_token": refresh, "account_id": workspace
        }})
    }

    #[test]
    fn passive_list_creates_nothing() {
        let f = Fixture::new();
        assert!(f.manager.list().unwrap().accounts.is_empty());
        assert!(!f.manager.directory().exists());
    }

    #[test]
    fn stable_identity_distinguishes_workspaces_and_hides_labels() {
        let mut a = identity(&auth("user", "personal", "a")).unwrap();
        let mut b = identity(&auth("user", "team", "b")).unwrap();
        a.alias = "Private & work".into();
        b.alias = a.alias.clone();
        assert_ne!(a.key, b.key);
        assert_eq!(a.label(false, 0), "Account 1 (plus)");
        assert_eq!(b.label(false, 1), "Account 2 (plus)");
        assert_ne!(a.label(true, 0), b.label(true, 1));
        assert!(a.label(true, 0).contains("Private && work [personal]"));
        assert!(b.label(true, 1).contains("[team]"));
        assert!(!format!("{a:?}").contains("fixture@example.com"));
    }

    #[test]
    fn switch_saves_rotated_tokens_and_can_restore() {
        let f = Fixture::new();
        let a = auth("user-a", "personal", "old");
        let b = auth("user-b", "personal", "other");
        f.active(&a);
        f.manager.save_current().unwrap();
        f.manager
            .import_entries(vec![entry(b.clone(), "Work").unwrap()])
            .unwrap();
        let rotated = auth("user-a", "personal", "rotated");
        f.active(&rotated);
        assert!(f.manager.switch(&identity(&b).unwrap().key).unwrap());
        assert_eq!(
            json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap(),
            b
        );
        let store = f.manager.load().unwrap();
        assert_eq!(
            store["accounts"][0]["auth"]["tokens"]["refresh_token"],
            "rotated"
        );
        f.manager.restore().unwrap();
        assert_eq!(
            json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap(),
            rotated
        );
    }

    #[test]
    fn invalid_store_and_import_do_not_change_active_auth() {
        let f = Fixture::new();
        let original = auth("a", "personal", "original");
        f.active(&original);
        f.manager.save_current().unwrap();
        let before = read_bounded(&f.manager.auth_path()).unwrap();
        assert!(f
            .manager
            .import_entries(vec![json!({"key": "bad"})])
            .is_err());
        assert!(f.manager.switch("bad").is_err());
        assert_eq!(read_bounded(&f.manager.auth_path()).unwrap(), before);
    }

    #[test]
    fn unsupported_storage_and_external_changes_are_rejected() {
        let config: toml::Value = toml::from_str("cli_auth_credentials_store = 'keyring'").unwrap();
        assert!(require_file_config(&config).is_err());
        let f = Fixture::new();
        f.active(&auth("a", "workspace", "a"));
        let before = read_bounded(&f.manager.auth_path()).unwrap();
        f.active(&auth("b", "workspace", "b"));
        assert!(f
            .manager
            .activate_if_unchanged(Some(&before), &auth("c", "workspace", "c"))
            .is_err());
        assert_eq!(
            identity(&json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap())
                .unwrap()
                .key,
            "b::workspace"
        );
        // Recovery from a missing file must not replace a login created before commit.
        assert!(f
            .manager
            .activate_if_unchanged(None, &auth("c", "workspace", "c"))
            .is_err());
        assert_eq!(
            identity(&json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap())
                .unwrap()
                .key,
            "b::workspace"
        );
    }

    #[test]
    fn import_does_not_overwrite_newer_local_credentials() {
        let f = Fixture::new();
        f.active(&auth("a", "workspace", "fresh"));
        f.manager.save_current().unwrap();
        assert_eq!(
            f.manager
                .import_entries(vec![
                    entry(auth("a", "workspace", "stale"), "Alias").unwrap()
                ])
                .unwrap(),
            0
        );
        assert_eq!(
            f.manager.load().unwrap()["accounts"][0]["auth"]["tokens"]["refresh_token"],
            "fresh"
        );
    }

    #[test]
    fn codex_auth_import_is_one_way_and_keeps_aliases() {
        let f = Fixture::new();
        let a = auth("a", "workspace", "imported");
        let active = auth("b", "workspace", "active");
        f.active(&active);
        let source = f.manager.home.join("accounts");
        fs::create_dir(&source).unwrap();
        let key = identity(&a).unwrap().key;
        let registry = serde_json::to_vec(&json!({"schema_version": 4, "accounts": [
            {"account_key": key, "alias": "Work", "email": "fixture@example.com"}
        ]}))
        .unwrap();
        let snapshot = serde_json::to_vec(&a).unwrap();
        fs::write(source.join("registry.json"), &registry).unwrap();
        fs::write(source.join("fixture.auth.json"), &snapshot).unwrap();
        let orphan = b"malformed orphan snapshot";
        fs::write(source.join("orphan.auth.json"), orphan).unwrap();
        let report = f.manager.import_codex_auth().unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(report.skipped, 1);
        let imported = f.manager.list().unwrap();
        assert_eq!(imported.accounts[0].alias, "Work");
        assert_eq!(imported.active_key.as_deref(), Some("b::workspace"));
        assert_eq!(fs::read(source.join("registry.json")).unwrap(), registry);
        assert_eq!(
            fs::read(source.join("fixture.auth.json")).unwrap(),
            snapshot
        );
        assert_eq!(fs::read(source.join("orphan.auth.json")).unwrap(), orphan);
        let repeated = f.manager.import_codex_auth().unwrap();
        assert_eq!(repeated.added, 0);
        assert_eq!(repeated.skipped, 2);
        assert_eq!(
            json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap(),
            active
        );
    }

    #[test]
    fn json_batch_import_is_all_or_nothing() {
        let f = Fixture::new();
        let path = f.manager.home.join("import.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!([auth("a", "workspace", "a"), {"tokens": {}}])).unwrap(),
        )
        .unwrap();
        assert!(f.manager.import_file(&path).is_err());
        assert!(!f.manager.directory().exists());
        assert!(identity(&json!({"OPENAI_API_KEY": "synthetic-key"})).is_err());
    }

    #[test]
    fn corrupt_store_does_not_touch_auth_and_recovery_respects_new_login() {
        let f = Fixture::new();
        f.active(&auth("a", "workspace", "a"));
        f.manager.save_current().unwrap();
        let b = auth("b", "workspace", "b");
        f.manager
            .import_entries(vec![entry(b.clone(), "").unwrap()])
            .unwrap();
        f.manager.switch(&identity(&b).unwrap().key).unwrap();
        let externally_refreshed = auth("b", "workspace", "new-b");
        f.active(&externally_refreshed);
        assert!(f.manager.restore().is_err());
        let before = read_bounded(&f.manager.auth_path()).unwrap();
        credential_storage::atomic_write(&f.manager.storage_path(), b"corrupt").unwrap();
        assert!(f.manager.list().is_err());
        assert!(f.manager.switch("a::workspace").is_err());
        assert_eq!(read_bounded(&f.manager.auth_path()).unwrap(), before);
    }

    #[test]
    fn operation_lock_and_store_write_failure_leave_auth_intact() {
        let f = Fixture::new();
        f.active(&auth("a", "workspace", "a"));
        let before = read_bounded(&f.manager.auth_path()).unwrap();
        let held = f.manager.operation_lock().unwrap();
        assert!(f.manager.save_current().is_err());
        drop(held);
        f.manager.save_current().unwrap();
        let b = auth("b", "workspace", "b");
        f.manager
            .import_entries(vec![entry(b, "").unwrap()])
            .unwrap();
        let held = credential_storage::lock(&f.manager.storage_path()).unwrap();
        assert!(f.manager.switch("b::workspace").is_err());
        assert_eq!(read_bounded(&f.manager.auth_path()).unwrap(), before);
        drop(held);
    }

    #[test]
    fn restore_recovers_missing_or_corrupt_auth_and_is_retryable() {
        for broken in [None, Some(b"invalid JSON".as_slice())] {
            let f = Fixture::new();
            let a = auth("a", "workspace", "a");
            let b = auth("b", "workspace", "b");
            f.active(&a);
            f.manager.save_current().unwrap();
            f.manager
                .import_entries(vec![entry(b.clone(), "").unwrap()])
                .unwrap();
            f.manager.switch(&identity(&b).unwrap().key).unwrap();
            let pending_restore = f.manager.load().unwrap();
            match broken {
                Some(bytes) => {
                    credential_storage::atomic_write(&f.manager.auth_path(), bytes).unwrap()
                }
                None => fs::remove_file(f.manager.auth_path()).unwrap(),
            }
            f.manager.restore().unwrap();
            assert_eq!(
                json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap(),
                a
            );
            assert!(!f.manager.list().unwrap().can_restore);
            // Simulate a metadata-save failure after successful auth restoration.
            f.manager.save(&pending_restore).unwrap();
            f.manager.restore().unwrap();
            assert_eq!(
                json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap(),
                a
            );
            assert!(!f.manager.list().unwrap().can_restore);
        }
    }

    #[test]
    fn recovery_rejects_inaccessible_or_non_file_auth() {
        let f = Fixture::new();
        let a = auth("a", "workspace", "a");
        let b = auth("b", "workspace", "b");
        f.active(&a);
        f.manager.save_current().unwrap();
        f.manager
            .import_entries(vec![entry(b.clone(), "").unwrap()])
            .unwrap();
        f.manager.switch(&identity(&b).unwrap().key).unwrap();
        let held = credential_storage::lock(&f.manager.auth_path()).unwrap();
        assert!(f.manager.restore().is_err());
        drop(held);
        assert_eq!(
            json_bytes(&read_bounded(&f.manager.auth_path()).unwrap()).unwrap(),
            b
        );
        fs::remove_file(f.manager.auth_path()).unwrap();
        fs::create_dir(f.manager.auth_path()).unwrap();
        assert!(f.manager.restore().is_err());
        assert!(f.manager.auth_path().is_dir());
    }

    #[test]
    fn missing_store_uses_recovery_copy_without_writing() {
        let f = Fixture::new();
        f.active(&auth("a", "workspace", "a"));
        f.manager.save_current().unwrap();
        let path = f.manager.storage_path();
        let recovery = credential_storage::recovery_path(&path);
        fs::copy(&path, &recovery).unwrap();
        let before = fs::read(&recovery).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(f.manager.list().unwrap().accounts.len(), 1);
        assert!(!path.exists());
        assert_eq!(fs::read(&recovery).unwrap(), before);
        // A corrupt primary is never silently replaced by potentially older credentials.
        fs::write(&path, b"corrupt").unwrap();
        assert!(f.manager.list().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"corrupt");
    }
}
