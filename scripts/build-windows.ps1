$ErrorActionPreference = 'Stop'
Push-Location "$PSScriptRoot/.."
try { docker compose run --rm build-windows; exit $LASTEXITCODE }
finally { Pop-Location }
