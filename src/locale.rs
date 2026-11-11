//! Language preference is independent of credentials and conversation storage.
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
static CHINESE: AtomicBool = AtomicBool::new(false);
fn command() -> std::process::Command {
    let mut c = std::process::Command::new("reg.exe");
    c.creation_flags(0x08000000);
    c
}
pub fn is_chinese() -> bool {
    CHINESE.load(Ordering::Relaxed)
}
pub fn load() {
    let enabled = command()
        .args(["query", r"HKCU\Software\HiCodex", "/v", "ChineseUI"])
        .output()
        .is_ok_and(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .any(|v| v.eq_ignore_ascii_case("0x1"))
        });
    CHINESE.store(enabled, Ordering::Relaxed);
}
pub fn set(enabled: bool) -> Result<(), String> {
    let result = command()
        .args([
            "add",
            r"HKCU\Software\HiCodex",
            "/v",
            "ChineseUI",
            "/t",
            "REG_DWORD",
            "/d",
            if enabled { "1" } else { "0" },
            "/f",
        ])
        .output()
        .map_err(|_| "Could not save language preference")?;
    if !result.status.success() {
        return Err("Could not save language preference".into());
    }
    CHINESE.store(enabled, Ordering::Relaxed);
    Ok(())
}
pub fn text(value: &str) -> &str {
    if !is_chinese() {
        return value;
    }
    match value {
        "Refresh all" => "刷新全部",
        "Usage dashboard" => "用量详情",
        "Accounts" => "账号管理",
        "Preferences" => "偏好设置",
        "Language" => "语言",
        "Sign in..." => "登录...",
        "Import JSON..." => "导入 JSON...",
        "Export JSON..." => "导出 JSON...",
        "Remove account" => "删除账号",
        "Start with Windows" => "开机启动",
        "Show CPU and RAM" => "显示 CPU 和内存",
        "Center taskbar icons" => "任务栏图标居中",
        "About HiCodex" => "关于 HiCodex",
        "Exit HiCodex" => "退出 HiCodex",
        "Cancel sign-in" => "取消登录",
        "No saved accounts" => "暂无账号",
        "Storage error..." => "账号存储错误...",
        "Week" => "本周",
        "5 hours" => "5 小时",
        "Switch" => "切换",
        "Switch on" => "切换已开启",
        "Failed" => "失败",
        "Just now" => "刚刚更新",
        "Cancel" => "取消",
        "Hide" => "隐藏",
        "Show" => "显示",
        "Refresh" => "刷新",
        "Refreshing..." => "刷新中…",
        _ => value,
    }
}
