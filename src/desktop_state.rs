//! Preserve local project navigation across account-scoped desktop views.
//! Only an allowlist of local navigation is merged. Cloud state, credentials,
//! draft prompts, tools/permissions and conversation ownership are never copied.
use crate::credential_storage;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider, BCRYPT_SHA256_ALGORITHM,
};

const STATE_NAME: &str = ".codex-global-state.json";
const ACCOUNT_STATE: &str = "electron-account-renderer-state-v1";
const MAX_STATE: u64 = 32 * 1024 * 1024;

pub(crate) fn owner_key(user: &str, account: &str) -> Result<String, String> {
    let input = serde_json::to_vec(&[user, account])
        .map_err(|_| "Could not encode desktop account identity")?;
    let mut hash = [0u8; 32];
    unsafe {
        let mut algorithm = std::ptr::null_mut();
        if BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0)
            < 0
        {
            return Err("Could not identify the desktop account; switch was blocked.".into());
        }
        let result = BCryptHash(
            algorithm,
            std::ptr::null(),
            0,
            input.as_ptr(),
            input.len() as u32,
            hash.as_mut_ptr(),
            hash.len() as u32,
        );
        BCryptCloseAlgorithmProvider(algorithm, 0);
        if result < 0 {
            return Err("Could not identify the desktop account; switch was blocked.".into());
        }
    }
    Ok(hash.iter().map(|byte| format!("{byte:02x}")).collect())
}

// Verified against Desktop 26.1106: these are navigation hints, not permissions
// or account migration markers. Keep all other owner-scoped values untouched.
const OWNER_FIELDS: [&str; 4] = [
    "electron-saved-workspace-roots",
    "electron-workspace-root-labels",
    "thread-workspace-root-hints",
    "thread-projectless-output-directories",
];

fn preserve_owner_navigation(state: &Value, target: Option<String>) -> Result<Value, String> {
    let root = state.as_object().ok_or("Unsupported desktop state")?;
    let mut owners = HashSet::new();
    for key in root.keys() {
        if let Some(rest) = key.strip_prefix("electron-owner-value:") {
            if let Some((_, owner)) = rest.rsplit_once(':') {
                owners.insert(owner.to_owned());
            }
        }
    }
    if let Some(owner) = root
        .get("electron-renderer-content-owner")
        .and_then(Value::as_str)
    {
        owners.insert(owner.to_owned());
    }
    if owners.is_empty() {
        return Ok(state.clone());
    }
    if let Some(target) = target {
        owners.insert(target);
    }
    let mut result = state.clone();
    for field in OWNER_FIELDS {
        let prefix = format!("electron-owner-value:{field}:");
        let mut union = Map::new();
        for (key, value) in root {
            if key == field || key.starts_with(&prefix) {
                let list = field == "electron-saved-workspace-roots";
                if (list && !value.is_array()) || (!list && !value.is_object()) {
                    return Err("Unsupported desktop navigation format; no state changed.".into());
                }
                let source = Map::from_iter([(field.to_owned(), value.clone())]);
                if list {
                    merge_list(&mut union, field, &source, |_| true);
                } else {
                    merge_map(&mut union, field, &source, |_, _| true);
                }
            }
        }
        if !union.contains_key(field) {
            continue;
        }
        for owner in &owners {
            let key = format!("{prefix}{owner}");
            let mut destination = Map::new();
            if let Some(existing) = root.get(&key) {
                destination.insert(field.to_owned(), existing.clone());
            }
            if field == "electron-saved-workspace-roots" {
                merge_list(&mut destination, field, &union, |_| true);
            } else {
                merge_map(&mut destination, field, &union, |_, _| true);
            }
            result[&key] = destination.remove(field).unwrap();
        }
    }
    Ok(result)
}

fn seed_account(state: &mut Value, user: &str, account: &str) -> Result<(), String> {
    *state = preserve_owner_navigation(state, Some(owner_key(user, account)?))?;
    if let Some(scoped) = state.get_mut(ACCOUNT_STATE) {
        let inactive = scoped["inactive"]
            .as_object_mut()
            .ok_or("Unsupported desktop account snapshots; switch was blocked.")?;
        inactive
            .entry(owner_key(user, account)?)
            .or_insert_with(|| json!({"globals":{},"atoms":{}}));
    }
    Ok(())
}

fn read_state(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not inspect desktop state; switch was blocked.".into()),
    };
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
        return Err("Desktop state must be a regular file, not a link.".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_STATE + 1).read_to_end(&mut bytes))
        .map_err(|_| "Could not read desktop state; switch was blocked.")?;
    if bytes.len() as u64 > MAX_STATE {
        return Err("Desktop state exceeds the backup limit; switch was blocked.".into());
    }
    Ok(Some(bytes))
}

fn decode(bytes: &[u8]) -> Result<Value, String> {
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .map_err(|_| "Desktop state is damaged; no login or history was changed.")?;
    if !value.is_object() {
        return Err("Unsupported desktop state; switch was blocked.".into());
    }
    Ok(value)
}

fn merge_map(
    destination: &mut Map<String, Value>,
    key: &str,
    source: &Map<String, Value>,
    accept: impl Fn(&str, &Value) -> bool,
) {
    let Some(incoming) = source.get(key).and_then(Value::as_object) else {
        return;
    };
    let value = destination
        .entry(key.to_owned())
        .or_insert_with(|| json!({}));
    let Some(target) = value.as_object_mut() else {
        return;
    };
    for (id, value) in incoming {
        if accept(id, value) {
            target.entry(id.clone()).or_insert_with(|| value.clone());
        }
    }
}

fn merge_list(
    destination: &mut Map<String, Value>,
    key: &str,
    source: &Map<String, Value>,
    accept: impl Fn(&str) -> bool,
) {
    let Some(incoming) = source.get(key).and_then(Value::as_array) else {
        return;
    };
    let value = destination
        .entry(key.to_owned())
        .or_insert_with(|| json!([]));
    let Some(target) = value.as_array_mut() else {
        return;
    };
    for item in incoming {
        if item.as_str().is_some_and(&accept) && !target.contains(item) {
            target.push(item.clone());
        }
    }
}

// Existing destination values win conflicts. This preserves each account's
// chosen project names, assignments and ordering instead of overwriting them.
fn merge_local(destination: &mut Map<String, Value>, source: &Map<String, Value>) {
    merge_map(destination, "local-projects", source, |_, value| {
        value.is_object() && value.get("rootPaths").is_some_and(Value::is_array)
    });
    let local_ids: HashSet<String> = destination
        .get("local-projects")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|map| map.keys().cloned())
        .collect();
    merge_list(
        destination,
        "electron-saved-workspace-roots",
        source,
        |_| true,
    );
    merge_map(
        destination,
        "electron-workspace-root-labels",
        source,
        |_, _| true,
    );
    merge_map(
        destination,
        "thread-project-assignments",
        source,
        |_, value| {
            value["projectKind"] == "local"
                && value["projectId"]
                    .as_str()
                    .is_some_and(|id| local_ids.contains(id))
        },
    );
    for key in ["project-order", "pinned-project-ids"] {
        merge_list(destination, key, source, |id| local_ids.contains(id));
    }
    merge_map(destination, "project-appearances", source, |id, _| {
        local_ids.contains(id)
    });
    if let Some(orders) = source
        .get("sidebar-project-thread-orders")
        .and_then(Value::as_object)
    {
        let target = destination
            .entry("sidebar-project-thread-orders".to_owned())
            .or_insert_with(|| json!({}));
        if let Some(target) = target.as_object_mut() {
            for (id, order) in orders {
                if !local_ids.contains(id) {
                    continue;
                }
                let entry = target
                    .entry(id.clone())
                    .or_insert_with(|| json!({"threadIds":[]}));
                if let (Some(entry), Some(order)) = (entry.as_object_mut(), order.as_object()) {
                    merge_list(entry, "threadIds", order, |_| true);
                }
            }
        }
    }
    let mut local_threads: HashSet<String> = destination
        .get("thread-project-assignments")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|map| map.iter())
        .filter(|(_, value)| value["projectKind"] == "local")
        .map(|(id, _)| id.clone())
        .collect();
    for map in [source, &*destination] {
        if let Some(hosts) = map
            .get("thread-project-membership-host-ids")
            .and_then(Value::as_object)
        {
            local_threads.extend(
                hosts
                    .iter()
                    .filter(|(_, host)| *host == "local")
                    .map(|(id, _)| id.clone()),
            );
        }
    }
    for key in [
        "thread-workspace-root-hints",
        "thread-projectless-output-directories",
        "thread-project-membership-host-ids",
    ] {
        merge_map(destination, key, source, |id, _| local_threads.contains(id));
    }
    merge_list(destination, "projectless-thread-ids", source, |id| {
        local_threads.contains(id)
    });
}

/// Seed the *existing* desktop account slots. The desktop itself still owns
/// selecting the account and all account-specific atoms on its next startup.
pub fn preserve_local_navigation(state: &Value) -> Result<Value, String> {
    let root = state.as_object().ok_or("Unsupported desktop state")?;
    let Some(scoped) = root.get(ACCOUNT_STATE) else {
        return preserve_owner_navigation(state, None);
    };
    if !scoped.is_object() || !(scoped["owner"].is_null() || scoped["owner"].is_string()) {
        return Err("Unsupported desktop account state; switch was blocked.".into());
    }
    let inactive = scoped["inactive"]
        .as_object()
        .ok_or("Unsupported desktop account snapshots; switch was blocked.")?;
    let mut sources = vec![root.clone()];
    for snapshot in inactive.values() {
        let globals = snapshot["globals"]
            .as_object()
            .ok_or("Invalid desktop account snapshot; switch was blocked.")?;
        if !snapshot["atoms"].is_object() {
            return Err("Invalid desktop account atoms; switch was blocked.".into());
        }
        sources.push(globals.clone());
    }
    let mut result = state.clone();
    for source in &sources {
        merge_local(result.as_object_mut().unwrap(), source);
    }
    for snapshot in result[ACCOUNT_STATE]["inactive"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        let globals = snapshot["globals"].as_object_mut().unwrap();
        for source in &sources {
            merge_local(globals, source);
        }
    }
    preserve_owner_navigation(&result, None)
}

pub struct PreparedState {
    path: PathBuf,
    original: Option<Vec<u8>>,
    replacement: Option<Vec<u8>>,
}

impl PreparedState {
    pub fn prepare(home: &Path, account: Option<(&str, &str)>) -> Result<Self, String> {
        Self::prepare_using_checkpoint(home, account, None)
    }

    pub fn prepare_repair(home: &Path, directory: &Path) -> Result<Self, String> {
        let mut checkpoint = None;
        let mut unreadable = false;
        for name in ["desktop-state-backup.dpapi", "desktop-state-previous.dpapi"] {
            if let Some(bytes) = read_state(&directory.join(name))? {
                match credential_storage::unprotect(&bytes).and_then(|plain| decode(&plain)) {
                    Ok(state) => {
                        checkpoint = Some(state);
                        break;
                    }
                    Err(_) => unreadable = true,
                }
            }
        }
        if unreadable && checkpoint.is_none() {
            return Err("The desktop checkpoint could not be decrypted or is damaged. No desktop state was changed.".into());
        }
        Self::prepare_using_checkpoint(home, None, checkpoint.as_ref())
    }

    fn prepare_using_checkpoint(
        home: &Path,
        account: Option<(&str, &str)>,
        checkpoint: Option<&Value>,
    ) -> Result<Self, String> {
        let path = home.join(STATE_NAME);
        let original = read_state(&path)?;
        let replacement = original
            .as_deref()
            .map(|bytes| -> Result<Option<Vec<u8>>, String> {
                let mut state = decode(bytes)?;
                // Validate before introducing an empty slot for a first-time login.
                preserve_local_navigation(&state)?;
                let original_state = state.clone();
                if let Some(checkpoint) = checkpoint {
                    let recovered = preserve_local_navigation(checkpoint)?;
                    let source = recovered.as_object().unwrap();
                    merge_local(state.as_object_mut().unwrap(), source);
                    if let Some(inactive) = state
                        .get_mut(ACCOUNT_STATE)
                        .and_then(|scoped| scoped.get_mut("inactive"))
                        .and_then(Value::as_object_mut)
                    {
                        for snapshot in inactive.values_mut() {
                            merge_local(snapshot["globals"].as_object_mut().unwrap(), source);
                        }
                    }
                }
                if let Some((user, account)) = account {
                    seed_account(&mut state, user, account)?;
                }
                let merged = preserve_local_navigation(&state)?;
                if merged == original_state {
                    return Ok(None);
                }
                let bytes =
                    serde_json::to_vec(&merged).map_err(|_| "Could not encode desktop state")?;
                if bytes.len() as u64 > MAX_STATE {
                    return Err("Merged desktop state exceeds the size limit".into());
                }
                Ok(Some(bytes))
            })
            .transpose()?
            .flatten();
        Ok(Self {
            path,
            original,
            replacement,
        })
    }

    pub fn backup(&self, directory: &Path) -> Result<(), String> {
        let Some(original) = &self.original else {
            return Ok(());
        };
        // Keep the preceding checkpoint too; never rely on the desktop's .bak,
        // which the desktop rewrites during normal operation.
        let path = directory.join("desktop-state-backup.dpapi");
        if let Some(previous) = read_state(&path)?.filter(|bytes| {
            credential_storage::unprotect(bytes)
                .and_then(|plain| decode(&plain))
                .is_ok()
        }) {
            credential_storage::atomic_write(
                &directory.join("desktop-state-previous.dpapi"),
                &previous,
            )?;
        }
        let encrypted = credential_storage::protect(original)?;
        credential_storage::atomic_write(&path, &encrypted)
    }

    pub fn unchanged(&self) -> Result<(), String> {
        if read_state(&self.path)? != self.original {
            return Err("Desktop state changed during switching. Fully quit Codex and retry; no login was changed.".into());
        }
        Ok(())
    }

    pub fn commit(&self) -> Result<bool, String> {
        self.unchanged()?;
        let Some(bytes) = &self.replacement else {
            return Ok(false);
        };
        credential_storage::atomic_write(&self.path, bytes)?;
        Ok(true)
    }

    pub fn rollback(&self) -> Result<(), String> {
        let Some(replacement) = &self.replacement else {
            return Ok(());
        };
        if read_state(&self.path)?.as_ref() == Some(replacement) {
            if let Some(original) = &self.original {
                credential_storage::atomic_write(&self.path, original)?;
            }
            return Ok(());
        }
        Err("Desktop state changed after the attempted switch. The encrypted checkpoint was retained; no newer desktop state was overwritten.".into())
    }
}

pub fn diagnose(home: &Path) -> Result<String, String> {
    let Some(bytes) = read_state(&home.join(STATE_NAME))? else {
        return Ok("No desktop state file; nothing to repair.".into());
    };
    let state = decode(&bytes)?;
    let repaired = preserve_local_navigation(&state)?;
    let counts = |s: &Value| {
        (
            s["local-projects"].as_object().map_or(0, Map::len),
            s["thread-project-assignments"]
                .as_object()
                .map_or(0, Map::len),
        )
    };
    let (projects, links) = counts(&state);
    let (next_projects, next_links) = counts(&repaired);
    Ok(format!("Local navigation: {projects} projects, {links} thread assignments; after repair: {next_projects} projects, {next_links} assignments. Changed: {}. Conversation databases and cloud ownership are not modified.", repaired != state))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modern_accounts_share_paths_without_copying_permissions_or_migration_flags() {
        let state = json!({
            "electron-owner-value:electron-saved-workspace-roots:a":["C:/one"],
            "electron-owner-value:electron-saved-workspace-roots:b":["C:/two"],
            "electron-owner-value:thread-workspace-root-hints:a":{"thread":"C:/one"},
            "electron-owner-value:thread-writable-roots:a":{"thread":["C:/private"]},
            "electron-owner-value:app-server-projects-migration-by-host:a":{"local":true}
        });
        let mut result = preserve_local_navigation(&state).unwrap();
        seed_account(&mut result, "new-user", "workspace").unwrap();
        let owner = owner_key("new-user", "workspace").unwrap();
        assert_eq!(
            result[format!("electron-owner-value:electron-saved-workspace-roots:{owner}")],
            json!(["C:/one", "C:/two"])
        );
        assert_eq!(
            result["electron-owner-value:thread-workspace-root-hints:b"]["thread"],
            "C:/one"
        );
        assert!(result
            .get("electron-owner-value:thread-writable-roots:b")
            .is_none());
        assert!(result
            .get("electron-owner-value:app-server-projects-migration-by-host:b")
            .is_none());
        assert_eq!(preserve_local_navigation(&result).unwrap(), result);
    }
    fn fixture() -> Value {
        json!({
            "local-projects": {"a":{"id":"a", "rootPaths":["C:/a"]}},
            "thread-project-assignments":{"ta":{"projectKind":"local","projectId":"a"}},
            "project-order":["a"],
            "electron-persisted-atom-state":{"cloud-token":"current", "draft":"private"},
            ACCOUNT_STATE:{"owner":"account-a","inactive":{
                "account-b":{"globals":{
                    "local-projects":{"b":{"id":"b","rootPaths":["C:/b"]}},
                    "thread-project-assignments":{"tb":{"projectKind":"local","projectId":"b"}, "tc":{"projectKind":"cloud","projectId":"cloud-id"}},
                    "project-order":["b","cloud-id"]
                },"atoms":{"cloud-token":"other", "draft":"other private"}}
            }}
        })
    }
    #[test]
    fn repair_preserves_account_data_and_merges_only_local_navigation() {
        let state = fixture();
        let result = preserve_local_navigation(&state).unwrap();
        assert_eq!(result["local-projects"].as_object().unwrap().len(), 2);
        assert_eq!(
            result["thread-project-assignments"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert!(result["thread-project-assignments"].get("tc").is_none());
        assert_eq!(
            result["electron-persisted-atom-state"],
            state["electron-persisted-atom-state"]
        );
        assert_eq!(
            result[ACCOUNT_STATE]["owner"],
            state[ACCOUNT_STATE]["owner"]
        );
        assert_eq!(
            result[ACCOUNT_STATE]["inactive"]["account-b"]["atoms"],
            state[ACCOUNT_STATE]["inactive"]["account-b"]["atoms"]
        );
        let globals = &result[ACCOUNT_STATE]["inactive"]["account-b"]["globals"];
        assert_eq!(globals["local-projects"].as_object().unwrap().len(), 2);
        assert_eq!(globals["project-order"], json!(["b", "cloud-id", "a"]));
        assert_eq!(preserve_local_navigation(&result).unwrap(), result);
    }
    #[test]
    fn malformed_profiles_fail_closed_and_legacy_state_is_unchanged() {
        let mut state = fixture();
        state[ACCOUNT_STATE]["inactive"]["account-b"]["globals"] = Value::Null;
        assert!(preserve_local_navigation(&state).is_err());
        state.as_object_mut().unwrap().remove(ACCOUNT_STATE);
        assert_eq!(preserve_local_navigation(&state).unwrap(), state);
    }
    #[test]
    fn first_login_and_return_to_previous_account_keep_local_threads() {
        let mut state = fixture();
        seed_account(&mut state, "user-c", "workspace-c").unwrap();
        let result = preserve_local_navigation(&state).unwrap();
        let key = owner_key("user-c", "workspace-c").unwrap();
        let next = &result[ACCOUNT_STATE]["inactive"][&key];
        assert_eq!(
            next["globals"]["thread-project-assignments"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(next["atoms"], json!({}));
        assert_eq!(key.len(), 64);
        assert_eq!(owner_key("user-c", "workspace-c").unwrap(), key);
        assert_ne!(owner_key("user-c", "workspace-d").unwrap(), key);
    }
    #[test]
    fn local_project_thread_order_is_unioned_but_cloud_order_is_not_copied() {
        let mut state = fixture();
        state["sidebar-project-thread-orders"] = json!({"a":{"threadIds":["ta"]}});
        state[ACCOUNT_STATE]["inactive"]["account-b"]["globals"]["sidebar-project-thread-orders"] = json!({
            "a":{"threadIds":["new-local", "ta"]}, "linked:cloud":{"threadIds":["private-cloud"]}
        });
        state[ACCOUNT_STATE]["inactive"]["account-b"]["globals"]
            ["thread-project-membership-host-ids"] =
            json!({"local-loose":"local","cloud-loose":"remote"});
        state[ACCOUNT_STATE]["inactive"]["account-b"]["globals"]["projectless-thread-ids"] =
            json!(["local-loose", "cloud-loose"]);
        let result = preserve_local_navigation(&state).unwrap();
        assert_eq!(
            result["sidebar-project-thread-orders"]["a"]["threadIds"],
            json!(["ta", "new-local"])
        );
        assert!(result["sidebar-project-thread-orders"]
            .get("linked:cloud")
            .is_none());
        assert_eq!(result["projectless-thread-ids"], json!(["local-loose"]));
    }
    #[test]
    fn desktop_startup_account_selection_preserves_both_profiles_after_switching_back() {
        fn select(state: &mut Value, next: Option<&str>) {
            // Reproduce the desktop's selectRendererOwner: save the outgoing
            // profile, clear globals, then load the selected inactive slot.
            let globals: Map<String, Value> = state
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| {
                    key.as_str() != ACCOUNT_STATE && key.as_str() != "electron-persisted-atom-state"
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let atoms = state["electron-persisted-atom-state"].clone();
            if let Some(owner) = state[ACCOUNT_STATE]["owner"].as_str().map(str::to_owned) {
                state[ACCOUNT_STATE]["inactive"][owner] = json!({"globals":globals,"atoms":atoms});
            }
            for key in globals.keys() {
                state.as_object_mut().unwrap().remove(key);
            }
            let saved = next.and_then(|key| {
                state[ACCOUNT_STATE]["inactive"]
                    .as_object_mut()
                    .unwrap()
                    .remove(key)
            });
            state["electron-persisted-atom-state"] = saved
                .as_ref()
                .map_or_else(|| json!({}), |s| s["atoms"].clone());
            if let Some(saved) = saved {
                state
                    .as_object_mut()
                    .unwrap()
                    .extend(saved["globals"].as_object().unwrap().clone());
            }
            state[ACCOUNT_STATE]["owner"] = next.map_or(Value::Null, |owner| json!(owner));
        }
        let original = fixture();
        let mut state = preserve_local_navigation(&original).unwrap();
        select(&mut state, None);
        select(&mut state, Some("account-b"));
        assert_eq!(
            state["thread-project-assignments"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            state["electron-persisted-atom-state"]["draft"],
            "other private"
        );
        state = preserve_local_navigation(&state).unwrap();
        select(&mut state, None);
        select(&mut state, Some("account-a"));
        assert_eq!(
            state["thread-project-assignments"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            state["electron-persisted-atom-state"],
            original["electron-persisted-atom-state"]
        );
    }
    #[test]
    fn concurrent_desktop_write_is_detected_before_commit() {
        let path = std::env::temp_dir().join(format!(
            "hicodex-state-conflict-{}.json",
            std::process::id()
        ));
        fs::write(&path, b"newer").unwrap();
        let prepared = PreparedState {
            path: path.clone(),
            original: Some(b"original".to_vec()),
            replacement: Some(b"ours".to_vec()),
        };
        assert!(prepared.commit().is_err());
        assert!(prepared.rollback().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"newer");
        fs::remove_file(path).unwrap();
    }
}
