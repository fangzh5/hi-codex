# Contributing

Thanks for helping improve HiCodex.

## Development requirements

- Windows 10 or Windows 11 (x64)
- Rustup with a stable Windows toolchain
- Either Visual Studio Build Tools with **Desktop development with C++** and a Windows SDK, or an x64 MinGW-w64 toolchain that provides `windres.exe`

## Checks

```powershell
.\scripts\build.ps1
```

The build script uses an available x64 GNU `windres.exe` with Rust's GNU target. If none is found, it falls back to the MSVC target.

Keep changes focused and include a short explanation of user-visible behavior. Please do not include account data, authentication files, tokens, or cookies in issues or pull requests.
