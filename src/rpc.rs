use crate::quota::{parse_rate_limit_response, UsageSnapshot};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default)]
pub struct AccountSummary {
    pub plan_type: Option<String>,
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
pub fn fetch_usage() -> Result<UsageResult, String> {
    let mut child = start_app_server()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Codex App Server stdin was unavailable".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Codex App Server stdout was unavailable".to_owned())?;

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
        wait_for_response(&receiver, 1, REQUEST_TIMEOUT)?;

        send(&mut stdin, &json!({"method": "initialized", "params": {}}))?;
        send(
            &mut stdin,
            &json!({
                "method": "account/read",
                "id": 2,
                "params": { "refreshToken": false }
            }),
        )?;
        send(
            &mut stdin,
            &json!({"method": "account/rateLimits/read", "id": 3}),
        )?;

        let account_response = wait_for_response(&receiver, 2, REQUEST_TIMEOUT)?;
        let limits_response = wait_for_response(&receiver, 3, REQUEST_TIMEOUT)?;
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
