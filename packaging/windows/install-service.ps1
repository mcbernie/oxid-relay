# Installs OxidRelay as a Windows service. Run from an elevated PowerShell.
#
# Adjust the paths below before running. Secrets (CLIENT_SECRET_AZURE,
# TEAMS_WEBHOOK_URL, ...) are read from machine-level environment variables;
# set them with [Environment]::SetEnvironmentVariable(..., 'Machine') and
# restart the service so it picks them up.

$ServiceName = 'OxidRelay'
$ExePath     = 'C:\Program Files\OxidRelay\oxid-relay.exe'
$ConfigPath  = 'C:\ProgramData\OxidRelay\config.toml'

# Quoting matters: the whole command line is one binPath value.
$BinPath = '"{0}" --service --config "{1}"' -f $ExePath, $ConfigPath

sc.exe create $ServiceName binPath= $BinPath start= auto
sc.exe description $ServiceName "OxidRelay mail relay and notification gateway"

# Example: set a secret at machine scope (then start/restart the service).
# [Environment]::SetEnvironmentVariable('CLIENT_SECRET_AZURE', '<secret>', 'Machine')

sc.exe start $ServiceName

# Uninstall:
#   sc.exe stop $ServiceName
#   sc.exe delete $ServiceName
