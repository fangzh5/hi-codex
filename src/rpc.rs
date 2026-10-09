use crate::quota::{parse_rate_limit_response, UsageSnapshot};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ATTEMPTS: usize = 3;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

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

#[cfg(test)]
fn wait_for_response(
    receiver: &Receiver<Value>,
    request_id: i64,
    timeout: Duration,
) -> Result<Value, String> {
    wait_for_response_cancellable(receiver, request_id, timeout, None)
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire))
}

fn wait_for_response_cancellable(
    receiver: &Receiver<Value>,
    request_id: i64,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled(cancel) {
            return Err("Usage refresh cancelled for account management".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Codex App Server timed out".to_owned());
        }

        let wait = if cancel.is_some() {
            remaining.min(Duration::from_millis(250))
        } else {
            remaining
        };
        match receiver.recv_timeout(wait) {
            Ok(value) if value.get("id").and_then(Value::as_i64) == Some(request_id) => {
                if let Some(error) = value.get("error") {
                    return Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex App Server rejected the request")
                        .to_owned());
                }
                if value.get("result").is_none() {
                    return Err("Codex App Server response did not contain a result".into());
                }
                return Ok(value);
            }
            Ok(_) => continue,
            Err(RecvTimeoutError::Timeout) => continue,
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
fn fetch_usage_once(cancel: Option<&AtomicBool>) -> Result<UsageResult, String> {
    if cancelled(cancel) {
        return Err("Usage refresh cancelled for account management".into());
    }
    let mut child = start_app_server()?;

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Codex App Server stdin was unavailable".to_owned());
    };
    let Some(stdout) = child.stdout.take() else {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err("Codex App Server stdout was unavailable".to_owned());
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

    let result = (|| {
        send(
            &mut stdin,
            &json!({
                "method": "initialize",
                "id": 1,
                "params": {
                    "clientInfo": {
                        "name": "hi-codex",
                        "title": "HiCodex",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
        )?;
        wait_for_response_cancellable(&receiver, 1, REQUEST_TIMEOUT, cancel)?;

        send(&mut stdin, &json!({"method": "initialized", "params": {}}))?;
        send(
            &mut stdin,
            &json!({
                "method": "account/read",
                "id": 2,
                "params": { "refreshToken": false }
            }),
        )?;
        let account_response =
            wait_for_response_cancellable(&receiver, 2, REQUEST_TIMEOUT, cancel)?;
        send(
            &mut stdin,
            &json!({"method": "account/rateLimits/read", "id": 3}),
        )?;
        let limits_response = wait_for_response_cancellable(&receiver, 3, REQUEST_TIMEOUT, cancel)?;
        let usage = parse_rate_limit_response(&limits_response)?;

        Ok(UsageResult {
            usage,
            account: parse_account(&account_response),
        })
    })();

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    result
}

pub fn fetch_usage() -> Result<UsageResult, String> {
    fetch_usage_impl(None)
}

pub fn fetch_usage_cancellable(cancel: &AtomicBool) -> Result<UsageResult, String> {
    fetch_usage_impl(Some(cancel))
}

fn fetch_usage_impl(cancel: Option<&AtomicBool>) -> Result<UsageResult, String> {
    let mut last_error = None;

    for attempt in 0..MAX_ATTEMPTS {
        match fetch_usage_once(cancel) {
            Ok(result) => return Ok(result),
            Err(error) => last_error = Some(error),
        }
        if cancelled(cancel) {
            return Err("Usage refresh cancelled for account management".into());
        }

        if let Some(delay) = RETRY_DELAYS.get(attempt) {
            let deadline = Instant::now() + *delay;
            while Instant::now() < deadline {
                if cancelled(cancel) {
                    return Err("Usage refresh cancelled for account management".into());
                }
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(250)),
                );
            }
        }
    }

    Err(format!(
        "Codex usage refresh failed after {MAX_ATTEMPTS} attempts: {}",
        last_error.unwrap_or_else(|| "unknown error".to_owned())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_refresh_can_be_cancelled_for_switching() {
        let (sender, receiver) = mpsc::channel();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let worker_flag = cancel.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            worker_flag.store(true, Ordering::Release);
        });
        let error = wait_for_response_cancellable(&receiver, 1, REQUEST_TIMEOUT, Some(&cancel))
            .unwrap_err();
        assert!(error.contains("cancelled"));
        worker.join().unwrap();
        drop(sender);
    }

    #[test]
    fn rpc_errors_and_missing_results_are_not_successes() {
        for response in [
            json!({"id": 2, "error": {"message": "not signed in"}}),
            json!({"id": 2}),
        ] {
            let (sender, receiver) = mpsc::channel();
            sender.send(response).unwrap();
            assert!(wait_for_response(&receiver, 2, Duration::from_millis(10)).is_err());
        }
    }

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
