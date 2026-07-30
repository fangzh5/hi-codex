$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
$rustup = Get-Command rustup -ErrorAction SilentlyContinue
if (-not $cargo -or -not $rustup) {
    throw 'Rustup is required. Install it with: winget install Rustlang.Rustup'
}

Push-Location $root
try {
    & $rustup.Source toolchain install stable-x86_64-pc-windows-msvc --profile minimal
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not prepare the MSVC Rust toolchain.'
    }

    & $cargo.Source +stable-x86_64-pc-windows-msvc test
    if ($LASTEXITCODE -ne 0) {
        throw 'Tests failed. Install Visual Studio Build Tools with the Desktop development with C++ workload.'
    }

    & $cargo.Source +stable-x86_64-pc-windows-msvc clippy --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) {
        throw 'Clippy checks failed.'
    }

    & $cargo.Source +stable-x86_64-pc-windows-msvc build --release
    if ($LASTEXITCODE -ne 0) {
        throw 'Release build failed.'
    }

    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    Copy-Item -Force (Join-Path $root 'target\release\hicodex.exe') (Join-Path $dist 'HiCodex.exe')
    Get-Item (Join-Path $dist 'HiCodex.exe') | Select-Object FullName,Length,LastWriteTime
}
finally {
    Pop-Location
}