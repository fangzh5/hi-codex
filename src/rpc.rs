use crate::quota::{parse_rate_limit_response, UsageSnapshot};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ATTEMPTS: usize = 3;
const RETRY_DELAYS: [Duration; MAX_ATTEMPTS - 1] = [Duration::from_secs(1), Duration::from_secs(3)];

static CLIENT: OnceLock<Mutex<Option<AppServerClient>>> = OnceLock::new();

#[derive(Clone, Default)]
pub struct AccountSummary {
    pub plan_type: Option<String>,
    pub email: Option<String>,
}

impl std::fmt::Debug for AccountSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountSummary")
            .field("plan_type", &self.plan_type)
            .field("email", &self.email.as_ref().map(|_| "*****"))
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct UsageResult {
    pub usage: UsageSnapshot,
    pub account: AccountSummary,
}

fn send(stdin: &mut impl Write, message: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdin, message)
        .map_err(|error| format!("Could not encode App Server request: {error}"))?;
    stdin
        .write_all(b"\n")
        .map_err(|error| format!("Could not write to Codex App Server: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("Could not flush Codex App Server request: {error}"))
}

fn wait_for_response(
    receiver: &Receiver<Value>,
    request_id: i64,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Codex App Server timed out".to_owned());
        }

        match receiver.recv_timeout(remaining) {
            Ok(value) if value.get("id").and_then(Value::as_i64) == Some(request_id) => {
                return Ok(value)
            }
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => return Err("Codex App Server timed out".to_owned()),
            Err(RecvTimeoutError::Disconnected) => {
                return Err("Codex App Server closed unexpectedly".to_owned())
            }
        }
    }
}

fn parse_account(response: &Value) -> AccountSummary {
    let account = response
        .get("result")
        .and_then(|result| result.get("account"));

    AccountSummary {
        email: account
            .and_then(|value| value.get("email"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|email| !email.is_empty())
            .map(str::to_owned),
        plan_type: account
            .and_then(|value| value.get("planType"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn codex_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(override_path) = env::var_os("HICODEX_CODEX_PATH") {
        candidates.push(PathBuf::from(override_path));
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        let bin_root = PathBuf::from(local_app_data)
            .join("OpenAI")
            .join("Codex")
            .join("bin");
        let mut app_managed = std::fs::read_dir(bin_root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("codex.exe"))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        app_managed.sort_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        app_managed.reverse();
        candidates.extend(app_managed);
    }

    candidates.push(PathBuf::from("codex.exe"));
    candidates.dedup();
    candidates
}

fn start_app_server() -> Result<Child, String> {
    let mut last_error = None;
    for candidate in codex_candidates() {
        match Command::new(&candidate)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) => last_error = Some(error),
        }
    }

    Err(format!(
        "Could not start codex app-server. Install or sign in to Codex first ({})",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no executable candidate was found".to_owned())
    ))
}

struct AppServerClient {
    child: Child,
    stdin: Option<ChildStdin>,
    receiver: Receiver<Value>,
    reader: Option<JoinHandle<()>>,
    next_request_id: i64,
}

impl AppServerClient {
    fn connect() -> Result<Self, String> {
        let mut child = start_app_server()?;
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex App Server stdin was unavailable".to_owned());
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex App Server stdout was unavailable".to_owned());
            }
        };

        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if sender.send(value).is_err() {
                        break;
                    }
                }
            }
        });

        let mut client = Self {
            child,
            stdin: Some(stdin),
            receiver,
            reader: Some(reader),
            next_request_id: 1,
        };
        let initialize_id = client.request_id();
        let initialize_result = (|| {
            client.send(&json!({
                "method": "initialize",
                "id": initialize_id,
                "params": {
                    "clientInfo": {
                        "name": "hi-codex",
                        "title": "HiCodex",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }))?;
            wait_for_response(&client.receiver, initialize_id, REQUEST_TIMEOUT)?;
            client.send(&json!({"method": "initialized", "params": {}}))
        })();

        match initialize_result {
            Ok(()) => Ok(client),
            Err(error) => {
                client.stop();
                Err(error)
            }
        }
    }

    fn request_id(&mut self) -> i64 {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        request_id
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "Codex App Server stdin was unavailable".to_owned())?;
        send(stdin, message)
    }

    fn fetch_usage(&mut self) -> Result<UsageResult, String> {
        let account_id = self.request_id();
        self.send(&json!({
            "method": "account/read",
            "id": account_id,
            "params": { "refreshToken": false }
        }))?;
        let account_response = wait_for_response(&self.receiver, account_id, REQUEST_TIMEOUT)?;

        let limits_id = self.request_id();
        self.send(&json!({
            "method": "account/rateLimits/read",
            "id": limits_id
        }))?;
        let limits_response = wait_for_response(&self.receiver, limits_id, REQUEST_TIMEOUT)?;
        let usage = parse_rate_limit_response(&limits_response)?;

        Ok(UsageResult {
            usage,
            account: parse_account(&account_response),
        })
    }

    fn stop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn client() -> &'static Mutex<Option<AppServerClient>> {
    CLIENT.get_or_init(|| Mutex::new(None))
}

pub fn fetch_usage() -> Result<UsageResult, String> {
    let mut client = client()
        .lock()
        .map_err(|_| "Codex App Server client lock was poisoned".to_owned())?;
    let mut last_error = None;

    for attempt in 0..MAX_ATTEMPTS {
        if client.is_none() {
            match AppServerClient::connect() {
                Ok(connected) => *client = Some(connected),
                Err(error) => last_error = Some(error),
            }
        }

        if let Some(connected) = client.as_mut() {
            match connected.fetch_usage() {
                Ok(result) => return Ok(result),
                Err(error) => last_error = Some(error),
            }
        }

        // A timed-out or disconnected process may still have unread responses.
        // Always discard it before retrying so the next attempt starts cleanly.
        drop(client.take());
        if let Some(delay) = RETRY_DELAYS.get(attempt) {
            std::thread::sleep(*delay);
        }
    }

    Err(format!(
        "Codex usage refresh failed after {MAX_ATTEMPTS} attempts: {}",
        last_error.unwrap_or_else(|| "unknown error".to_owned())
    ))
}

pub fn shutdown() {
    if let Some(client) = CLIENT.get() {
        if let Ok(mut client) = client.lock() {
            drop(client.take());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_response_preserves_full_identity() {
        let account = parse_account(&json!({"result": {"account": {
            "type": "chatgpt", "email": "alice123@gmail.com", "planType": "plus"
        }}}));
        assert_eq!(account.email.as_deref(), Some("alice123@gmail.com"));
        assert_eq!(account.plan_type.as_deref(), Some("plus"));
        assert!(parse_account(&json!({"result": {"account": null}}))
            .email
            .is_none());
        assert!(
            parse_account(&json!({"result": {"account": {"type": "apiKey"}}}))
                .email
                .is_none()
        );
    }
}
