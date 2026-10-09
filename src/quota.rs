use serde_json::Value;
use std::collections::HashSet;

pub const FIVE_HOUR_MINUTES: u64 = 300;
pub const WEEK_MINUTES: u64 = 10_080;

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaWindow {
    pub used_percent: f64,
    pub window_duration_minutes: u64,
    pub resets_at: i64,
}

impl QuotaWindow {
    pub fn remaining_percent(&self) -> u8 {
        (100.0 - self.used_percent).clamp(0.0, 100.0).round() as u8
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSnapshot {
    pub five_hour: Option<QuotaWindow>,
    pub weekly: Option<QuotaWindow>,
    pub other: Vec<QuotaWindow>,
    pub reset_credits: Option<u32>,
}

impl UsageSnapshot {
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.weekly.is_none() && self.other.is_empty()
    }
}

fn number_as_f64(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn number_as_u64(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .or_else(|| value.and_then(Value::as_f64).map(|number| number as u64))
}

fn number_as_i64(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_i64)
        .or_else(|| value.and_then(Value::as_u64).map(|number| number as i64))
        .or_else(|| value.and_then(Value::as_f64).map(|number| number as i64))
}

fn collect_windows(value: &Value, output: &mut Vec<QuotaWindow>) {
    match value {
        Value::Object(object) => {
            let used = number_as_f64(object.get("usedPercent"));
            let duration = number_as_u64(object.get("windowDurationMins"));
            let reset = number_as_i64(object.get("resetsAt"));

            if let (Some(used_percent), Some(window_duration_minutes), Some(resets_at)) =
                (used, duration, reset)
            {
                output.push(QuotaWindow {
                    used_percent,
                    window_duration_minutes,
                    resets_at,
                });
            }

            for child in object.values() {
                collect_windows(child, output);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_windows(item, output);
            }
        }
        _ => {}
    }
}

pub fn parse_rate_limit_response(response: &Value) -> Result<UsageSnapshot, String> {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex App Server returned an error");
        return Err(message.to_owned());
    }

    let result = response
        .get("result")
        .ok_or_else(|| "Rate-limit response did not contain a result".to_owned())?;

    let mut collected = Vec::new();
    // Select the Codex bucket before inspecting windows. Never combine windows
    // from different model buckets just because their durations match.
    let bucket = result
        .get("rateLimitsByLimitId")
        .and_then(|buckets| buckets.get("codex"))
        .filter(|bucket| bucket.is_object())
        .or_else(|| {
            result.get("rateLimits").filter(|bucket| {
                bucket.is_object()
                    && bucket
                        .get("limitId")
                        .and_then(Value::as_str)
                        .is_none_or(|id| id == "codex")
            })
        });
    if let Some(bucket) = bucket {
        collect_windows(bucket, &mut collected);
    }

    let mut seen = HashSet::new();
    collected.retain(|window| {
        seen.insert((
            window.window_duration_minutes,
            window.resets_at,
            window.used_percent.round() as i64,
        ))
    });

    let reset_credits = result
        .get("rateLimitResetCredits")
        .and_then(|value| value.get("availableCount"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let mut snapshot = UsageSnapshot {
        reset_credits,
        ..UsageSnapshot::default()
    };
    for window in collected {
        match window.window_duration_minutes {
            FIVE_HOUR_MINUTES if snapshot.five_hour.is_none() => snapshot.five_hour = Some(window),
            WEEK_MINUTES if snapshot.weekly.is_none() => snapshot.weekly = Some(window),
            _ => snapshot.other.push(window),
        }
    }

    if snapshot.is_empty() {
        Err("No Codex usage windows were returned for this account".to_owned())
    } else {
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chooses_codex_bucket_without_mixing_model_limits() {
        let window = |used, minutes| json!({"usedPercent": used, "windowDurationMins": minutes, "resetsAt": 2000000000});
        let response = json!({"result": {
            "rateLimits": {"primary": window(90, 300)},
            "rateLimitsByLimitId": {
                "a_other_model": {"primary": window(99, 300), "secondary": window(99, 10080)},
                "codex": {"primary": window(20, 300), "secondary": null}
            }
        }});
        let snapshot = parse_rate_limit_response(&response).unwrap();
        assert_eq!(snapshot.five_hour.unwrap().remaining_percent(), 80);
        assert!(snapshot.weekly.is_none());
        assert!(snapshot.other.is_empty());
        assert!(parse_rate_limit_response(&json!({"result": {
            "rateLimitsByLimitId": {"a_other_model": {"primary": window(99, 300)}}
        }}))
        .is_err());
    }

    #[test]
    fn parses_five_hour_and_weekly_windows_by_duration() {
        let response = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 25.4,
                        "windowDurationMins": 300,
                        "resetsAt": 2000000000
                    },
                    "secondary": {
                        "usedPercent": 42,
                        "windowDurationMins": 10080,
                        "resetsAt": 2000600000
                    }
                }
            }
        });

        let snapshot = parse_rate_limit_response(&response).unwrap();
        assert_eq!(snapshot.five_hour.unwrap().remaining_percent(), 75);
        assert_eq!(snapshot.weekly.unwrap().remaining_percent(), 58);
    }

    #[test]
    fn missing_five_hour_window_is_not_treated_as_zero() {
        let response = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 10,
                        "windowDurationMins": 10080,
                        "resetsAt": 2000600000
                    },
                    "secondary": null
                }
            }
        });

        let snapshot = parse_rate_limit_response(&response).unwrap();
        assert!(snapshot.five_hour.is_none());
        assert_eq!(snapshot.weekly.unwrap().remaining_percent(), 90);
    }

    #[test]
    fn parses_reset_credit_count() {
        let response = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 10,
                        "windowDurationMins": 10080,
                        "resetsAt": 2000600000
                    }
                },
                "rateLimitResetCredits": {
                    "availableCount": 2
                }
            }
        });

        let snapshot = parse_rate_limit_response(&response).unwrap();
        assert_eq!(snapshot.reset_credits, Some(2));
    }

    #[test]
    fn deduplicates_compatibility_and_multi_bucket_views() {
        let response = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 50,
                        "windowDurationMins": 300,
                        "resetsAt": 2000000000
                    }
                },
                "rateLimitsByLimitId": {
                    "codex": {
                        "primary": {
                            "usedPercent": 50,
                            "windowDurationMins": 300,
                            "resetsAt": 2000000000
                        }
                    }
                }
            }
        });

        let snapshot = parse_rate_limit_response(&response).unwrap();
        assert!(snapshot.five_hour.is_some());
        assert!(snapshot.other.is_empty());
    }
}
