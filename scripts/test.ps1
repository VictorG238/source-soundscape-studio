$ErrorActionPreference = 'Stop'
Push-Location "$PSScriptRoot/.."
try { docker compose run --rm test; exit $LASTEXITCODE }
finally { Pop-Location }
