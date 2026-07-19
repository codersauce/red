param(
    [string]$Installer = "install/install.ps1"
)

$ErrorActionPreference = "Stop"
$contents = Get-Content -Raw $Installer
$errors = $null
$tokens = $null
[Management.Automation.Language.Parser]::ParseInput(
    $contents,
    [ref]$tokens,
    [ref]$errors
) | Out-Null
if ($errors.Count -ne 0) {
    throw "PowerShell parser errors: $($errors -join [Environment]::NewLine)"
}

$analysis = Invoke-ScriptAnalyzer -Path $Installer -Severity Warning,Error
if ($analysis) {
    $analysis | Format-Table -AutoSize | Out-String | Write-Error
}
