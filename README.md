# Джарвис

Голосовой ассистент для Windows по мотивам Iron Man: слушает микрофон в фоне, откликается на
«Джарвис», выполняет команды и работает офлайн. Ядро написано на Rust.

> **Статус: этап 0 (каркас).** Пока есть конфиг, логи, скачивание моделей и проверка окружения
> `jarvis --check`. Слух, команды, разрешения и остальное появятся на следующих этапах
> (см. `JARVIS_SPEC.md`, раздел 11).

## Требования

- Windows 10 (1809+) или Windows 11, x64.
- Rust stable (MSVC) и Visual Studio Build Tools с компонентом «Разработка классических приложений на C++».
- Микрофон. Для игрового режима лучше гарнитура (см. ниже).

## Установка

### 1. Инструменты

Если Rust ещё не установлен:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup -e
```

После установки откройте новый терминал и проверьте: `cargo --version`.

### 2. Модели и библиотеки

```powershell
.\scripts\download-models.ps1          # обязательное: libvosk + русская модель для команд (~60 МБ)
.\scripts\download-models.ps1 -Full    # плюс полная модель для свободной речи (~1.9 ГБ, нужна с этапа 7)
```

Скрипт скачивает архивы с докачкой при обрыве, сверяет SHA-256 с `scripts/assets.json` и распаковывает
в `models/` и `lib/`. Уже установленное пропускается; `-Force` переустанавливает.

Если PowerShell запрещает запуск скриптов:
`powershell -ExecutionPolicy Bypass -File .\scripts\download-models.ps1`.

| Ресурс | Куда | Зачем |
|---|---|---|
| libvosk 0.3.45 | `lib/vosk-win64-0.3.45/` | движок распознавания речи (последняя сборка под Windows) |
| Vosk small-ru 0.22 | `models/vosk-model-small-ru-0.22/` | команды; поддерживает ограниченную грамматику |
| Vosk ru 0.42 | `models/vosk-model-ru-0.42/` | свободная речь (вопросы к LLM), необязательно |

### 3. Сборка и проверка

```powershell
cargo build --release
.\target\release\jarvis.exe --check
```

`--check` печатает отчёт: ОС, права, конфиг, микрофон (со списком устройств), модели.
Код возврата не 0, если есть ошибка.

```
[ OK ] ОС                  Windows 11 Pro 24H2 (сборка 26100.9550)
[ OK ] Права               обычный пользователь
[ OK ] Конфиг              config/default.toml (config/user.toml нет — всё по умолчанию)
[ OK ] Микрофон            системный по умолчанию: Микрофон (ME6S) (48000 Гц, 1 кан., f32)
[ OK ] libvosk 0.3.45      lib\vosk-win64-0.3.45 — движок распознавания речи ...
[ OK ] Vosk small-ru 0.22  models\vosk-model-small-ru-0.22 — русская модель для команд ...
[ -- ] Vosk ru 0.42        не скачано — ...; необязательно (scripts\download-models.ps1 -Full)
```

## Запуск

```powershell
.\target\release\jarvis.exe            # или: cargo run --release
.\target\release\jarvis.exe --help
```

`jarvis` сам находит корень проекта (папку с `config/`), поднимаясь от текущей папки и от папки
программы. Указать явно: `--root F:\JARVIS`.

## Настройка

- `config/default.toml` — значения по умолчанию с комментариями. Лежит в git, не меняйте его.
- `config/user.toml` — ваши изменения: только нужные ключи, в тех же секциях. В git не попадает.

```toml
# config/user.toml
[audio]
input_device = "Fuxi-H3"   # часть имени микрофона; "default" — системный

[session]
follow_up_secs = 15
```

С ошибкой в конфиге Джарвис не стартует и пишет, что не так:

```
Ошибка: конфиг не загружен, запуск отменён: F:\JARVIS\config\user.toml: неизвестный параметр
`audio.input_devise` (такого нет в config/default.toml — опечатка?)
```

Логи: `logs/jarvis.ГГГГ-ММ-ДД.log`, ротация раз в сутки, хранится `logging.max_files` файлов.
Подробнее на время отладки: `logging.level = "debug"` или переменная `RUST_LOG=debug`.

## Структура

```
crates/
  jarvis-core/       конфиг, логи, ресурсы (позже: автомат состояний, разрешения, команды)
  jarvis-platform/   Windows API: версия ОС, права (позже: клавиши, трей, питание)
  jarvis-app/        бинарник jarvis
config/              default.toml (в git), user.toml и permissions.toml (ваши, не в git)
scripts/             download-models.ps1 и assets.json (URL + SHA-256 ресурсов)
models/, lib/        скачанные модели и библиотеки (не в git)
```

Остальные крейты (`jarvis-audio`, `jarvis-nlu`, `jarvis-actions`, конфигуратор) появятся на своих этапах.

## Как добавить свою команду

Появится на этапе 2 (манифесты `commands/*/command.toml`).

## Игровой режим

Играйте в наушниках: иначе микрофон слышит звук игры, и распознавание заклинаний ухудшается.

## Разработка

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

Правила проекта для Claude Code — в `CLAUDE.md`, полное ТЗ — в `JARVIS_SPEC.md`.

## Решение проблем

| Симптом | Что делать |
|---|---|
| `cargo` не найден | Откройте новый терминал после установки Rustup или добавьте `%USERPROFILE%\.cargo\bin` в PATH. |
| `link.exe not found` при сборке | Не установлены Build Tools с компонентом C++ (шаг 1). |
| `--check`: «не скачано» / «нет метки» | Запустите `scripts\download-models.ps1` (при повреждении — с `-Force`). |
| `--check`: микрофон не найден | Проверьте `audio.input_device` — подстрока имени из списка «устройства ввода». |
| Загрузка модели оборвалась | Запустите скрипт ещё раз — скачивание продолжится с места обрыва. |
