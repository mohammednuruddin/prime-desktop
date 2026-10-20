# Exercises attestation on this Windows PC: unattested before the suite passes, enforced after, and a record from
# another OS build is not accepted. It uses a throwaway state directory, so the real record is untouched.
#   powershell -File sandbox\scripts\check-attestation.ps1 -Target <cargo target\debug folder> -Policy <policy.json> -Request <request.json>
param([Parameter(Mandatory)][string]$Target, [Parameter(Mandatory)][string]$Policy, [Parameter(Mandatory)][string]$Request)
$helper = Join-Path $Target 'desktop-sandbox.exe'
$env:PRIME_DESKTOP_SANDBOX_STATE_DIR = Join-Path $env:TEMP 'desktop-attestation-check'
Remove-Item -Recurse -Force $env:PRIME_DESKTOP_SANDBOX_STATE_DIR -ErrorAction SilentlyContinue

function Enforcement { (& $helper run --policy $Policy --request $Request --backend mxc | ConvertFrom-Json).enforcement }
# Windows PowerShell 5 hands back a JSON array as one object, so unroll it before filtering.
function Attested { (& $helper capabilities | ConvertFrom-Json | ForEach-Object { $_ } | Where-Object { $_.backend -eq 'mxc' }).attested }
function Expect($what, $got, $want) { if ($got -ne $want) { throw "$what`: expected $want, got $got" } else { "ok  $what = $got" } }

Expect 'before attesting' (Enforcement) 'unattested'
& $helper attest | Out-Null
Expect 'attest exit code' $LASTEXITCODE 0
Expect 'after attesting' (Enforcement) 'enforced'
Expect 'capabilities say attested' (Attested) $true

$record = Join-Path $env:PRIME_DESKTOP_SANDBOX_STATE_DIR 'attestation-mxc.json'
(Get-Content $record -Raw) -replace '"host": "windows-[0-9.]+"', '"host": "windows-1.1"' | Set-Content $record
Expect 'a record from another OS build' (Enforcement) 'unattested'
Remove-Item -Recurse -Force $env:PRIME_DESKTOP_SANDBOX_STATE_DIR
