# Как запустить HomeLLM (текущая версия)

Пошагово для Windows 10/11. Готовых установщиков пока нет — приложение собирается из исходников
(установщики — задача HOM-8). Команды — для Windows PowerShell, выполняйте по одной.

## 1. Инструменты (один раз)

Нужно ~15 ГБ свободного места на диске C или на том диске, куда ставите.

```powershell
winget install --id Rustlang.Rustup -e
winget install --id Kitware.CMake -e
winget install --id LLVM.LLVM -e
winget install --id KhronosGroup.VulkanSDK -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Затем **закройте терминал и откройте новый** — иначе он не увидит установленное. Проверка:

```powershell
cargo --version
cmake --version
```

Если bindgen не находит libclang, один раз задайте:

```powershell
[Environment]::SetEnvironmentVariable("LIBCLANG_PATH", "C:\Program Files\LLVM\bin", "User")
```

## 2. Код и сборка

```powershell
git clone https://github.com/dzotyara/HomeLLM
cd HomeLLM
cargo build --release
```

Первая сборка идёт 5–15 минут (собирается llama.cpp с Vulkan). Если она упала с ошибкой
`MSB8066 ... vulkan-shaders-gen` — это известный сбой, просто запустите `cargo build --release` ещё раз.

Без видеокарты или без Vulkan SDK — сборка только на процессоре:

```powershell
cargo build --release -p homellm-app --no-default-features
```

## 3. Запуск

```powershell
.\target\release\homellm-app.exe
```

При первом запуске откроется окно «Модели»: нажмите «Скачать» у модели с пометкой «рекомендую», потом
«Запустить». Дальше модель запускается сама. Модель можно сменить кнопкой с её именем в шапке — или
попросить в чате: «скачай qwen3.5-9b», «переключись на gemma4-12b».

Консольная версия (для отладки):

```powershell
.\target\release\homellm.exe hw
.\target\release\homellm.exe models
.\target\release\homellm.exe pull qwen3-4b
.\target\release\homellm.exe chat
```

## 4. Где что лежит

| Что | Где (по умолчанию) | Как поменять |
|---|---|---|
| Модели | `%APPDATA%\HomeLLM\data\models` | «Настройки» → «Папка для моделей» или переменная `HOMELLM_MODELS_DIR` |
| Чаты | `%APPDATA%\HomeLLM\data\chats.json` | — |
| Настройки | `%APPDATA%\HomeLLM\config\settings.json` | окно «Настройки» или просьба в чате |

Модели весят 1–20 ГБ: если диск C маленький, перенесите папку моделей на другой диск.

## 5. Частые проблемы

| Симптом | Что делать |
|---|---|
| `cargo`, `cmake` «не найдено» | Откройте новый терминал после установки |
| `Access is denied` при сборке | Закройте запущенный HomeLLM — Windows не даёт перезаписать открытую программу |
| `Unable to find libclang` | Шаг с `LIBCLANG_PATH` выше, потом новый терминал |
| `MSB8066 vulkan-shaders-gen` | Повторите `cargo build --release` |
| Загрузка модели оборвалась | Нажмите «Скачать» ещё раз — продолжится с места обрыва |
| Модель отвечает странно, путает команды | Маленькие модели (1–2B) слабые: возьмите 8B и больше |
| В PowerShell ошибка про `&&` | Windows PowerShell 5.1 не знает `&&` — выполняйте команды по одной |
