# Checks what building HomeLLM on Windows needs and says how to fix what is missing.
# Run from the repository folder:  powershell -ExecutionPolicy Bypass -File scripts\check-windows.ps1
# Changes nothing on the computer: only reads and prints.

$ok = $true

function Report($good, $what, $fix) {
    if ($good) {
        Write-Host "[ok]  $what" -ForegroundColor Green
    } else {
        Write-Host "[!!]  $what" -ForegroundColor Red
        if ($fix) { Write-Host "      $fix" -ForegroundColor Yellow }
        $script:ok = $false
    }
}

function Warn($what, $fix) {
    Write-Host "[--]  $what" -ForegroundColor Yellow
    if ($fix) { Write-Host "      $fix" -ForegroundColor Yellow }
}

Write-Host "Проверка окружения для сборки HomeLLM`n"

# Rust
Report ([bool](Get-Command cargo -ErrorAction SilentlyContinue)) "Rust (cargo)" `
    "winget install --id Rustlang.Rustup -e   (потом новый терминал)"

# CMake
Report ([bool](Get-Command cmake -ErrorAction SilentlyContinue)) "CMake" `
    "winget install --id Kitware.CMake -e   (потом новый терминал)"

# C++ compiler: Visual Studio Build Tools with the C++ workload
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$msvc = $false
if (Test-Path $vswhere) {
    $msvc = [bool](& $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
}
Report $msvc "Компилятор C++ (Visual Studio Build Tools)" `
    'winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'

# libclang: bindgen needs it for the llama.cpp bindings
$clangDirs = @($env:LIBCLANG_PATH, "$env:ProgramFiles\LLVM\bin", "$env:ProgramFiles\LLVM\lib") | Where-Object { $_ }
$libclang = $clangDirs | Where-Object { (Test-Path "$_\libclang.dll") -or (Test-Path "$_\clang.dll") } | Select-Object -First 1
if ($libclang) {
    Report $true "LLVM (libclang): $libclang"
    if (-not $env:LIBCLANG_PATH) {
        Warn "LIBCLANG_PATH не задан: обычно не нужно, но если bindgen не найдёт libclang:" `
            "[Environment]::SetEnvironmentVariable('LIBCLANG_PATH', '$libclang', 'User')   (потом новый терминал)"
    }
} else {
    Report $false "LLVM (libclang) не найден — из-за этого «Unable to find libclang»" `
        "winget install --id LLVM.LLVM -e   (потом новый терминал)"
}

# Vulkan SDK: optional, for the GPU build
$vulkan = $env:VULKAN_SDK -and (Test-Path "$env:VULKAN_SDK\Bin\glslc.exe")
if ($vulkan) {
    Report $true "Vulkan SDK: $env:VULKAN_SDK"
} else {
    Warn "Vulkan SDK не найден: сборка пойдёт только на процессоре" `
        "Для видеокарты: winget install --id KhronosGroup.VulkanSDK -e   Без неё: cargo build --release -p homellm-app --no-default-features"
}

# Path length: MSBuild's file tracker keeps the 260-character limit even with Windows' long
# paths on, and CMake's builds under target/ nest deep: FTK1011 on long paths.
$here = (Get-Location).Path
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $here "target" }
if ($target.Length -gt 22) {
    Warn "Папка сборки $target — длинный путь ($($target.Length) символов): сборка с Vulkan может упасть с FTK1011 (лимит путей MSBuild)" `
        "Склонируйте проект в короткую папку (например C:\src\HomeLLM) или собирайте в короткую: `$env:CARGO_TARGET_DIR = 'C:\t'"
} else {
    Report $true "Путь к сборке короткий: $target"
}

# Disk space: tools, the build and a model or two
$drive = (Get-Item $here).PSDrive
$freeGb = [math]::Round($drive.Free / 1GB)
if ($freeGb -lt 15) {
    Warn "На диске ${drive}: свободно $freeGb ГБ — для сборки и моделей нужно ~15 ГБ" ""
} else {
    Report $true "Свободно на диске ${drive}: $freeGb ГБ"
}

Write-Host ""
if ($ok) {
    Write-Host "Всё нужное есть. Сборка: cargo build --release" -ForegroundColor Green
} else {
    Write-Host "Установите отмеченное [!!], откройте новый терминал и запустите проверку ещё раз." -ForegroundColor Red
    exit 1
}
