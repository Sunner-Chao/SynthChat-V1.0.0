param(
    [switch]$ShowOnly
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectRoot

. (Join-Path $ProjectRoot 'git-script-profile.ps1')
$profileDefaults = Get-GitScriptProfile

Write-Host "Git 配置概览" -ForegroundColor Green
Write-Host ""

$currentBranch = (& git branch --show-current).Trim()
$originUrl = (& git remote get-url origin 2>$null).Trim()
$localName = (& git config --get user.name 2>$null).Trim()
$localEmail = (& git config --get user.email 2>$null).Trim()
$globalName = (& git config --global --get user.name 2>$null).Trim()
$globalEmail = (& git config --global --get user.email 2>$null).Trim()

Write-Host "当前分支: $currentBranch" -ForegroundColor Yellow
Write-Host "origin:   $originUrl" -ForegroundColor Yellow
if ($profileDefaults.Repository) {
    $scopeLabel = if ($profileDefaults.RepositoryType -eq 'organization') { '组织仓库' } else { '个人仓库' }
    Write-Host "仓库类型: $scopeLabel" -ForegroundColor Yellow
    Write-Host "仓库路径: $($profileDefaults.Repository)" -ForegroundColor Yellow
}
Write-Host ""
Write-Host "本地仓库账号:" -ForegroundColor Cyan
Write-Host "  user.name  = $localName"
Write-Host "  user.email = $localEmail"
Write-Host ""
Write-Host "全局 Git 账号:" -ForegroundColor Cyan
Write-Host "  user.name  = $globalName"
Write-Host "  user.email = $globalEmail"

if (-not $ShowOnly) {
    Write-Host ""
    Write-Host "常用命令示例:" -ForegroundColor DarkGray
    Write-Host '.\set-git-account.ps1 -UserName "Your Name" -Email "you@example.com"' -ForegroundColor DarkGray
    Write-Host '.\set-git-account.ps1 -UserName "Your Name" -Email "you@example.com" -Global' -ForegroundColor DarkGray
    Write-Host '.\set-git-remote.ps1' -ForegroundColor DarkGray
    Write-Host '运行后通过数字菜单选择个人仓库或组织仓库。' -ForegroundColor DarkGray
}
