param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$FixtureDirectory
)

$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$null = New-Item -ItemType Directory -Path $FixtureDirectory -Force
$FixtureDirectory = (Resolve-Path -LiteralPath $FixtureDirectory).Path
$fixtureSource = Join-Path $PSScriptRoot 'fixtures/TabIntegration.cs'
$fixtureBinary = Join-Path $FixtureDirectory 'ai.exe'
# Build the .NET Framework fixture with Windows PowerShell first. The same
# native child can then be launched by both Windows PowerShell and pwsh.
if (-not (Test-Path -LiteralPath $fixtureBinary)) {
    if ($PSVersionTable.PSEdition -ne 'Desktop') {
        throw 'Run this test with Windows PowerShell first to build the fixture'
    }
    Add-Type -Path $fixtureSource -OutputAssembly $fixtureBinary -OutputType ConsoleApplication
}
Add-Type -Path $fixtureSource

$init = & $Binary init powershell | Out-String
if ($LASTEXITCODE -ne 0) { throw 'Could not generate integration' }
$tokens = $null
$parseErrors = $null
$null = [System.Management.Automation.Language.Parser]::ParseInput(
    $init, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw 'PowerShell integration contains syntax errors' }

# Use the actual generated handlers and child-process code with an editor
# double so CI needs no interactive console. Native-terminal QA is separate.
$init = $init.Replace('Import-Module PSReadLine -MinimumVersion 2.0 -ErrorAction Stop', '')
$init = $init.Replace('[Microsoft.PowerShell.PSConsoleReadLine]', '[TabTestEditor]')
$init = $init.Replace('[Console]::CursorTop', '7')
$init = $init.Replace('[Console]', '[TabTestConsole]')
$script:mode = 'Windows'
$script:handlers = @{}
function Get-PSReadLineOption { [pscustomobject]@{ EditMode = $script:mode } }
function Set-PSReadLineKeyHandler {
    param($Chord, $ViMode, $BriefDescription, $Description, $ScriptBlock)
    if ($script:mode -eq 'Vi' -and $ViMode -ne 'Insert') {
        throw 'Vi command mode must not be rebound'
    }
    $script:handlers[$Chord] = $ScriptBlock
}
function Assert-Equal($Actual, $Expected, [string]$Label) {
    if ($Actual -cne $Expected) { throw "${Label}: expected [$Expected], got [$Actual]" }
}
function Reset-Editor([string]$Text = '') {
    [TabTestEditor]::Buffer = $Text
    [TabTestEditor]::Cursor = $Text.Length
    [TabTestEditor]::OutputSincePrompt = $false
    [TabTestEditor]::Accepted = 0
    [TabTestEditor]::Redraws = 0
    $script:diagnostics = @()
}

function global:PSConsoleHostReadLine {
    $script:readerCalls++
    Assert-Equal ([TabTestConsole]::OutputEncoding.CodePage) 65001 'Reader starts in UTF-8'
    if ($script:readerThrows) { throw 'Reader interrupted' }
    & $script:handlers.Tab
    # A normal SelfInsert occurs after the Tab handler has returned. Round-trip
    # through the active writer encoding to catch the OEM replacement of emoji.
    [TabTestEditor]::Insert('a')
    $encoding = [TabTestConsole]::OutputEncoding
    $rendered = $encoding.GetString($encoding.GetBytes([TabTestEditor]::Buffer))
    Assert-Equal $rendered ($global:__AishellPromptPrefix + 'a') 'Emoji survives the first typed character'
    return [TabTestEditor]::Buffer
}
$script:originalHostReader = (Get-Command PSConsoleHostReadLine).ScriptBlock

$previousPath = $env:PATH
try {
    $env:PATH = $FixtureDirectory + ';' + $env:PATH
    foreach ($script:mode in @('Windows', 'Emacs', 'Vi')) {
        # Import-Module re-exports PSConsoleHostReadLine when init is reloaded.
        Set-Item Function:\global:PSConsoleHostReadLine -Value $script:originalHostReader
        Invoke-Expression $init
        # Keep stdout/stderr cursor ownership observable without a real terminal.
        function global:Write-AishellErrorLine {
            param([AllowEmptyString()][string]$Message = '')
            [TabTestEditor]::OutputSincePrompt = $true
            $script:diagnostics += $Message
        }

        if ($PSVersionTable.PSEdition -eq 'Desktop') {
            Reset-Editor
            $script:readerCalls = 0
            $script:readerThrows = $false
            $result = PSConsoleHostReadLine
            Assert-Equal $result ($global:__AishellPromptPrefix + 'a') 'Reader returns editable input'
            Assert-Equal $script:readerCalls 1 'Reload calls the original reader once'
            Assert-Equal ([TabTestConsole]::OutputEncoding.CodePage) 850 'Encoding restored before command execution'
            $script:readerThrows = $true
            $interrupted = $false
            try { PSConsoleHostReadLine } catch { $interrupted = $true }
            Assert-Equal $interrupted $true 'Reader exception propagates'
            Assert-Equal ([TabTestConsole]::OutputEncoding.CodePage) 850 'Encoding restored after interruption'
            $script:readerThrows = $false
        }

        Reset-Editor
        & $script:handlers.Tab
        Assert-Equal ([TabTestEditor]::Buffer) $global:__AishellPromptPrefix 'Empty Tab opens AI'
        & $script:handlers.Tab
        & $script:handlers.Enter
        Assert-Equal ([TabTestEditor]::Accepted) 0 'Empty request is never executed'
        Assert-Equal ([TabTestEditor]::Buffer) $global:__AishellPromptPrefix 'Empty request stays editable'

        foreach ($request in @('command', 'quiet', 'answer', 'failure', ('a' + [char]0x00E7 + [char]0x00E3 + 'o'))) {
            foreach ($key in @('Tab', 'Enter')) {
                $line = $global:__AishellPromptPrefix + $request
                Reset-Editor $line
                & $script:handlers[$key]
                $expected = "Write-Output '$request'"
                if ($request -eq 'failure') { $expected = $line }
                if ($request -eq 'answer') { $expected = '' }
                Assert-Equal ([TabTestEditor]::Buffer) $expected "$key $request result"
                Assert-Equal ([TabTestEditor]::Cursor) $expected.Length "$key $request cursor"
                Assert-Equal ([TabTestEditor]::Accepted) 0 "$key must leave result unexecuted"
                Assert-Equal ([TabTestEditor]::OutputSincePrompt) $false 'Diagnostics are synchronized'
                if ($request -ne 'quiet' -and [TabTestEditor]::Redraws -eq 0) {
                    throw 'Diagnostics require a prompt redraw'
                }
                # The next keystroke must edit the command/request normally.
                [TabTestEditor]::Insert('x')
                Assert-Equal ([TabTestEditor]::Buffer) ($expected + 'x') 'Editing after generation'
            }
        }

        Reset-Editor 'Get-Ch'
        & $script:handlers.Tab
        & $script:handlers.Tab
        Assert-Equal ([TabTestEditor]::Completion) $script:mode 'Normal completion respects edit mode'
        & $script:handlers.Enter
        Assert-Equal ([TabTestEditor]::Accepted) 1 'Normal Enter still accepts input'
    }
    Write-Output "PowerShell $($PSVersionTable.PSVersion): Tab integration regression checks passed"
}
finally {
    $env:PATH = $previousPath
}
