function Get-GitConfigValue {
    param(
        [string[]]$Args
    )

    try {
        return (& git config @Args 2>$null).Trim()
    } catch {
        return ''
    }
}

function Read-GitScriptChoice {
    param(
        [string]$Prompt,
        [int]$Max,
        [int]$Default = 1
    )

    while ($true) {
        $inputValue = Read-Host "$Prompt（直接回车选择 $Default）"
        if ([string]::IsNullOrWhiteSpace($inputValue)) {
            return $Default
        }

        $number = 0
        if ([int]::TryParse($inputValue, [ref]$number) -and $number -ge 1 -and $number -le $Max) {
            return $number
        }

        Write-Host "输入无效，请输入 1-$Max。" -ForegroundColor Red
    }
}

function Get-GitHubCliLogin {
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if (-not $gh) {
        return ''
    }

    $login = (& gh api user -q .login 2>$null)
    if ($login) {
        return $login.Trim()
    }

    return ''
}

function Get-GitHubRepositoryParts {
    param(
        [string]$Repository
    )

    if (-not $Repository -or $Repository -notmatch '^([^/]+)/([^/]+)$') {
        return @{
            Owner = ''
            Name  = ''
        }
    }

    return @{
        Owner = $Matches[1]
        Name  = $Matches[2]
    }
}

function Read-GitHubRepositoryContext {
    param(
        [string]$DefaultRepository = '',
        [string]$DefaultType = '',
        [string]$DefaultRepositoryName = ''
    )

    $parts = Get-GitHubRepositoryParts -Repository $DefaultRepository
    $defaultTypeName = if ($DefaultType -eq 'organization') { 'organization' } else { 'personal' }
    $defaultChoice = if ($defaultTypeName -eq 'organization') { 2 } else { 1 }

    Write-Host ""
    Write-Host "请选择本次 GitHub 仓库归属类型：" -ForegroundColor Cyan
    Write-Host "  1. 个人仓库" -ForegroundColor DarkGray
    Write-Host "  2. 组织仓库" -ForegroundColor DarkGray
    $scopeChoice = Read-GitScriptChoice -Prompt "请输入 1 或 2" -Max 2 -Default $defaultChoice
    $isOrganization = ($scopeChoice -eq 2)

    if ($isOrganization) {
        $suggestedOwner = if ($defaultTypeName -eq 'organization') { $parts.Owner } else { '' }
        $ownerPrompt = if ($suggestedOwner) {
            "请输入 GitHub 组织名称（直接回车使用 $suggestedOwner）"
        } else {
            "请输入 GitHub 组织名称"
        }
    } else {
        $suggestedOwner = if ($defaultTypeName -eq 'personal') { $parts.Owner } else { '' }
        if (-not $suggestedOwner) {
            $suggestedOwner = Get-GitHubCliLogin
        }

        $ownerPrompt = if ($suggestedOwner) {
            "请输入 GitHub 用户名（直接回车使用 $suggestedOwner）"
        } else {
            "请输入 GitHub 用户名"
        }
    }

    $ownerInput = Read-Host $ownerPrompt
    $owner = if ([string]::IsNullOrWhiteSpace($ownerInput)) { $suggestedOwner } else { $ownerInput.Trim() }
    if (-not $owner -or $owner -match '[\/\s]') {
        throw "GitHub 用户名或组织名称不能为空，且不能包含斜杠或空格。"
    }

    $suggestedRepositoryName = if ($DefaultRepositoryName) {
        $DefaultRepositoryName.Trim()
    } else {
        $parts.Name
    }
    $repoPrompt = if ($suggestedRepositoryName) {
        "请输入仓库名称（直接回车使用 $suggestedRepositoryName）"
    } else {
        "请输入仓库名称"
    }

    $repoInput = Read-Host $repoPrompt
    $repositoryName = if ([string]::IsNullOrWhiteSpace($repoInput)) {
        $suggestedRepositoryName
    } else {
        $repoInput.Trim()
    }

    if (-not $repositoryName -or $repositoryName -match '[\/\s]') {
        throw "仓库名称不能为空，且不能包含斜杠或空格。"
    }

    return @{
        Repository     = "$owner/$repositoryName"
        RepositoryType = if ($isOrganization) { 'organization' } else { 'personal' }
        Owner          = $owner
        Organization   = if ($isOrganization) { $owner } else { '' }
        Name           = $repositoryName
    }
}

function Read-GitProtocolInteractive {
    param(
        [string]$DefaultProtocol = 'ssh'
    )

    $protocolInput = Read-Host "请选择协议 ssh/https（直接回车默认 $DefaultProtocol）"
    if ([string]::IsNullOrWhiteSpace($protocolInput)) {
        return $DefaultProtocol
    }

    $protocol = $protocolInput.Trim().ToLowerInvariant()
    if ($protocol -notin @('ssh', 'https')) {
        throw "协议必须是 ssh 或 https。"
    }

    return $protocol
}

function Read-GitSshHostInteractive {
    param(
        [string]$DefaultAlias = 'github-sunner'
    )

    $useAlias = Read-Host "是否使用 SSH Host 别名？(y/N)"
    if ($useAlias -notmatch '^(y|yes)$') {
        return 'github.com'
    }

    $aliasInput = Read-Host "请输入 SSH Host 别名（直接回车默认 $DefaultAlias）"
    if ([string]::IsNullOrWhiteSpace($aliasInput)) {
        return $DefaultAlias
    }

    return $aliasInput.Trim()
}

function New-GitHubRemoteUrl {
    param(
        [string]$Repository,
        [string]$Protocol = 'ssh',
        [string]$SshHost = 'github.com'
    )

    if ($Protocol -eq 'https') {
        return "https://github.com/$Repository.git"
    }

    return "git@${SshHost}:$Repository.git"
}

function Ensure-GitHubOriginInteractive {
    param(
        [string]$RemoteName = 'origin'
    )

    $currentUrl = (& git remote get-url $RemoteName 2>$null)
    if ($currentUrl) {
        return $currentUrl.Trim()
    }

    $profileDefaults = Get-GitScriptProfile
    $context = Read-GitHubRepositoryContext `
        -DefaultRepository $profileDefaults.Repository `
        -DefaultType $profileDefaults.RepositoryType
    $protocol = Read-GitProtocolInteractive -DefaultProtocol $profileDefaults.Protocol
    $sshHost = if ($protocol -eq 'ssh') {
        Read-GitSshHostInteractive -DefaultAlias $profileDefaults.SshHost
    } else {
        'github.com'
    }
    $remoteUrl = New-GitHubRemoteUrl -Repository $context.Repository -Protocol $protocol -SshHost $sshHost

    & git remote add $RemoteName $remoteUrl
    if ($LASTEXITCODE -ne 0) {
        throw "配置 $RemoteName 远程失败。"
    }

    Save-GitScriptProfile `
        -Repository $context.Repository `
        -RepositoryType $context.RepositoryType `
        -Owner $context.Owner `
        -Organization $context.Organization `
        -RemoteUrl $remoteUrl `
        -Protocol $protocol `
        -SshHost $sshHost `
        -RemoteName $RemoteName

    Write-Host "已新增远程 $RemoteName：$remoteUrl" -ForegroundColor Green
    return $remoteUrl
}

function Save-GitScriptProfile {
    param(
        [string]$Repository,
        [string]$RepositoryType,
        [string]$Owner,
        [string]$Organization,
        [string]$RemoteUrl,
        [string]$Protocol,
        [string]$SshHost,
        [string]$RemoteName
    )

    if ($Repository) {
        & git config --global lstwinhr.defaultRepository $Repository | Out-Null
    }
    if ($RepositoryType) {
        & git config --global lstwinhr.defaultRepositoryType $RepositoryType | Out-Null
        if ($RepositoryType -eq 'personal') {
            & git config --global --unset lstwinhr.defaultOrganization 2>$null | Out-Null
        }
    }
    if ($Owner) {
        & git config --global lstwinhr.defaultOwner $Owner | Out-Null
    }
    if ($Organization) {
        & git config --global lstwinhr.defaultOrganization $Organization | Out-Null
    }
    if ($RemoteUrl) {
        & git config --global lstwinhr.defaultRemoteUrl $RemoteUrl | Out-Null
    }
    if ($Protocol) {
        & git config --global lstwinhr.defaultProtocol $Protocol | Out-Null
    }
    if ($SshHost) {
        & git config --global lstwinhr.defaultSshHost $SshHost | Out-Null
    }
    if ($RemoteName) {
        & git config --global lstwinhr.defaultRemoteName $RemoteName | Out-Null
    }
}

function Get-GitScriptProfile {
    $repository = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRepository')
    $repositoryType = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRepositoryType')
    $owner = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultOwner')
    $organization = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultOrganization')
    $remoteUrl = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRemoteUrl')
    $protocol = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultProtocol')
    $sshHost = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultSshHost')
    $remoteName = Get-GitConfigValue -Args @('--global', '--get', 'lstwinhr.defaultRemoteName')
    $parts = Get-GitHubRepositoryParts -Repository $repository

    if (-not $owner) {
        $owner = $parts.Owner
    }
    if (-not $repositoryType) {
        $repositoryType = 'personal'
    }

    return @{
        Repository     = $repository
        RepositoryType = $repositoryType
        Owner          = $owner
        Organization   = $organization
        RemoteUrl      = $remoteUrl
        Protocol       = $(if ($protocol) { $protocol } else { 'ssh' })
        SshHost        = $(if ($sshHost) { $sshHost } else { 'github.com' })
        RemoteName     = $(if ($remoteName) { $remoteName } else { 'origin' })
    }
}
