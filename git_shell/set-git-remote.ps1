param(
    [string]$Repository,

    [string]$RemoteName = 'origin',

    [ValidateSet('ssh', 'https')]
    [string]$Protocol = 'ssh',

    [string]$SshHost = 'github.com'
)

$ErrorActionPreference = 'Stop'

$ProjectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ProjectRoot

# 加载仓库上下文配置
. (Join-Path $ProjectRoot 'git-script-profile.ps1')

# 加载共享模块
$helperModule = Join-Path $ProjectRoot 'git-remote-helper.ps1'
. $helperModule

$profileDefaults = Get-GitScriptProfile
$defaultRepository = if ($Repository) { $Repository } else { $profileDefaults.Repository }
$repositoryContext = Read-GitHubRepositoryContext `
    -DefaultRepository $defaultRepository `
    -DefaultType $profileDefaults.RepositoryType
$Repository = $repositoryContext.Repository

# 获取 SSH Host（仅在未显式传入时询问用户）
$SshHost = if ($Protocol -eq 'ssh') {
    Get-SshHostFromParams -SshHost $SshHost -PSBoundParameters $PSBoundParameters
} else {
    'github.com'
}

# 构造远程 URL
$remoteUrl = New-GitRemoteUrl -Repository $Repository -Protocol $Protocol -SshHost $SshHost

# 配置远程
Ensure-GitRemote -RemoteName $RemoteName -RemoteUrl $remoteUrl

Save-GitScriptProfile `
    -Repository $repositoryContext.Repository `
    -RepositoryType $repositoryContext.RepositoryType `
    -Owner $repositoryContext.Owner `
    -Organization $repositoryContext.Organization `
    -RemoteUrl $remoteUrl `
    -Protocol $Protocol `
    -SshHost $SshHost `
    -RemoteName $RemoteName

# 显示配置结果
Show-GitRemoteConfig
