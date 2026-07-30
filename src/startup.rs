use std::os::windows::process::CommandExt;
use std::process::Command;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "HiCodex";

fn reg_command() -> Command {
    let mut command = Command::new("reg.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub fn is_enabled() -> bool {
    reg_command()
        .args(["query", RUN_KEY, "/v", VALUE_NAME])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let status = if enabled {
        let executable = std::env::current_exe()
            .map_err(|error| format!("Executable path unavailable: {error}"))?;
        reg_command()
            .args(["add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d"])
            .arg(executable)
            .arg("/f")
            .status()
    } else {
        reg_command()
            .args(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"])
            .status()
    }
    .map_err(|error| format!("Could not update startup preference: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("Windows rejected the startup preference change".to_owned())
    }
}
