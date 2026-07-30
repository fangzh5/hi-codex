# Contributing

Thanks for helping improve HiCodex.

## Development requirements

- Windows 10 or Windows 11 (x64)
- Rust stable with the MSVC target
- Visual Studio Build Tools with **Desktop development with C++** and a Windows SDK

## Checks

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Keep changes focused and include a short explanation of user-visible behavior. Please do not include account data, authentication files, tokens, or cookies in issues or pull requests.