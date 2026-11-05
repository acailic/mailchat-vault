<#
.SYNOPSIS
    The Windows column of the release validation matrix (roadmap M6.4;
    see docs/validation-and-release.md, "Release validation matrix").

.DESCRIPTION
    MUST RUN ON Windows — it builds the real msi/nsis installers and
    drives scripts/windows-smoke.ps1 (real installs need real Windows;
    the Linux matrix harness cannot substitute for it).

    Legs, in order:

      suite    the full test suite, host toolchain (the same command
               set as the CI `test` job / the Linux suite leg):
                 npm ci; npm run check; npm test; npm run build;
                 cargo fmt --all -- --check;
                 cargo clippy --workspace --all-targets -- -D warnings;
                 cargo test --workspace;
                 cargo build -p mailchat-vault --features tauri/custom-protocol
      bundles  the release installers exactly the way the release
               workflow builds them:
                 npm run tauri build -- --bundles msi,nsis
      smoke    scripts/windows-smoke.ps1 against the just-built msi/exe
               (fresh install -> launch -> alive -> vault.db created ->
               uninstall leaves the archive). With -OldMsi/-OldExe the
               in-place upgrade phases run as well (needs -InspectExe
               or -BuildInspect). With -Published, the leg instead runs
               the release-fix one-command route
               scripts/release/windows-smoke-from-release.ps1 against a
               PUBLISHED release (-Tag, optionally -OldTag), which is
               the preferred smoke once that tooling is on the branch.

    The desktop e2e route (tests/e2e) is Linux-only today — the
    WebDriver stack it drives is the Linux WebKitGTK one — so it is NOT
    a Windows matrix leg; the Windows column of the matrix records it
    as not applicable rather than silently green.

    Prerequisites: Windows 10/11 with WebView2 runtime (stock on Win11),
    node 22, rust stable, git, pwsh 7+.

.EXAMPLE
    pwsh scripts/release/matrix-windows.ps1
    # full column: suite + bundles + fresh-install smoke

.EXAMPLE
    pwsh scripts/release/matrix-windows.ps1 -SkipSuite
    # iterate on bundles+smoke only

.EXAMPLE
    pwsh scripts/release/matrix-windows.ps1 -OldTag v0.1.0 -BuildInspect
    # upgrade phases against the published v0.1.0 installers

.EXAMPLE
    pwsh scripts/release/matrix-windows.ps1 -Published -Tag v0.1.0
    # smoke the PUBLISHED v0.1.0 installers via the release-fix
    # one-command route (requires windows-smoke-from-release.ps1)
#>
[CmdletBinding()]
param(
    [string] $LogsDir = '',
    [switch] $SkipSuite,
    [switch] $SkipBundles,
    [switch] $SkipSmoke,
    # -- smoke leg inputs ------------------------------------------------
    # Locally-built smoke (default): upgrade phases enabled when OldMsi/
    # OldExe are provided (plus -InspectExe or -BuildInspect).
    [string] $OldMsi = '',
    [string] $OldExe = '',
    [string] $InspectExe = '',
    [switch] $BuildInspect,
    # Published-release smoke (release-fix route): downloads the tag's
    # installers and drives windows-smoke-from-release.ps1 with them.
    [switch] $Published,
    [string] $Tag = '',
    [string] $OldTag = '',
    [switch] $AllowDataWipe
)

$ErrorActionPreference = 'Stop'
if (-not [System.Runtime.InteropServices.RuntimeInformation]::OSDescription.Contains('Windows')) {
    Write-Error "matrix-windows.ps1 must run on Windows (it builds/installs real Windows artifacts)"
}
$repo = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $LogsDir) {
    $LogsDir = Join-Path $repo ("target\release\matrix-logs\{0}-windows" -f (Get-Date -AsUTC -Format 'yyyyMMddTHHmmssZ'))
}
New-Item -ItemType Directory -Force -Path $LogsDir | Out-Null

$overall = 0
$summary = New-Object System.Collections.Generic.List[string]
function Record([string] $leg, [string] $status) {
    $script:summary.Add(('{0,-10} {1}' -f $leg, $status))
}

# Run a list of steps (name/command/args), logging each to $logFile,
# stopping at the first non-zero exit. Returns $true when all passed.
function Run-Steps([string] $logFile, $steps) {
    "" | Set-Content $logFile
    Push-Location $script:repo
    try {
        foreach ($s in $steps) {
            $line = "== [{0}] {1}" -f $s.leg, $s.n
            Write-Host $line
            Add-Content $logFile $line
            & $s.c @($s.a) 2>&1 | Tee-Object -FilePath $logFile -Append
            if ($LASTEXITCODE -ne 0) {
                Add-Content $logFile ("step FAILED with exit {0}" -f $LASTEXITCODE)
                return $false
            }
        }
        return $true
    } finally {
        Pop-Location
    }
}

Write-Host "== matrix-windows: $((Get-Date).AsUTC.ToString('yyyy-MM-ddTHH:mm:ssZ'))"
Write-Host "   repo:  $repo @ $(git -C $repo rev-parse --short HEAD)"
Write-Host "   host:  $([System.Runtime.InteropServices.RuntimeInformation]::OSDescription) ($env:PROCESSOR_ARCHITECTURE)"
Write-Host "   logs:  $LogsDir"
@(
    "matrix-windows $((Get-Date).AsUTC.ToString('yyyy-MM-ddTHH:mm:ssZ'))",
    "repo $(git -C $repo rev-parse HEAD)",
    [System.Runtime.InteropServices.RuntimeInformation]::OSDescription,
    "node $(node --version)",
    "rustc $(rustc --version)"
) | Set-Content (Join-Path $LogsDir 'environment.txt')

# ----------------------------------------------------------------- suite
if ($SkipSuite) {
    Record 'suite' 'SKIPPED (-SkipSuite)'
} else {
    Write-Host ""
    Write-Host "==== leg 1/3: full test suite (host toolchain)"
    $suiteSteps = @(
        @{ leg = 'suite'; n = 'npm ci';                              c = 'npm';   a = @('ci', '--no-audit', '--no-fund') },
        @{ leg = 'suite'; n = 'npm run check (tsc)';                  c = 'npm';   a = @('run', 'check') },
        @{ leg = 'suite'; n = 'npm test (vitest)';                    c = 'npm';   a = @('test') },
        @{ leg = 'suite'; n = 'npm run build (frontend dist)';        c = 'npm';   a = @('run', 'build') },
        @{ leg = 'suite'; n = 'cargo fmt --all -- --check';           c = 'cargo'; a = @('fmt', '--all', '--', '--check') },
        @{ leg = 'suite'; n = 'cargo clippy --workspace --all-targets -- -D warnings'; c = 'cargo'; a = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
        @{ leg = 'suite'; n = 'cargo test --workspace';               c = 'cargo'; a = @('test', '--workspace') },
        @{ leg = 'suite'; n = 'cargo build -p mailchat-vault --features tauri/custom-protocol'; c = 'cargo'; a = @('build', '-p', 'mailchat-vault', '--features', 'tauri/custom-protocol') }
    )
    if (Run-Steps (Join-Path $LogsDir 'suite.log') $suiteSteps) {
        Record 'suite' 'PASS'
    } else {
        Write-Host "error: suite leg failed; see $LogsDir\suite.log"
        Record 'suite' "FAIL (see $LogsDir\suite.log)"
        $overall = 1
    }
}

# -------------------------------------------------------------- bundles
$msi = ''; $exe = ''
if ($SkipBundles) {
    Record 'bundles' 'SKIPPED (-SkipBundles)'
} else {
    Write-Host ""
    Write-Host "==== leg 2/3: release installers (tauri build, as the release workflow does)"
    $bundleSteps = @(
        @{ leg = 'bundles'; n = 'npm run tauri build -- --bundles msi,nsis'; c = 'npm'; a = @('run', 'tauri', 'build', '--', '--bundles', 'msi,nsis') }
    )
    if (Run-Steps (Join-Path $LogsDir 'bundles.log') $bundleSteps) {
        $msi = Get-ChildItem "$repo\target\release\bundle\msi\*.msi" -ErrorAction SilentlyContinue |
               Select-Object -First 1 -ExpandProperty FullName
        $exe = Get-ChildItem "$repo\target\release\bundle\nsis\*-setup.exe" -ErrorAction SilentlyContinue |
               Select-Object -First 1 -ExpandProperty FullName
        if ($msi -and $exe) {
            Write-Host "   built: $msi"
            Write-Host "          $exe"
            Record 'bundles' 'PASS'
        } else {
            Write-Host "error: tauri build succeeded but msi/exe not found under target\release\bundle\"
            Record 'bundles' 'FAIL (no msi/exe)'
            $overall = 1
        }
    } else {
        Write-Host "error: bundle build failed; see $LogsDir\bundles.log"
        Record 'bundles' "FAIL (see $LogsDir\bundles.log)"
        $overall = 1
    }
}

# ---------------------------------------------------------------- smoke
if ($SkipSmoke) {
    Record 'smoke' 'SKIPPED (-SkipSmoke)'
} else {
    Write-Host ""
    Write-Host "==== leg 3/3: installer smoke"
    $fromRelease = Join-Path $PSScriptRoot 'windows-smoke-from-release.ps1'
    if ($Published) {
        if (-not (Test-Path $fromRelease)) {
            Write-Host "error: -Published needs scripts/release/windows-smoke-from-release.ps1"
            Write-Host "       (release-fix tooling) on this branch"
            Record 'smoke' 'FAIL (-Published but release-fix tools not on this branch)'
            $overall = 1
        } elseif (-not $Tag) {
            Write-Host "error: -Published requires -Tag <published release tag>"
            Record 'smoke' 'FAIL (-Published without -Tag)'
            $overall = 1
        } else {
            $frArgs = @('-Tag', $Tag)
            if ($OldTag)       { $frArgs += @('-OldTag', $OldTag) }
            if ($BuildInspect) { $frArgs += '-BuildInspect' }
            if ($InspectExe)   { $frArgs += @('-InspectExe', $InspectExe) }
            if ($AllowDataWipe){ $frArgs += '-AllowDataWipe' }
            & $fromRelease @frArgs 2>&1 | Tee-Object -FilePath (Join-Path $LogsDir 'smoke.log')
            if ($LASTEXITCODE -eq 0) {
                Record 'smoke' 'PASS (published release smoke)'
            } else {
                Write-Host "error: published-release smoke failed; see $LogsDir\smoke.log"
                Record 'smoke' "FAIL (see $LogsDir\smoke.log)"
                $overall = 1
            }
        }
    } else {
        if (-not $msi -or -not $exe) {
            $msi = Get-ChildItem "$repo\target\release\bundle\msi\*.msi" -ErrorAction SilentlyContinue |
                   Select-Object -First 1 -ExpandProperty FullName
            $exe = Get-ChildItem "$repo\target\release\bundle\nsis\*-setup.exe" -ErrorAction SilentlyContinue |
                   Select-Object -First 1 -ExpandProperty FullName
        }
        if (-not $msi -or -not $exe) {
            Write-Host "error: no msi/exe to smoke — run the bundles leg (or drop -SkipBundles) first"
            Record 'smoke' 'FAIL (no msi/exe)'
            $overall = 1
        } else {
            $smoke = Join-Path $repo 'scripts\windows-smoke.ps1'
            $smokeArgs = @{
                NewMsi = $msi; NewExe = $exe
                OldMsi = $OldMsi; OldExe = $OldExe
                InspectExe = $InspectExe
                LogDir = (Join-Path $LogsDir 'windows-smoke-logs')
            }
            if ($BuildInspect -and -not $InspectExe) {
                Write-Host "== building smoke_inspect (cargo)"
                cargo build --release -p vault-core --example smoke_inspect
                if ($LASTEXITCODE -ne 0) {
                    Write-Host "error: smoke_inspect build failed"
                    Record 'smoke' 'FAIL (smoke_inspect build)'
                    $overall = 1
                } else {
                    $smokeArgs.InspectExe = Join-Path $repo 'target\release\examples\smoke_inspect.exe'
                }
            }
            if ($overall -eq 0) {
                if ($AllowDataWipe) { $smokeArgs.AllowDataWipe = $true }
                $upgradeNote = if ($OldMsi -and $OldExe) { 'fresh+upgrade' } else { 'fresh phases only (-OldMsi/-OldExe for upgrade)' }
                & $smoke @smokeArgs 2>&1 | Tee-Object -FilePath (Join-Path $LogsDir 'smoke.log')
                if ($LASTEXITCODE -eq 0) {
                    Record 'smoke' "PASS ($upgradeNote)"
                } else {
                    Write-Host "error: windows-smoke failed; see $LogsDir\smoke.log"
                    Record 'smoke' "FAIL (see $LogsDir\smoke.log)"
                    $overall = 1
                }
            }
        }
    }
}

# -------------------------------------------------------------- summary
Write-Host ""
Write-Host "==================== Windows matrix summary ===================="
$summary | ForEach-Object { Write-Host $_ }
Write-Host "logs: $LogsDir"
if ($overall -eq 0) {
    Write-Host "RESULT: PASS — requested Windows legs green"
} else {
    Write-Host "RESULT: FAIL — at least one leg failed"
}
Write-Host "================================================================="
exit $overall
