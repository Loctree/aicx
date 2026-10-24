# Mock only Windows OS APIs; execute the production installer as a scriptblock.
param([Parameter(Mandatory=$true)][string]$Fixture)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Path $Fixture -Force | Out-Null
$native = Join-Path $Fixture "aicx.exe"
Set-Content $native "#!/bin/sh`nexit 0`n"
if (-not $IsWindows) { & chmod +x $native }
$source = Get-Content (Join-Path $PSScriptRoot "install-mcp-service.ps1") -Raw
$resolver = [regex]::Match($source, "(?s)\`$resolveNative = @'\r?\n(.*?)\r?\n'@").Groups[1].Value
if (-not $resolver -or $resolver.Contains('"')) {
  throw "Node -e resolver must avoid embedded double quotes for Windows PowerShell 5"
}
$source = $source.Replace('[Security.Principal.WindowsIdentity]::GetCurrent()',
  '([pscustomobject]@{User=[pscustomobject]@{Value="S-1-5-21-fixture"};Name="fixture"})')
$installer = [scriptblock]::Create($source)
$env:AICX_BIN = $native
$env:LOCALAPPDATA = $Fixture
$env:USERPROFILE = $Fixture
$env:SystemRoot = Join-Path $Fixture "Windows"
$env:AICX_HOME = Join-Path $Fixture "custom 'home' `$value space"
$env:AICX_MCP_PORT = "8044"
$script:events = @()
$script:existing = $null
$script:registered = $null
$script:failRegister = $false
$script:healthy = $true
$script:foreignWrapper = $false
$script:ownerSid = "S-1-5-21-fixture"
function Assert($condition, $message) { if (-not $condition) { throw $message } }
function Get-ScheduledTask {
  param($TaskName, $ErrorAction)
  if ($script:registered) { return $script:registered }
  return $script:existing
}
function Export-ScheduledTask {
  param($TaskName)
  if ($script:registered) { return "<Task><Description>$($script:registered.Description)</Description></Task>" }
  return "<previous/>"
}
function New-ScheduledTaskAction { param($Execute, $Argument) return @{Execute=$Execute;Arguments=$Argument} }
function New-ScheduledTaskTrigger { param([switch]$AtLogOn, $User) return @{User=$User} }
function New-ScheduledTaskPrincipal { param($UserId, $LogonType, $RunLevel) return @{UserId=$UserId;LogonType=$LogonType;RunLevel=$RunLevel} }
function New-ScheduledTaskSettingsSet {
  param($ExecutionTimeLimit, $RestartCount, $RestartInterval, $MultipleInstances,
        [switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries)
  return @{}
}
function Register-ScheduledTask {
  param($TaskName, $Action, $Trigger, $Principal, $Settings, $Description, [switch]$Force, $Xml)
  $script:events += "register"
  if ($script:failRegister) { throw "registration failed" }
  if ($Xml) {
    Assert ($Xml -eq "<previous/>") "rollback must use exported XML"
    $script:events += "restore"
    $script:registered = $script:existing
  } else {
    Assert ($Principal.LogonType -eq "Interactive") "must not run as LocalSystem"
    Assert ($Principal.RunLevel -eq "Limited") "must not require elevation"
    Assert ($Action.Execute.EndsWith("powershell.exe")) "must use native PowerShell action"
    $decoded = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($Action.Arguments.Split(" ")[-1]))
    Assert ($decoded.Contains("& '$($env:AICX_BIN.Replace("'", "''"))' serve ")) "wrapper must run native hybrid server"
    if ($env:AICX_HOME) {
      Assert ($decoded.Contains("`$env:AICX_HOME = '$($env:AICX_HOME.Replace("'", "''"))'")) "must persist custom AICX_HOME without interpolation"
    } else {
      Assert ($decoded.Contains("Remove-Item Env:AICX_HOME -ErrorAction SilentlyContinue")) "unset home must leave canonical storage.home resolution active"
    }
    $script:serveArgs = ($Description -split '; ', 2)[1]
    $script:action = $Action
    $script:registered = [pscustomobject]@{State="Ready";Description=$Description;Principal=$Principal;Actions=@($Action)}
  }
}
function Start-ScheduledTask { param($TaskName) $script:events += "start"; $script:registered.State = "Running" }
function Stop-ScheduledTask { param($TaskName, $ErrorAction) $script:events += "stop"; $script:registered.State = "Ready" }
function Unregister-ScheduledTask { param($TaskName, [switch]$Confirm) $script:events += "remove"; $script:registered = $null }
function Get-NetTCPConnection { param($LocalAddress, $LocalPort, $State, $ErrorAction) return @{OwningProcess=42} }
function Get-CimInstance {
  param($ClassName, $Filter)
  if ($Filter -eq "ProcessId = 43") {
    return @{
      ExecutablePath=$script:action.Execute
      CommandLine=$(if ($script:foreignWrapper) { "another task action" } else { $script:action.Arguments })
      ProcessId=43;ParentProcessId=1
    }
  }
  return @{ExecutablePath=$env:AICX_BIN;CommandLine=$script:serveArgs;ProcessId=42;ParentProcessId=43}
}
function Invoke-CimMethod { param($InputObject, $MethodName) return @{Sid=$script:ownerSid} }
function Invoke-WebRequest { param($Uri, [switch]$UseBasicParsing, $TimeoutSec) if (-not $script:healthy) { throw "unhealthy" }; return @{StatusCode=200} }
function Start-Sleep { param($Milliseconds) }
function sc.exe { throw "SCM must never be touched" }
function Expect-Failure {
  $failed = $false
  try { & $installer | Out-Null } catch { $failed = $true }
  Assert $failed "expected installation failure"
}
& $installer | Out-Null
Assert (($script:events -join ",") -eq "register,start") "first install must register and start"
Assert ((Get-Content (Join-Path $Fixture "aicx/aicx-mcp-service.xml") -Raw).Contains("--host 127.0.0.1")) "onboarding must see the successful task definition"
# Execute the persisted wrapper in a new process with AICX_HOME removed.
# The fake native server records the environment it actually receives.
$persisted = $script:action.Arguments.Split(" ")[-1]
New-Item -ItemType Directory -Path $env:AICX_HOME -Force | Out-Null
Set-Content $native '#!/bin/sh
if [ "$*" = "serve --help" ]; then exit 0; fi
printf "%s" "$AICX_HOME" > "$AICX_HOME/home-seen"
'
$customHome = $env:AICX_HOME
Remove-Item Env:AICX_HOME
& (Get-Process -Id $PID).Path -NoProfile -NonInteractive -EncodedCommand $persisted
$env:AICX_HOME = $customHome
Assert ((Get-Content (Join-Path $customHome "home-seen") -Raw) -eq $customHome) "task must restore custom AICX_HOME without installer environment"
Set-Content $native "#!/bin/sh`nexit 0`n"
# No explicit override: retain the installing user's bootstrap config and
# remove even a stale AICX_HOME supplied by the scheduler's ambient environment.
Remove-Item Env:AICX_HOME
& $installer | Out-Null
$canonicalWrapper = $script:action.Arguments.Split(" ")[-1]
$bootstrap = Join-Path $Fixture ".aicx"
New-Item -ItemType Directory -Path $bootstrap -Force | Out-Null
$configuredStorage = Join-Path $Fixture "configured-storage"
Set-Content (Join-Path $bootstrap "config.toml") "[storage]`nhome = '$configuredStorage'"
Set-Content $native '#!/bin/sh
if [ -n "${AICX_HOME:-}" ]; then exit 7; fi
printf "%s" "$HOME" > "$HOME/canonical-home-seen"
'
$env:AICX_HOME = Join-Path $Fixture "stale-scheduler-home"
& (Get-Process -Id $PID).Path -NoProfile -NonInteractive -EncodedCommand $canonicalWrapper
Assert ($LASTEXITCODE -eq 0) "wrapper must remove ambient AICX_HOME when installer had no override"
Assert ((Get-Content (Join-Path $Fixture "canonical-home-seen") -Raw) -eq $Fixture) "canonical resolver must see the installing user's home"
Assert ((Get-Content (Join-Path $bootstrap "config.toml") -Raw).Contains($configuredStorage)) "configured storage home must remain available"
$env:AICX_HOME = $customHome
Set-Content $native "#!/bin/sh`nexit 0`n"
$owned = $script:registered
$script:existing = $owned
$script:registered = $null
$script:events = @()
$script:failRegister = $true
Expect-Failure
Assert (($script:events -join ",") -eq "register") "registration failure must not stop previous task"
$script:failRegister = $false
$script:healthy = $false
$script:events = @()
Expect-Failure
Assert (($script:events -join ",") -eq "register,stop,start,stop,register,restore,start") "failed health must restore and restart previous task"
$script:existing = $null
$script:registered = $null
$script:events = @()
Expect-Failure
Assert (($script:events -join ",") -eq "register,start,stop,remove") "failed new install must remove only its new task"
$script:registered = $null
$script:healthy = $true
$script:foreignWrapper = $true
$script:events = @()
Expect-Failure
Assert (($script:events -join ",") -eq "register,start,stop,remove") "same binary/argv from another task must not satisfy readiness"
$script:registered = $null
$script:foreignWrapper = $false
$script:ownerSid = "S-1-5-18"
$script:events = @()
Expect-Failure
Assert (($script:events -join ",") -eq "register,start,stop,remove") "LocalSystem listener must not satisfy readiness"
$script:registered = $null
$script:ownerSid = "S-1-5-21-fixture"
$script:existing = [pscustomobject]@{Description="foreign";Principal=@{UserId="foreign"};State="Running"}
$script:events = @()
Expect-Failure
Assert ($script:events.Count -eq 0) "foreign task must not be modified"
$script:existing = [pscustomobject]@{
  Description=$owned.Description;Principal=$owned.Principal;State="Running"
  Actions=@(@{Execute="foreign.exe";Arguments="foreign"})
}
Expect-Failure
Assert ($script:events.Count -eq 0) "matching description alone must not authorize stopping a foreign action"
$script:existing = $owned
$script:events = @()
$env:AICX_BIN = Join-Path $Fixture "missing.exe"
Expect-Failure
Assert ($script:events.Count -eq 0) "invalid binary must preserve registration"
$env:AICX_BIN = $native
$script:events = @()
Set-Content $native "#!/bin/sh`nexit 1`n"
Expect-Failure
Assert ($script:events.Count -eq 0) "failed native validation must preserve registration"
Set-Content $native "#!/bin/sh`nexit 0`n"
# npm's actual Node resolution must handle hoisted and nested optional payloads,
# preferring the nested dependency if both versions exist.
$source = $source.Replace('$bin = (Resolve-Path -LiteralPath $bin).ProviderPath',
  '$bin = (Resolve-Path -LiteralPath $bin).ProviderPath; $env:AICX_BIN = $bin')
$installer = [scriptblock]::Create($source)
foreach ($layout in @("hoisted", "nested", "both")) {
  $prefix = Join-Path $Fixture "npm-$layout"
  $wrapperRoot = Join-Path $prefix "node_modules/@loctree/aicx"
  New-Item -ItemType Directory -Path $wrapperRoot -Force | Out-Null
  Copy-Item (Join-Path $PSScriptRoot "../distribution/npm/aicx/index.js") (Join-Path $wrapperRoot "index.js")
  $hoisted = Join-Path $prefix "node_modules/@loctree/aicx-win32-x64-gnu"
  $nested = Join-Path $wrapperRoot "node_modules/@loctree/aicx-win32-x64-gnu"
  $roots = if ($layout -eq "both") { @($hoisted, $nested) } elseif ($layout -eq "nested") { @($nested) } else { @($hoisted) }
  foreach ($root in $roots) {
    $payload = Join-Path $root "bin"
    New-Item -ItemType Directory -Path $payload -Force | Out-Null
    Copy-Item $native (Join-Path $payload "aicx.exe")
    Set-Content (Join-Path $root "package.json") '{"name":"@loctree/aicx-win32-x64-gnu","version":"0.15.0"}'
  }
  $expectedRoot = if ($layout -eq "hoisted") { $hoisted } else { $nested }
  $expected = (Resolve-Path (Join-Path $expectedRoot "bin/aicx.exe")).ProviderPath
  $env:AICX_BIN = Join-Path $prefix "aicx.cmd"
  $script:existing = $null
  $script:registered = $null
  $script:healthy = $true
  & $installer | Out-Null
  Assert ($env:AICX_BIN -eq $expected) "npm $layout layout resolved the wrong native payload"
}
Write-Output "Windows per-user scheduled task install/rollback/foreign ownership: passed"
