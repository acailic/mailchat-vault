<#
.SYNOPSIS
    Windows installer smoke test for mailchat-vault: install, launch,
    uninstall — through BOTH installers (WiX .msi and NSIS .exe) — plus
    an in-place upgrade from an older release whose archive must survive
    intact.

.DESCRIPTION
    Runs on a GitHub Actions Windows runner (or any Windows box with the
    bundles on disk). Four phases:

      1. msi-fresh    install the .msi -> app exe exists -> launch ->
                      app stays alive $AliveSeconds -> first launch
                      created vault.db -> kill -> uninstall -> exe gone
                      and vault.db SURVIVED the uninstall
      2. nsis-fresh   same contract through the NSIS setup .exe (/S)
      3. msi-upgrade  install the OLD .msi -> seed the data dir with the
                      frozen v0.1.0 fixture -> launch old -> alive ->
                      digest the archive -> install the NEW .msi over it
                      (same product version falls back to
                      uninstall+install, logged as a REINSTALL) ->
                      launch new -> alive (this is where the schema
                      migration runs) -> digest again -> content counters
                      identical -> uninstall -> archive still there
      4. nsis-upgrade same, through the NSIS installer (same-version
                      overwrite is a true in-place upgrade there)

    "Archive intact" is asserted by comparing smoke_inspect digests of
    %APPDATA%\<identifier>\vault.db taken before and after the upgrade:
    every content counter must be identical, only the schema version may
    advance. Uninstalls must never delete the archive.

    Driven by .github/workflows/windows-smoke.yml; manual docs in
    tests/windows-smoke/README.md and docs/release.md.

.PARAMETER NewMsi
    Path to the NEW WiX installer (e.g. mailchat-vault_0.1.1_x64_en-US.msi).

.PARAMETER NewExe
    Path to the NEW NSIS setup (e.g. mailchat-vault_0.1.1_x64-setup.exe).

.PARAMETER OldMsi, OldExe
    Paths to the OLD installers (e.g. the published v0.1.0 bundles) for
    the upgrade phases. Optional; when omitted the upgrade phases are
    skipped with a notice.

.PARAMETER FixtureDb
    The frozen v0.1.0 archive fixture (tests/windows-smoke/fixtures/
    vault-v0.1.0.db) seeded into the old install before upgrading.

.PARAMETER InspectExe
    Path to the smoke_inspect example binary (built by the workflow with
    `cargo build --release -p vault-core --example smoke_inspect`).

.PARAMETER ProductName / AppId
    Tauri productName / identifier. Defaults match tauri.conf.json.

.PARAMETER AliveSeconds
    How long a launched app must stay alive to prove it did not crash.
    Default 20.

.PARAMETER LogDir
    Where install logs, app stdout/stderr and digests are written.
    Defaults to %RUNNER_TEMP%\windows-smoke (or %TEMP%).

.NOTES
    Exit code 0 = every phase passed; 1 = at least one failed (per-phase
    results are printed and written to summary.txt). Requires no
    interactive session beyond what a standard GitHub Windows runner
    provides (WebView2 runtime is preinstalled there).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $NewMsi,
    [Parameter(Mandatory = $true)] [string] $NewExe,
    [string] $OldMsi = '',
    [string] $OldExe = '',
    [string] $OldLabel = 'old',
    [string] $FixtureDb = '',
    [string] $InspectExe = '',
    [string] $ProductName = 'mailchat-vault',
    [string] $AppId = 'com.acailic.mailchatvault',
    [int] $AliveSeconds = 20,
    [string] $LogDir = ''
)

$ErrorActionPreference = 'Stop'
$script:Results = [ordered]@{}
if (-not $env:APPDATA) {
    # Windows always sets this; anything else means we are not on a
    # Windows-style profile and the whole smoke contract is void.
    throw 'APPDATA is not set — this smoke test must run on Windows.'
}
$script:DataDir = Join-Path $env:APPDATA $AppId
$script:DbPath = Join-Path $script:DataDir 'vault.db'
if (-not $LogDir) {
    $base = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
    $LogDir = Join-Path $base 'windows-smoke'
}
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

function Write-Log {
    param([string] $Message)
    Write-Host ("[smoke {0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $Message)
}

function Fail {
    param([string] $Message)
    throw $Message
}

function Assert-True {
    param([bool] $Condition, [string] $Message)
    if (-not $Condition) { Fail $Message }
}

# --- install / uninstall -------------------------------------------------

function Invoke-Msi {
    # msiexec <verb> <msi> — 0 and 3010 (reboot required) are success.
    param([string] $Verb, [string] $Msi, [string] $LogFile)
    $logPath = Join-Path $LogDir $LogFile
    Write-Log ("msiexec {0} {1} (log: {2})" -f $Verb, (Split-Path $Msi -Leaf), $LogFile)
    $p = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" `
        -ArgumentList @($Verb, $Msi, '/qn', '/norestart', '/l*v', $logPath) `
        -Wait -PassThru
    return [int] $p.ExitCode
}

function Install-Msi {
    param([string] $Msi, [string] $Tag)
    $code = Invoke-Msi '/i' $Msi ("install-{0}.log" -f $Tag)
    Assert-True ($code -eq 0 -or $code -eq 3010) `
        ("msi install of {0} failed: msiexec exit {1}" -f (Split-Path $Msi -Leaf), $code)
}

function Uninstall-Msi {
    param([string] $Msi, [string] $Tag)
    $code = Invoke-Msi '/x' $Msi ("uninstall-{0}.log" -f $Tag)
    Assert-True ($code -eq 0 -or $code -eq 3010) `
        ("msi uninstall failed: msiexec exit {0}" -f $code)
}

function Install-Nsis {
    param([string] $SetupExe, [string] $Tag)
    Write-Log ("NSIS install {0} /S" -f (Split-Path $SetupExe -Leaf))
    $p = Start-Process -FilePath $SetupExe -ArgumentList '/S' -Wait -PassThru
    Assert-True ($p.ExitCode -eq 0) `
        ("NSIS install failed: exit {0}" -f $p.ExitCode)
    # The installer finishes asynchronously after process exit in some
    # modes; wait for the app exe to show up.
    Wait-For { (Get-AppExe) -ne $null } 120 "NSIS install never produced the app exe"
}

function Uninstall-Nsis {
    param([string] $InstallDir, [string] $Tag)
    $uninstaller = Join-Path $InstallDir 'uninstall.exe'
    Assert-True (Test-Path $uninstaller) "no uninstall.exe under $InstallDir"
    Write-Log ("NSIS uninstall {0} /S" -f $uninstaller)
    # The NSIS uninstaller copies itself to temp and detaches; -Wait alone
    # is not enough, so poll for the install dir to disappear.
    Start-Process -FilePath $uninstaller -ArgumentList '/S' | Out-Null
    Wait-For { -not (Test-Path $InstallDir) } 120 "install dir $InstallDir never went away after uninstall"
}

# --- where the app landed -------------------------------------------------

function Get-AppExe {
    # The installers use Tauri defaults; cover every known root instead
    # of hardcoding one.
    $roots = @(
        (Join-Path $env:ProgramFiles $ProductName),
        (Join-Path ${env:ProgramFiles(x86)} $ProductName),
        (Join-Path $env:LOCALAPPDATA $ProductName),
        (Join-Path $env:LOCALAPPDATA (Join-Path 'Programs' $ProductName))
    )
    foreach ($root in $roots) {
        $exe = Join-Path $root "$ProductName.exe"
        if (Test-Path $exe) { return $exe }
    }
    return $null
}

function Wait-For {
    param([scriptblock] $Condition, [int] $TimeoutSeconds, [string] $What)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Condition) { return }
        Start-Sleep -Seconds 2
    }
    Fail ("timed out after {0}s: {1}" -f $TimeoutSeconds, $What)
}

# --- launch / archive -----------------------------------------------------

function Stop-AppProcesses {
    Get-Process -Name $ProductName -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
}

function Test-AppLaunch {
    # Launch the installed app and prove it did not crash: still alive
    # after $AliveSeconds. Fresh installs additionally must have created
    # vault.db on first run.
    param([string] $Exe, [string] $Tag, [bool] $ExpectFreshDb)
    $outLog = Join-Path $LogDir ("app-{0}-out.log" -f $Tag)
    $errLog = Join-Path $LogDir ("app-{0}-err.log" -f $Tag)
    Write-Log ("launching {0} (must stay alive {1}s)" -f $Exe, $AliveSeconds)
    $proc = Start-Process -FilePath $Exe -PassThru `
        -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    Start-Sleep -Seconds $AliveSeconds
    $proc.Refresh()
    if ($proc.HasExited) {
        $stderr = ''
        if (Test-Path $errLog) { $stderr = (Get-Content $errLog -Raw) }
        Fail ("app exited after {0}s with code {1}; stderr: {2}" -f `
            $AliveSeconds, $proc.ExitCode, ($stderr | Out-String))
    }
    Write-Log ("alive after {0}s -> OK" -f $AliveSeconds)
    if ($ExpectFreshDb) {
        Assert-True (Test-Path $script:DbPath) `
            ("first launch did not create {0}" -f $script:DbPath)
        Write-Log ("first launch created {0}" -f $script:DbPath)
    }
    Stop-Process -Id $proc.Id -Force
    Wait-For { $proc.Refresh(); $proc.HasExited } 30 "app process would not die"
}

function Get-Digest {
    param([string] $Tag)
    Assert-True (Test-Path $InspectExe) "inspect tool not found: $InspectExe"
    Assert-True (Test-Path $script:DbPath) ("no vault.db to digest at {0}" -f $script:DbPath)
    $json = (& $InspectExe $script:DbPath 2>&1) | Out-String -Width 4096
    if ($LASTEXITCODE -ne 0) {
        Fail ("smoke_inspect failed with exit {0}: {1}" -f $LASTEXITCODE, $json.Trim())
    }
    $digest = $json | ConvertFrom-Json
    $digest | ConvertTo-Json -Compress | Set-Content (Join-Path $LogDir ("digest-{0}.json" -f $Tag))
    Write-Log ("digest {0}: {1}" -f $Tag, ($digest | ConvertTo-Json -Compress))
    return $digest
}

$ContentKeys = @(
    'messages', 'identities', 'occurrences', 'conversations',
    'source_instances', 'body_bytes', 'fts_docs', 'fts_vault_hits'
)

function Compare-Digest {
    param($Before, $After, [string] $Tag)
    foreach ($key in $ContentKeys) {
        Assert-True ($Before.$key -eq $After.$key) `
            ("{0}: archive counter '{1}' changed across the upgrade: {2} -> {3}" -f `
                $Tag, $key, $Before.$key, $After.$key)
    }
    Assert-True ($After.schema_version -ge $Before.schema_version) `
        ("{0}: schema version went backwards: {1} -> {2}" -f `
            $Tag, $Before.schema_version, $After.schema_version)
    Assert-True ($After.fts_vault_hits -gt 0) `
        ("{0}: FTS found no 'vault' hits after the upgrade — archive unsearchable" -f $Tag)
    Write-Log ("{0}: archive intact (schema {1} -> {2})" -f `
        $Tag, $Before.schema_version, $After.schema_version)
}

function Reset-DataDir {
    Stop-AppProcesses
    if (Test-Path $script:DataDir) {
        Remove-Item -Recurse -Force $script:DataDir
    }
}

function Seed-Fixture {
    Assert-True (Test-Path $FixtureDb) "fixture not found: $FixtureDb"
    New-Item -ItemType Directory -Force -Path $script:DataDir | Out-Null
    Copy-Item $FixtureDb $script:DbPath -Force
    Write-Log ("seeded {0} from {1}" -f $script:DbPath, (Split-Path $FixtureDb -Leaf))
}

# --- phases ----------------------------------------------------------------

function Run-Phase {
    param([string] $Name, [scriptblock] $Body)
    Write-Log ("===== phase {0} =====" -f $Name)
    Reset-DataDir
    try {
        & $Body
        $script:Results[$Name] = 'PASS'
        Write-Log ("===== phase {0}: PASS =====" -f $Name)
    } catch {
        $script:Results[$Name] = "FAIL: $($_.Exception.Message)"
        Write-Host ("!!!!! phase {0} FAILED: {1}" -f $Name, $_.Exception.Message) `
            -ForegroundColor Red
        Reset-DataDir
        # Best-effort: run the real uninstallers first so product
        # registration never outlives its files, then sweep the known
        # install roots (the same list Get-AppExe covers) for leftovers.
        $roots = @(
            (Join-Path $env:ProgramFiles $ProductName),
            (Join-Path ${env:ProgramFiles(x86)} $ProductName),
            (Join-Path $env:LOCALAPPDATA $ProductName),
            (Join-Path $env:LOCALAPPDATA (Join-Path 'Programs' $ProductName))
        )
        try { Invoke-Msi '/x' $NewMsi ("cleanup-{0}.log" -f $Name) | Out-Null } catch { }
        foreach ($root in $roots) {
            if (Test-Path $root) {
                $un = Join-Path $root 'uninstall.exe'
                if (Test-Path $un) {
                    try {
                        Start-Process -FilePath $un -ArgumentList '/S' | Out-Null
                        Wait-For { -not (Test-Path $root) } 120 `
                            "cleanup uninstall of $root stalled"
                    } catch { }
                }
            }
        }
        foreach ($root in $roots) {
            if (Test-Path $root) {
                Write-Log ("cleanup: removing leftover install at {0}" -f $root)
                Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
            }
        }
    }
}

# --- validation ------------------------------------------------------------

foreach ($f in @($NewMsi, $NewExe)) {
    Assert-True (Test-Path $f) "required bundle not found: $f"
}
$upgradeReady = ($OldMsi -ne '' -and $OldExe -ne '' -and
    $FixtureDb -ne '' -and $InspectExe -ne '')
if (-not $upgradeReady) {
    Write-Log ("upgrade inputs incomplete (OldMsi/OldExe/FixtureDb/InspectExe) " +
        "— upgrade phases will be skipped")
}

# 1) MSI fresh install / launch / uninstall.
Run-Phase 'msi-fresh' {
    Install-Msi $NewMsi 'new'
    $exe = Get-AppExe
    Assert-True ($null -ne $exe) 'app exe not found after msi install'
    Write-Log ("installed at {0}" -f $exe)
    Test-AppLaunch $exe 'msi-fresh' $true
    Uninstall-Msi $NewMsi 'new'
    $exe = Get-AppExe
    Assert-True ($null -eq $exe) 'app exe still present after msi uninstall'
    Assert-True (Test-Path $script:DbPath) `
        'msi uninstall deleted the user archive (vault.db) — must never happen'
    Write-Log 'uninstalled; archive survived'
}

# 2) NSIS fresh install / launch / uninstall.
Run-Phase 'nsis-fresh' {
    Install-Nsis $NewExe 'new'
    $exe = Get-AppExe
    Assert-True ($null -ne $exe) 'app exe not found after NSIS install'
    $dir = Split-Path $exe -Parent
    Write-Log ("installed at {0}" -f $exe)
    Test-AppLaunch $exe 'nsis-fresh' $true
    Uninstall-Nsis $dir 'new'
    Assert-True ($null -eq (Get-AppExe)) 'app exe still present after NSIS uninstall'
    Assert-True (Test-Path $script:DbPath) `
        'NSIS uninstall deleted the user archive (vault.db) — must never happen'
    Write-Log 'uninstalled; archive survived'
}

# 3) MSI upgrade: old install + seeded fixture -> new install over it.
if ($upgradeReady) {
    Assert-True (Test-Path $OldMsi) "OldMsi not found: $OldMsi"
    Run-Phase 'msi-upgrade' {
        Install-Msi $OldMsi 'old'
        $exe = Get-AppExe
        Assert-True ($null -ne $exe) 'old app exe not found after install'
        Seed-Fixture
        Test-AppLaunch $exe 'msi-old' $false
        $before = Get-Digest 'msi-before'
        # A true upgrade installs over the old product. Same product
        # version gets 1638 ("another version already installed") — fall
        # back to uninstall+install, logged as a REINSTALL.
        $code = Invoke-Msi '/i' $NewMsi 'upgrade-new.log'
        if ($code -eq 1638) {
            Write-Host 'same MSI product version installed — REINSTALL path (uninstall, then install)'
            Uninstall-Msi $OldMsi 'old'
            Install-Msi $NewMsi 'new'
        } else {
            Assert-True ($code -eq 0 -or $code -eq 3010) `
                ("upgrade msi install failed: msiexec exit {0}" -f $code)
        }
        Assert-True (Test-Path $script:DbPath) 'vault.db vanished across the upgrade'
        $exe = Get-AppExe
        Assert-True ($null -ne $exe) 'new app exe not found after upgrade'
        Test-AppLaunch $exe 'msi-new' $false   # schema migration runs on open
        $after = Get-Digest 'msi-after'
        Compare-Digest $before $after 'msi-upgrade'
        Uninstall-Msi $NewMsi 'new'
        Assert-True ($null -eq (Get-AppExe)) 'app exe still present after msi uninstall'
        Assert-True (Test-Path $script:DbPath) 'msi uninstall deleted the upgraded archive'
        Write-Log 'uninstalled; upgraded archive survived'
    }
}

# 4) NSIS upgrade: same contract through the NSIS installer (same-version
#    overwrite is a true in-place upgrade there).
if ($upgradeReady) {
    Assert-True (Test-Path $OldExe) "OldExe not found: $OldExe"
    Run-Phase 'nsis-upgrade' {
        Install-Nsis $OldExe 'old'
        $exe = Get-AppExe
        Assert-True ($null -ne $exe) 'old app exe not found after NSIS install'
        Seed-Fixture
        Test-AppLaunch $exe 'nsis-old' $false
        $before = Get-Digest 'nsis-before'
        Install-Nsis $NewExe 'new'                # installs over the old build
        Assert-True (Test-Path $script:DbPath) 'vault.db vanished across the upgrade'
        $exe = Get-AppExe
        Assert-True ($null -ne $exe) 'new app exe not found after NSIS upgrade'
        Test-AppLaunch $exe 'nsis-new' $false
        $after = Get-Digest 'nsis-after'
        Compare-Digest $before $after 'nsis-upgrade'
        Uninstall-Nsis (Split-Path $exe -Parent) 'new'
        Assert-True ($null -eq (Get-AppExe)) 'app exe still present after NSIS uninstall'
        Assert-True (Test-Path $script:DbPath) 'NSIS uninstall deleted the upgraded archive'
        Write-Log 'uninstalled; upgraded archive survived'
    }
}

# --- summary ----------------------------------------------------------------

Reset-DataDir
Write-Host ''
Write-Host '===== Windows smoke summary ====='
$failed = 0
foreach ($entry in $script:Results.GetEnumerator()) {
    $line = ('{0,-14} {1}' -f $entry.Key, $entry.Value)
    if ($entry.Value -eq 'PASS') {
        Write-Host ("  {0}" -f $line) -ForegroundColor Green
    } else {
        Write-Host ("  {0}" -f $line) -ForegroundColor Red
        $failed++
    }
}
$summaryPath = Join-Path $LogDir 'summary.txt'
($script:Results.GetEnumerator() | ForEach-Object { '{0} {1}' -f $_.Key, $_.Value }) |
    Set-Content $summaryPath
Write-Log ("summary written to {0}" -f $summaryPath)
if ($failed -gt 0) {
    Write-Host ("{0} phase(s) FAILED" -f $failed) -ForegroundColor Red
    exit 1
}
Write-Host 'all phases passed' -ForegroundColor Green
exit 0
