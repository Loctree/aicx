# Windows service for the AICX loopback listener.
# Same contract as launchd: 127.0.0.1, no bearer, experimental auto-refresh (300s).
$ErrorActionPreference = "Stop"
$port = if ($env:AICX_MCP_PORT) { $env:AICX_MCP_PORT } else { "8044" }
$bin = $env:AICX_BIN
if (-not $bin) {
  $candidate = Join-Path $env:USERPROFILE ".local\bin\aicx.exe"
  if (Test-Path $candidate) { $bin = $candidate }
  else { $bin = (Get-Command aicx -ErrorAction SilentlyContinue).Source }
}
if (-not $bin) {
  Write-Error "mcp service: aicx not found"
  exit 1
}
$args = "serve --transport http --host 127.0.0.1 --port $port --no-require-auth --experimental-auto-refresh"
$recordDir = Join-Path $env:LOCALAPPDATA "aicx"
New-Item -ItemType Directory -Force -Path $recordDir | Out-Null
$record = Join-Path $recordDir "aicx-mcp-service.xml"
@"
<service>
  <bin>$bin</bin>
  <args>$args</args>
</service>
"@ | Set-Content -Path $record -Encoding utf8
& sc.exe stop aicx-mcp | Out-Null
& sc.exe delete aicx-mcp | Out-Null
$binPath = "`"$bin`" $args"
& sc.exe create aicx-mcp binPath= $binPath start= auto DisplayName= "AICX dashboard and MCP"
if ($LASTEXITCODE -ne 0) {
  Write-Error "mcp service: sc.exe create failed ($LASTEXITCODE). The foreign listener was not stopped."
  exit $LASTEXITCODE
}
& sc.exe start aicx-mcp
if ($LASTEXITCODE -ne 0) {
  Write-Error "mcp service: sc.exe start failed ($LASTEXITCODE)"
  exit $LASTEXITCODE
}
Write-Output "mcp service: dashboard http://127.0.0.1:$port/ and MCP http://127.0.0.1:$port/mcp"
