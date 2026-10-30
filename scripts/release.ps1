# Builds a release with local paths stripped from the binaries and stages dist/<version>-<gitsha>/:
# the public zip (exes, licences, SHA256SUMS.txt) plus symbols/ (PDBs, keep private for crash analysis).
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$ver = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$sha = (git rev-parse --short=12 HEAD).Trim()
if (git status --porcelain) { $sha += '-dirty' }
# Remap absolute source / registry paths (they contain the user name) and store only the PDB file name.
$env:RUSTFLAGS = "--remap-path-prefix=$root=mouser-rs --remap-path-prefix=$env:USERPROFILE\.cargo=cargo " +
    "--remap-path-prefix=$env:USERPROFILE\.rustup=rustup -C link-arg=/PDBALTPATH:%_PDB%"
$env:CARGO_TARGET_DIR = Join-Path $root 'target/publish'
cargo build --release -p mouser-agent -p mouser-gui
if ($LASTEXITCODE) { throw 'cargo build failed' }
$td = $env:CARGO_TARGET_DIR
$rel = Join-Path $td 'release'
$out = Join-Path $root "dist/$ver-$sha"
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item $out -ItemType Directory | Out-Null
foreach ($f in 'mouser-agent.exe', 'mouser-gui.exe') { Copy-Item (Join-Path $rel $f) $out }
$sym = Join-Path $out 'symbols'
New-Item $sym -ItemType Directory | Out-Null
foreach ($f in 'mouser_agent.pdb', 'mouser_gui.pdb') { Copy-Item (Join-Path $rel $f) $sym }
# Refuse to ship a binary that still names this machine's user profile.
foreach ($f in 'mouser-agent.exe', 'mouser-gui.exe') {
    $bytes = [IO.File]::ReadAllBytes((Join-Path $out $f))
    if ([Text.Encoding]::ASCII.GetString($bytes).Contains($env:USERPROFILE)) { throw "$f still contains $env:USERPROFILE" }
}
foreach ($f in 'LICENSE', 'NOTICE', 'crates/mouser-gui/assets/MOUSER-LICENSE.txt') {
    if (Test-Path $f) { Copy-Item $f $out }
}
Get-ChildItem $out -File | Where-Object Name -ne 'SHA256SUMS.txt' | Sort-Object Name | ForEach-Object {
    '{0} *{1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
} | Set-Content (Join-Path $out 'SHA256SUMS.txt') -Encoding ascii
$zip = Join-Path $root "dist/mouser-rs-v$ver-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Get-ChildItem $out -File).FullName -DestinationPath $zip
Write-Host "Release staged in $out; public zip $zip"
