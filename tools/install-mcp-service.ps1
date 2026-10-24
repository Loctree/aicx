# Per-user, interactive-logon task: aicx is a console app, not an SCM service.
# Never stop or delete an existing Windows service, including legacy aicx-mcp.
$ErrorActionPreference = "Stop"
$port = if ($env:AICX_MCP_PORT) { $env:AICX_MCP_PORT } else { "8044" }
if ($port -notmatch '^[0-9]{1,5}$' -or [int]$port -lt 1 -or [int]$port -gt 65535) {
  throw "mcp service: AICX_MCP_PORT must be between 1 and 65535"
}

$bin = $env:AICX_BIN
if (-not $bin) {
  $candidate = Join-Path $env:USERPROFILE ".local\bin\aicx.exe"
  if (Test-Path -LiteralPath $candidate -PathType Leaf) { $bin = $candidate }
  else {
    $command = Get-Command aicx.exe, aicx -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { $bin = $command.Source }
  }
}
# npm's global .cmd/.ps1 shim cannot be used as a scheduled executable.
# Resolve only the shipped platform payload, never invoke a command shell.
if ($bin -and [IO.Path]::GetExtension($bin) -in @(".cmd", ".ps1")) {
  $prefix = Split-Path -Parent $bin
  $wrapper = @(
    (Join-Path $prefix "node_modules\@loctree\aicx\index.js"),
    (Join-Path $prefix "..\@loctree\aicx\index.js")
  ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
  $node = Get-Command node.exe, node -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1
  if (-not $wrapper -or -not $node) {
    throw "mcp service: npm shim requires its installed wrapper and Node runtime"
  }
  # Reuse the wrapper's Node resolver: optional payloads may be nested beneath
  # the wrapper or hoisted beside it. Do not guess npm's dependency layout.
  $resolveNative = @'
const path = require('path');
const wrapper = path.resolve(process.argv[1]);
const { resolvePlatformPackageRoot } = require(wrapper);
const root = resolvePlatformPackageRoot('@loctree/aicx-win32-x64-gnu', path.dirname(wrapper));
process.stdout.write(path.join(root, 'bin', 'aicx.exe'));
'@
  $resolved = & $node.Source -e $resolveNative $wrapper
  if ($LASTEXITCODE -ne 0 -or -not $resolved) {
    throw "mcp service: npm wrapper could not resolve its native Windows payload"
  }
  $bin = "$resolved"
}
if (-not $bin -or [IO.Path]::GetExtension($bin) -ne ".exe" -or
    -not (Test-Path -LiteralPath $bin -PathType Leaf)) {
  throw "mcp service: native aicx.exe not found; install the prebuilt Windows package"
}
$bin = (Resolve-Path -LiteralPath $bin).ProviderPath
if ([IO.Path]::GetFileName($bin) -eq "aicx-mcp.exe") {
  $bin = Join-Path (Split-Path -Parent $bin) "aicx.exe"
  if (-not (Test-Path -LiteralPath $bin -PathType Leaf)) {
    throw "mcp service: hybrid aicx.exe not found beside aicx-mcp.exe"
  }
}
# Validate before modifying even our own previous registration.
& $bin serve --help | Out-Null
if ($LASTEXITCODE -ne 0) { throw "mcp service: native aicx serve validation failed" }

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $identity.User.Value
$taskName = "AICX-MCP-$sid"
$ownership = "AICX per-user dashboard and MCP ($sid)"
$powershell = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
$encodedPrefix = "-NoProfile -NonInteractive -EncodedCommand "
$wrapperHeader = "# AICX-MCP per-user task v1 ($sid)"
$arguments = "serve --transport http --host 127.0.0.1 --port $port --no-require-auth --no-auto-refresh"
$description = "$ownership; $arguments"
$homeSetup = if ($env:AICX_HOME) {
  $aicxHome = [IO.Path]::GetFullPath($env:AICX_HOME)
  "`$env:AICX_HOME = '$($aicxHome.Replace("'", "''"))'"
} else {
  # Keep canonical [storage].home/default resolution in the native binary.
  "Remove-Item Env:AICX_HOME -ErrorAction SilentlyContinue"
}
# No persistent user/system environment changes. EncodedCommand keeps paths and
# custom homes (including quotes, dollars and spaces) out of command parsing.
$instance = [guid]::NewGuid().ToString()
$wrapper = @"
$wrapperHeader
# Instance: $instance
$homeSetup
`$env:HOME = '$($env:USERPROFILE.Replace("'", "''"))'
& '$($bin.Replace("'", "''"))' $arguments
exit `$LASTEXITCODE
"@
$taskArguments = $encodedPrefix + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($wrapper))

function Test-OwnedTask($task) {
  if (-not $task -or -not $task.Description.StartsWith("$ownership; ") -or
      $task.Principal.UserId -notin @($sid, $identity.Name) -or
      @($task.Actions).Count -ne 1 -or $task.Actions[0].Execute -ne $powershell -or
      -not $task.Actions[0].Arguments.StartsWith($encodedPrefix)) { return $false }
  try {
    $decoded = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String(
      $task.Actions[0].Arguments.Substring($encodedPrefix.Length)))
    return $decoded.StartsWith("$wrapperHeader`n") -or $decoded.StartsWith("$wrapperHeader`r`n")
  } catch { return $false }
}

function Test-TaskListener($process) {
  if (-not $process -or $process.ExecutablePath -ne $bin -or
      -not $process.CommandLine -or -not $process.CommandLine.Contains($arguments)) { return $false }
  $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwnerSid
  if ($owner.Sid -ne $sid) { return $false }
  # Match the unique registered wrapper, not another user's or same-user
  # listener running the very same binary/arguments on the requested port.
  for ($depth = 0; $depth -lt 16; $depth++) {
    if ($process.ExecutablePath -eq $powershell -and $process.CommandLine -and
        $process.CommandLine.Contains($taskArguments)) {
      $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwnerSid
      return $owner.Sid -eq $sid
    }
    if ($process.ParentProcessId -le 1 -or $process.ParentProcessId -eq $process.ProcessId) { break }
    $process = Get-CimInstance Win32_Process -Filter "ProcessId = $($process.ParentProcessId)"
    if (-not $process) { break }
  }
  return $false
}

$existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($existing -and -not (Test-OwnedTask $existing)) {
  throw "mcp service: task name is owned by another registration; no changes made"
}
$backup = if ($existing) { Export-ScheduledTask -TaskName $taskName } else { $null }
$wasRunning = $existing -and $existing.State -eq "Running"
$action = New-ScheduledTaskAction -Execute $powershell -Argument $taskArguments
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $sid
$principal = New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) `
  -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
  -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
$registered = $false
try {
  Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Description $description -Force | Out-Null
  $registered = $true
  $current = Get-ScheduledTask -TaskName $taskName
  if (-not (Test-OwnedTask $current) -or $current.Actions[0].Arguments -ne $taskArguments) {
    throw "task registration changed unexpectedly; no task will be stopped"
  }
  if ($wasRunning) { Stop-ScheduledTask -TaskName $taskName }
  Start-ScheduledTask -TaskName $taskName
  $ready = $false
  for ($attempt = 0; $attempt -lt 40; $attempt++) {
    $task = Get-ScheduledTask -TaskName $taskName
    if ((Test-OwnedTask $task) -and $task.Actions[0].Arguments -eq $taskArguments -and
        $task.State -eq "Running") {
      $listeners = @(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort ([int]$port) `
        -State Listen -ErrorAction SilentlyContinue)
      foreach ($listener in $listeners) {
        $process = Get-CimInstance Win32_Process -Filter "ProcessId = $($listener.OwningProcess)"
        if (Test-TaskListener $process) {
          try {
            $response = Invoke-WebRequest -Uri "http://127.0.0.1:$port/health" `
              -UseBasicParsing -TimeoutSec 2
            if ($response.StatusCode -eq 200) { $ready = $true }
          } catch { }
        }
      }
    }
    if ($ready) { break }
    Start-Sleep -Milliseconds 250
  }
  if (-not $ready) { throw "scheduled task did not start an owned, healthy loopback listener" }
  # Keep onboarding's existing definition path, but record the actual task,
  # only after successful activation (never a speculative SCM registration).
  $recordDir = Join-Path $env:LOCALAPPDATA "aicx"
  New-Item -ItemType Directory -Force -Path $recordDir | Out-Null
  Export-ScheduledTask -TaskName $taskName |
    Set-Content -LiteralPath (Join-Path $recordDir "aicx-mcp-service.xml") -Encoding utf8
} catch {
  $failure = $_
  $current = if ($registered) { Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue } else { $null }
  if ($registered -and (Test-OwnedTask $current) -and $current.Actions[0].Arguments -eq $taskArguments) {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($backup) {
      Register-ScheduledTask -TaskName $taskName -Xml $backup -Force | Out-Null
      if ($wasRunning) { Start-ScheduledTask -TaskName $taskName }
    } else {
      Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
  }
  throw $failure
}
Write-Output "mcp service: per-user task $taskName (runs while this user is logged in)"
Write-Output "mcp service: dashboard http://127.0.0.1:$port/ and MCP http://127.0.0.1:$port/mcp"
Write-Output "mcp service: legacy Windows services were left unchanged"
