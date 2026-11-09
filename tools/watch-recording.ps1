param([int]$RecorderControllerPid,[string]$ResourceToken)
Wait-Process -Id $RecorderControllerPid -ErrorAction SilentlyContinue
& 'C:/Program Files/Git/bin/bash.exe' (Join-Path $env:USERPROFILE '.claude/hooks/resource-monitor.sh') deregister $ResourceToken | Out-Null

