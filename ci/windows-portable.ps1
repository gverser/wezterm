<#
.SYNOPSIS
  打一个解压就能用的 Windows 便携包 WezTerm-portable-<版本号>.zip。

.DESCRIPTION
  zip 里是一个 WezTerm-dev 文件夹。解压到当前用户能写的目录，双击 wezterm-gui.exe
  就能用，不用安装。文件夹里带着 wezterm.lua，所以是便携安装：数据存在旁边的 data
  文件夹里，第一次启动时生成。

  包里的文件全部来自本仓库：
    target\release           wezterm.exe、wezterm-gui.exe、wezterm-mux-server.exe、
                             strip-ansi-escapes.exe
    assets\windows           conpty.dll、OpenConsole.exe、libEGL.dll、libGLESv2.dll、
                             mesa\opengl32.dll（和 ci/deploy.sh 打的上游 Windows zip 一样）
    assets\windows\portable  README.txt（填上版本号和打包日期）、wezterm.lua
  不读本机的安装、配置和数据目录，所以包里没有任何会话、令牌或凭据。

  要在 PowerShell 里跑，不要用 Git Bash：.cargo/config.toml 让 OpenSSL 从源码编，
  要用 Strawberry Perl，Git Bash 的 perl 编不了；换了 shell，PATH 一变还会触发全量重编。

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File ci\windows-portable.ps1
  编 release，在仓库根目录生成 WezTerm-portable-<版本号>.zip。

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File ci\windows-portable.ps1 -SkipBuild -OutDir C:\out
  不编译，用 target\release 里现有的 exe 打包，zip 放到 C:\out。
#>
param(
    # 不编译，直接用 target\release 里现有的 exe
    [switch]$SkipBuild,
    # zip 放到哪个目录。默认是仓库根目录，.gitignore 里的 /WezTerm*.zip 会忽略它
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'
$Repo = Split-Path -Parent $PSScriptRoot
$Release = Join-Path $Repo 'target\release'
$Assets = Join-Path $Repo 'assets\windows'
# zip 里顶层文件夹的名字
$Folder = 'WezTerm-dev'
# 相对路径按运行脚本时所在的目录算，下面会切到仓库目录
$OutDir = if ($OutDir) {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
} else {
    $Repo
}

# zip 里的路径 => 来源文件。README.txt 要填版本号和日期，单独写
$Files = [ordered]@{
    'wezterm.exe'            = Join-Path $Release 'wezterm.exe'
    'wezterm-gui.exe'        = Join-Path $Release 'wezterm-gui.exe'
    'wezterm-mux-server.exe' = Join-Path $Release 'wezterm-mux-server.exe'
    'strip-ansi-escapes.exe' = Join-Path $Release 'strip-ansi-escapes.exe'
    'conpty.dll'             = Join-Path $Assets 'conhost\conpty.dll'
    'OpenConsole.exe'        = Join-Path $Assets 'conhost\OpenConsole.exe'
    'libEGL.dll'             = Join-Path $Assets 'angle\libEGL.dll'
    'libGLESv2.dll'          = Join-Path $Assets 'angle\libGLESv2.dll'
    'mesa/opengl32.dll'      = Join-Path $Assets 'mesa\opengl32.dll'
    'wezterm.lua'            = Join-Path $Assets 'portable\wezterm.lua'
}
$Readme = Join-Path $Assets 'portable\README.txt'

function Write-Step($text) {
    Write-Host "`n== $text" -ForegroundColor Cyan
}

function Build-Release {
    $perl = Get-Command perl -ErrorAction SilentlyContinue
    if (-not $perl -or $perl.Source -match '\\usr\\bin\\') {
        $found = if ($perl) { $perl.Source } else { '找不到' }
        throw "OpenSSL 要用 Strawberry Perl 来编，现在的 perl 是：$found。请在普通 PowerShell 里跑，不要用 Git Bash。"
    }
    $cargoArgs = 'build', '--release', '-p', 'wezterm', '-p', 'wezterm-gui',
        '-p', 'wezterm-mux-server', '-p', 'strip-ansi-escapes'
    Write-Host ('> cargo ' + ($cargoArgs -join ' ')) -ForegroundColor DarkGray
    & cargo $cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build 失败，退出码 $LASTEXITCODE"
    }
}

function Get-ReleaseVersion {
    $out = & (Join-Path $Release 'wezterm.exe') --version
    if ($LASTEXITCODE -ne 0 -or -not $out) {
        throw 'target\release\wezterm.exe --version 没跑起来'
    }
    $version = ([string]$out).Trim() -replace '^wezterm\s+', ''
    # 版本号要拿来当文件名，不能是空的或者带路径符号
    if ($version -notmatch '^[\w.-]+$') {
        throw "读出来的版本号不对：'$version'"
    }
    return $version
}

function Write-Zip($zip, $version) {
    Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
    $partial = "$zip.partial"
    foreach ($f in $zip, $partial) {
        if (Test-Path -LiteralPath $f) {
            Write-Host "覆盖同名的旧文件：$f"
            Remove-Item -LiteralPath $f -Force
        }
    }
    try {
        # 不用 ZipFile.CreateFromDirectory：PowerShell 5.1 底下的 .NET Framework 会把路径
        # 写成 WezTerm-dev\xxx，别的解压工具会当成一个带反斜杠的文件名。逐个写，用 /。
        $archive = [IO.Compression.ZipFile]::Open($partial, [IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($name in $Files.Keys) {
                [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                    $archive, $Files[$name], "$Folder/$name", [IO.Compression.CompressionLevel]::Optimal)
            }
            $text = [IO.File]::ReadAllText($Readme, [Text.Encoding]::UTF8)
            $text = $text.Replace('@VERSION@', $version).Replace('@DATE@', (Get-Date -Format 'yyyy-MM-dd'))
            # 仓库里存的可能是 LF；带 BOM、用 CRLF，老版本的记事本也能正常显示中文
            $text = $text -replace "`r?`n", "`r`n"
            $entry = $archive.CreateEntry("$Folder/README.txt", [IO.Compression.CompressionLevel]::Optimal)
            $writer = New-Object IO.StreamWriter($entry.Open(), (New-Object Text.UTF8Encoding $true))
            try {
                $writer.Write($text)
            } finally {
                $writer.Dispose()
            }
        } finally {
            $archive.Dispose()
        }
        Move-Item -LiteralPath $partial -Destination $zip
    } catch {
        if (Test-Path -LiteralPath $partial) {
            Remove-Item -LiteralPath $partial -Force
        }
        throw
    }
}

function Main {
    $clock = [Diagnostics.Stopwatch]::StartNew()

    $head = & git -C $Repo rev-parse --short=8 HEAD
    if ($LASTEXITCODE -ne 0) {
        throw "$Repo 不是 git 仓库"
    }
    Write-Host "源码：$Repo，提交 $head"
    $dirty = @(& git -C $Repo status --porcelain --untracked-files=no)
    if ($dirty.Count -gt 0) {
        Write-Host "注意：有 $($dirty.Count) 个文件改了没提交，会一起编进去，但版本号只认最后一个提交。" -ForegroundColor Yellow
    }

    if ($SkipBuild) {
        Write-Step '跳过编译（-SkipBuild），用 target\release 里现有的 exe'
    } else {
        Write-Step '编译 release'
        Build-Release
    }

    Write-Step '检查文件'
    foreach ($src in @($Files.Values) + $Readme) {
        if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
            throw "缺文件：$src"
        }
    }
    $version = Get-ReleaseVersion
    Write-Host "版本：$version"
    if ($version -notlike "*$head*") {
        Write-Host "注意：target\release 里的 exe 不是当前提交 $head 编的。要打当前的代码，去掉 -SkipBuild 重跑。" -ForegroundColor Yellow
    }

    Write-Step '打 zip'
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
    $zip = Join-Path $OutDir "WezTerm-portable-$version.zip"
    Write-Zip $zip $version

    Write-Step '完成'
    Write-Host "版本  ：$version"
    Write-Host "便携包：$zip"
    Write-Host ('大小  ：{0:N1} MB' -f ((Get-Item -LiteralPath $zip).Length / 1MB))
    Write-Host ('用时  ：{0:hh\:mm\:ss}' -f $clock.Elapsed)
}

$code = 0
Push-Location $Repo
try {
    Main
} catch {
    Write-Host "`n失败：$($_.Exception.Message)" -ForegroundColor Red
    $code = 1
} finally {
    Pop-Location
}
exit $code
