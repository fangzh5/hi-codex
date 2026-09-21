$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
$rustup = (Get-Command rustup -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    $candidate = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path $candidate -PathType Leaf) {
        $cargo = $candidate
    }
}
if (-not $rustup) {
    $candidate = Join-Path $env:USERPROFILE '.cargo\bin\rustup.exe'
    if (Test-Path $candidate -PathType Leaf) {
        $rustup = $candidate
    }
}
if (-not $cargo -or -not $rustup) {
    throw 'Rustup is required. Install it with: winget install Rustlang.Rustup'
}

$windresCandidates = @()
if ($env:HICODEX_WINDRES) {
    $windresCandidates += $env:HICODEX_WINDRES
}
if ($env:WINDRES) {
    $windresCandidates += $env:WINDRES
}
$namedWindres = Get-Command x86_64-w64-mingw32-windres.exe -ErrorAction SilentlyContinue
if ($namedWindres) {
    $windresCandidates += $namedWindres.Source
}
$localToolchains = Join-Path $env:USERPROFILE '.local\toolchains'
if (Test-Path $localToolchains) {
    $windresCandidates += Get-ChildItem $localToolchains -Filter windres.exe -File -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match 'mingw64\\bin\\windres\.exe$' } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -ExpandProperty FullName
}

$windres = $null
foreach ($candidate in $windresCandidates | Select-Object -Unique) {
    if (-not (Test-Path $candidate -PathType Leaf)) {
        continue
    }
    $description = & $candidate --version 2>$null | Select-Object -First 1
    if ($description -match 'x86_64') {
        $windres = $candidate
        break
    }
}

if ($windres) {
    $toolchain = 'stable-x86_64-pc-windows-gnu'
    $env:WINDRES = $windres
    Write-Host "Using GNU Rust toolchain with $windres"
}
else {
    $toolchain = 'stable-x86_64-pc-windows-msvc'
    Write-Host 'No x64 windres found; using the MSVC Rust toolchain.'
}

Push-Location $root
try {
    & $rustup toolchain install $toolchain --profile minimal
    if ($LASTEXITCODE -ne 0) {
        throw "Could not prepare the $toolchain Rust toolchain."
    }

    & $rustup component add rustfmt clippy --toolchain $toolchain
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not prepare Rustfmt and Clippy.'
    }

    & $cargo "+$toolchain" fmt --all -- --check
    if ($LASTEXITCODE -ne 0) {
        throw 'Formatting checks failed.'
    }

    & $cargo "+$toolchain" test --locked
    if ($LASTEXITCODE -ne 0) {
        throw 'Tests failed. Install Visual Studio Build Tools for MSVC, or provide an x64 GNU windres executable.'
    }

    & $cargo "+$toolchain" clippy --locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) {
        throw 'Clippy checks failed.'
    }

    & $cargo "+$toolchain" build --release --locked
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
