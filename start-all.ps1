# batch-pnpm-dev-debug.ps1
# 调试版本，逐步显示执行过程

Write-Host "=== 脚本开始执行 ===" -ForegroundColor Cyan

$projectRoot = "D:\project"
Write-Host "项目根目录: $projectRoot" -ForegroundColor Yellow

# 检查目录
if (-not (Test-Path $projectRoot)) {
    Write-Host "错误: 目录不存在！" -ForegroundColor Red
    Read-Host "按 Enter 退出"
    exit
}

Write-Host "正在扫描项目..." -ForegroundColor Yellow

$projects = Get-ChildItem -Path $projectRoot -Directory | Where-Object {
    $_.Name -notin @('WPS云盘', '此电脑', '网络')
}

Write-Host "找到 $($projects.Count) 个文件夹" -ForegroundColor Yellow

# 过滤有 package.json 的
$validProjects = @()
foreach ($p in $projects) {
    $pkgPath = Join-Path $p.FullName "package.json"
    if (Test-Path $pkgPath) {
        $validProjects += $p
        Write-Host "  ✓ $($p.Name) (有 package.json)" -ForegroundColor Green
    } else {
        Write-Host "  ✗ $($p.Name) (无 package.json)" -ForegroundColor Gray
    }
}

$projects = $validProjects

if ($projects.Count -eq 0) {
    Write-Host "`n没有找到任何有效的项目！" -ForegroundColor Red
    Read-Host "按 Enter 退出"
    exit
}

Write-Host "`n可用的项目：" -ForegroundColor Cyan
for ($i = 0; $i -lt $projects.Count; $i++) {
    Write-Host "  [$i] $($projects[$i].Name)" -ForegroundColor Yellow
}

Write-Host "`n选项：" -ForegroundColor Cyan
Write-Host "  [a] 启动所有项目" -ForegroundColor Yellow
Write-Host "  [q] 退出" -ForegroundColor Yellow

$selection = Read-Host "`n请输入序号（用逗号分隔）"

Write-Host "你输入了: $selection" -ForegroundColor Cyan

if ($selection -eq 'q') {
    Write-Host "退出" -ForegroundColor Yellow
    exit
}

if ($selection -eq 'a') {
    $selectedProjects = $projects
    Write-Host "选择了所有项目" -ForegroundColor Green
} else {
    $indices = $selection -split ',' | ForEach-Object { $_.Trim() }
    $selectedProjects = @()
    foreach ($idx in $indices) {
        if ($idx -match '^\d+$') {
            $num = [int]$idx
            if ($num -lt $projects.Count) {
                $selectedProjects += $projects[$num]
                Write-Host "选择了: $($projects[$num].Name)" -ForegroundColor Green
            } else {
                Write-Host "警告: 序号 $idx 超出范围" -ForegroundColor Yellow
            }
        } else {
            Write-Host "警告: '$idx' 不是有效数字" -ForegroundColor Yellow
        }
    }
}

if ($selectedProjects.Count -eq 0) {
    Write-Host "没有选择任何项目，退出" -ForegroundColor Red
    Read-Host "按 Enter 退出"
    exit
}

Write-Host "`n准备启动 $($selectedProjects.Count) 个项目..." -ForegroundColor Cyan

# 优先用 Windows Terminal（wt.exe）：所有项目开在同一个窗口的不同 tab。
# 找不到 wt 时回退为各自独立窗口。
# 注意：不依赖 PATH —— 有些环境 PATH 里没有 WindowsApps 目录，直接用固定路径找。
$wtCmd = $null
$wtFromPath = Get-Command wt.exe -ErrorAction SilentlyContinue
if ($wtFromPath) {
    $wtCmd = $wtFromPath.Source
} else {
    # 应用执行别名的固定位置
    $wtAlias = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\wt.exe'
    if (Test-Path $wtAlias) { $wtCmd = $wtAlias }
}
if ($wtCmd) { Write-Host "wt.exe: $wtCmd" -ForegroundColor Gray }

# 先收集每个项目的启动命令（base64 编码，规避引号/空格/编码问题）
$tabSpecs = @()
foreach ($project in $selectedProjects) {
    $projectPath = $project.FullName
    $projectName = $project.Name
    Write-Host "启动: $projectName" -ForegroundColor Green

    # 转义单引号，避免路径/名称中含 ' 时命令被截断
    $safePath = $projectPath.Replace("'", "''")
    $safeName = $projectName.Replace("'", "''")

    $cmd = @"
Set-Location -LiteralPath '$safePath'
Write-Host '=== $safeName ===' -ForegroundColor Magenta
Write-Host '路径: $safePath' -ForegroundColor Gray
pnpm dev
"@

    # -NoExit 让 tab 在 pnpm dev 退出后保持打开，方便查看报错
    $encoded = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($cmd))
    $tabSpecs += @{ Name = $projectName; Encoded = $encoded }
}

function Start-FallbackWindows {
    param($specs)
    Write-Host "回退: 每个项目独立窗口启动" -ForegroundColor Yellow
    foreach ($t in $specs) {
        Start-Process powershell -ArgumentList '-NoExit', '-EncodedCommand', $t.Encoded -WindowStyle Normal
        Start-Sleep -Milliseconds 200
    }
}

if ($wtCmd -and $tabSpecs.Count -gt 0) {
    $wtArgs = @()

    # 关键：如果脚本本身就运行在 Windows Terminal 里，用 -w last 把 tab 直接追加到当前窗口；
    # 否则新开一个窗口，所有 tab 都放在这一个窗口里。
    if ($env:WT_SESSION) {
        $wtArgs += @('-w', 'last')
        Write-Host "启动方式: 追加 tab 到当前窗口 (Windows Terminal)" -ForegroundColor Cyan
    } else {
        Write-Host "启动方式: 新开一个 Windows Terminal 窗口, 共 $($tabSpecs.Count) 个 tab" -ForegroundColor Cyan
    }

    $i = 0
    foreach ($t in $tabSpecs) {
        if ($i -gt 0) { $wtArgs += ';' }
        # --suppressApplicationTitle 让标题固定为项目名, 不被 pnpm/vite 改掉
        $wtArgs += @(
            'new-tab',
            '--title', $t.Name,
            '--suppressApplicationTitle',
            'powershell', '-NoExit', '-EncodedCommand', $t.Encoded
        )
        $i++
    }

    & $wtCmd @wtArgs

    if ($LASTEXITCODE -ne 0) {
        Write-Host "wt.exe 调用失败 (退出码 $LASTEXITCODE)" -ForegroundColor Red
        Start-FallbackWindows $tabSpecs
    } else {
        Write-Host "已启动 $($tabSpecs.Count) 个 tab" -ForegroundColor Green
    }
} elseif ($tabSpecs.Count -gt 0) {
    Write-Host "未找到 wt.exe (Windows Terminal): PATH 和 $env:LOCALAPPDATA\Microsoft\WindowsApps\wt.exe 都不存在" -ForegroundColor Yellow
    Start-FallbackWindows $tabSpecs
}

Write-Host "`n完成！所有项目已启动" -ForegroundColor Green
Read-Host "按 Enter 退出"
