# Superseded: run `oxid-relay.exe install` from an elevated PowerShell in the
# unpacked release archive instead. It copies the binary and plugins, creates
# C:\ProgramData\OxidRelay\config.toml, and registers and starts the
# "oxid-relay" service (with restart-on-failure). Remove it again with
# `oxid-relay.exe uninstall [--purge]`.
#
# Secrets (CLIENT_SECRET_AZURE, TEAMS_WEBHOOK_URL, ...) are read from
# machine-level environment variables:
#   [Environment]::SetEnvironmentVariable('CLIENT_SECRET_AZURE', '<secret>', 'Machine')
#   Restart-Service oxid-relay

Write-Host 'This script is superseded. Run: .\oxid-relay.exe install (elevated PowerShell)'
exit 1
