# glassterm's built-in prompt, robbyrussell style:  ➜  folder git:(branch)
# Runs after the user's own $PROFILE. Steps aside for oh-my-posh / starship.
if (-not ($env:POSH_SESSION_ID -or $env:STARSHIP_SHELL)) {
    function global:prompt {
        $ok = $?
        $E = [char]27
        $p = $ExecutionContext.SessionState.Path.CurrentLocation.ProviderPath
        $homeDir = [Environment]::GetFolderPath('UserProfile')

        # Branch straight from .git/HEAD: no git.exe per prompt, so it stays instant.
        $branch = $null
        $d = $p
        while ($d) {
            $head = [IO.Path]::Combine($d, '.git', 'HEAD')
            if ([IO.File]::Exists($head)) {
                $ref = [IO.File]::ReadAllText($head).Trim()
                if ($ref.StartsWith('ref: refs/heads/')) { $branch = $ref.Substring(16) }
                else { $branch = $ref.Substring(0, [Math]::Min(7, $ref.Length)) }
                break
            }
            $d = [IO.Path]::GetDirectoryName($d)
        }

        $leaf = Split-Path -Leaf $p
        if ($p -eq $homeDir) { $leaf = '~' } elseif (-not $leaf) { $leaf = $p }
        $title = if ($p.StartsWith($homeDir)) { '~' + $p.Substring($homeDir.Length) } else { $p }

        $arrow = if ($ok) { "${E}[1;32m" } else { "${E}[1;31m" }
        $s = "${E}]0;$title$([char]7)$arrow$([char]0x279C)  ${E}[1;36m$leaf${E}[0m"
        if ($branch) { $s += " ${E}[1;34mgit:(${E}[31m$branch${E}[1;34m)${E}[0m" }
        "$s "
    }
}

# zsh-ish line editing, where this PSReadLine is new enough to have it.
if (Get-Module PSReadLine) {
    Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete
    Set-PSReadLineKeyHandler -Key UpArrow -Function HistorySearchBackward
    Set-PSReadLineKeyHandler -Key DownArrow -Function HistorySearchForward
    try { Set-PSReadLineOption -PredictionSource History -ErrorAction Stop } catch { }
}
