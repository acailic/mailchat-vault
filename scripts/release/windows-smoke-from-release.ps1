<#
.SYNOPSIS
    One-command Windows release smoke against a PUBLISHED release — the
    local (no-CI) route for the M6.2 gate while repo Actions are
    disabled by decision.

.DESCRIPTION
    Downloads the installers of a published GitHub release and drives
    scripts/windows-smoke.ps1 with them, so the msi/nsis fresh-install
    phases (install -> launch -> alive -> vault.db created -> uninstall
    leaves the archive) run against exactly the bytes users download.

    By default runs the FRESH phases only (no Rust toolchain needed on
    the machine). With -OldTag <tag> it also downloads an older
    published release and runs the in-place upgrade phases; those need
    the smoke_inspect digest tool, built automatically with -BuildInspect
    (requires cargo) or passed explicitly via -InspectExe.

    Run it on any Windows box (WebView2 runtime present, gh
    authenticated):

      pwsh scripts/release/windows-smoke-from-release.ps1 -Tag v0.1.0

    The gate for publishing a release stays what docs/release.md
    names: green fresh+upgrade phases on both installers.

.PARAMETER Tag
    Published release tag whose msi/exe are smoked as the NEW bundles.

.PARAMETER OldTag
    Optional older published release to upgrade FROM (enables the
    upgrade phases). Defaults to '' (fresh phases only).

.PARAMETER BundlesDir
    Where the downloaded installers land. Default: target\release\verify\<tag>.

.PARAMETER InspectExe
    Path to a prebuilt smoke_inspect (cargo build --release -p
    vault-core --example smoke_inspect). With -BuildInspect it is built
    into the default cargo location instead.

.PARAMETER BuildInspect
    Build smoke_inspect with cargo before running (upgrade phases only).

.PARAMETER AllowDataWipe
    Passthrough consent for scripts/windows-smoke.ps1 to wipe
    %APPDATA%\com.acailic.mailchatvault (it refuses to start without it
    if a vault.db is already there).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Tag,
    [string] $OldTag = '',
    [string] $BundlesDir = '',
    [string] $InspectExe = '',
    [switch] $BuildInspect,
    [switch] $AllowDataWipe
)

$ErrorActionPreference = 'Stop'
if (($BuildInspect -or $InspectExe) -and -not $OldTag) {
    throw "-BuildInspect/-InspectExe require -OldTag (the upgrade phases that consume them only run with -OldTag)"
}
$repo = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$smoke = Join-Path $repo 'scripts\windows-smoke.ps1'

if (-not $BundlesDir) {
    $BundlesDir = Join-Path $repo ("target\release\verify\{0}" -f $Tag)
}
New-Item -ItemType Directory -Force -Path $BundlesDir | Out-Null

Write-Host "== downloading published installers for $Tag"
gh release download $Tag --repo (gh repo view --json nameWithOwner -q nameWithOwner) `
    --pattern '*_x64_en-US.msi' --pattern '*_x64-setup.exe' `
    --dir $BundlesDir --clobber

$newMsi = Get-ChildItem "$BundlesDir\*_x64_en-US.msi" | Select-Object -First 1 -ExpandProperty FullName
$newExe = Get-ChildItem "$BundlesDir\*_x64-setup.exe" | Select-Object -First 1 -ExpandProperty FullName
if (-not $newMsi -or -not $newExe) {
    throw "release $Tag did not provide both an msi and a setup exe under $BundlesDir"
}

$oldMsi = ''; $oldExe = ''
if ($OldTag) {
    $oldDir = Join-Path $repo ("target\release\verify\{0}" -f $OldTag)
    New-Item -ItemType Directory -Force -Path $oldDir | Out-Null
    Write-Host "== downloading OLD installers for upgrade phases ($OldTag)"
    gh release download $OldTag --repo (gh repo view --json nameWithOwner -q nameWithOwner) `
        --pattern '*_x64_en-US.msi' --pattern '*_x64-setup.exe' `
        --dir $oldDir --clobber
    $oldMsi = Get-ChildItem "$oldDir\*_x64_en-US.msi" | Select-Object -First 1 -ExpandProperty FullName
    $oldExe = Get-ChildItem "$oldDir\*_x64-setup.exe" | Select-Object -First 1 -ExpandProperty FullName
    if (-not $oldMsi -or -not $oldExe) {
        throw "old release $OldTag did not provide both installers under $oldDir"
    }
    if ($BuildInspect -and -not $InspectExe) {
        Write-Host "== building smoke_inspect (cargo)"
        cargo build --release -p vault-core --example smoke_inspect
        $InspectExe = Join-Path $repo 'target\release\examples\smoke_inspect.exe'
    }
}

Write-Host "== running windows-smoke.ps1 against the published $Tag installers"
$smokeArgs = @{
    NewMsi = $newMsi; NewExe = $newExe
    OldMsi = $oldMsi;  OldExe = $oldExe
    InspectExe = $InspectExe
    LogDir = (Join-Path $BundlesDir 'windows-smoke-logs')
}
if ($AllowDataWipe) { $smokeArgs.AllowDataWipe = $true }
& $smoke @smokeArgs
exit $LASTEXITCODE
