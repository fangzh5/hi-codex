use std::mem::{size_of, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{GetSystemTimes, CREATE_NO_WINDOW};

const SETTINGS_KEY: &str = r"HKCU\Software\HiCodex";
const SYSTEM_USAGE_VALUE: &str = "ShowSystemUsage";

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUsage {
    pub cpu_percent: Option<u8>,
    pub memory_percent: Option<u8>,
}

#[derive(Debug, Default)]
pub struct SystemUsageSampler {
    previous_times: Option<CpuTimes>,
}

#[derive(Clone, Copy, Debug)]
struct CpuTimes {
    idle: u64,
    kernel: u64,
    user: u64,
}

impl SystemUsageSampler {
    pub fn sample(&mut self) -> SystemUsage {
        let cpu_percent = match read_cpu_times() {
            Some(current) => self
                .previous_times
                .replace(current)
                .and_then(|previous| calculate_cpu_percent(previous, current)),
            None => None,
        };
        SystemUsage {
            cpu_percent,
            memory_percent: read_memory_percent(),
        }
    }

    pub fn reset(&mut self) {
        self.previous_times = None;
    }
}

fn filetime_to_u64(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

fn read_cpu_times() -> Option<CpuTimes> {
    let mut idle: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return None;
    }
    Some(CpuTimes {
        idle: filetime_to_u64(idle),
        kernel: filetime_to_u64(kernel),
        user: filetime_to_u64(user),
    })
}

fn calculate_cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<u8> {
    let idle = current.idle.checked_sub(previous.idle)?;
    let kernel = current.kernel.checked_sub(previous.kernel)?;
    let user = current.user.checked_sub(previous.user)?;
    let total = kernel.checked_add(user)?;
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle);
    Some(((busy.saturating_mul(100) + total / 2) / total).min(100) as u8)
}

fn read_memory_percent() -> Option<u8> {
    let mut status: MEMORYSTATUSEX = unsafe { zeroed() };
    status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        None
    } else {
        Some(status.dwMemoryLoad.min(100) as u8)
    }
}

fn reg_command() -> Command {
    let mut command = Command::new("reg.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub fn preference_enabled() -> bool {
    reg_command()
        .args(["query", SETTINGS_KEY, "/v", SYSTEM_USAGE_VALUE])
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .any(|value| value.eq_ignore_ascii_case("0x1"))
        })
        .unwrap_or(false)
}

pub fn set_preference(enabled: bool) -> Result<(), String> {
    let value = if enabled { "1" } else { "0" };
    let status = reg_command()
        .args([
            "add",
            SETTINGS_KEY,
            "/v",
            SYSTEM_USAGE_VALUE,
            "/t",
            "REG_DWORD",
            "/d",
            value,
            "/f",
        ])
        .status()
        .map_err(|error| format!("Could not update system usage preference: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Windows rejected the system usage preference change".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{calculate_cpu_percent, CpuTimes};

    #[test]
    fn calculates_cpu_usage_from_time_deltas() {
        let previous = CpuTimes {
            idle: 100,
            kernel: 200,
            user: 100,
        };
        let current = CpuTimes {
            idle: 140,
            kernel: 280,
            user: 120,
        };
        assert_eq!(calculate_cpu_percent(previous, current), Some(60));
    }

    #[test]
    fn rejects_empty_sample() {
        let sample = CpuTimes {
            idle: 100,
            kernel: 200,
            user: 100,
        };
        assert_eq!(calculate_cpu_percent(sample, sample), None);
    }
}
