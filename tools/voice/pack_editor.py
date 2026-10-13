"""Редактор записей голоса Джарвиса: локальная страница в браузере.

Показывает записи, которые сейчас использует Джарвис (ответы и приветствия
звукового пакета из config — voice.pack, и фразы личных команд commands/my),
даёт их послушать, подрезать, вырезать куски, сократить паузу перед «сэр»,
поменять громкость, включить или выключить вариант ответа — и сохранить.

- Сервер слушает только 127.0.0.1; запросы на изменение требуют ключ, который
  есть только у открытой страницы (чужой сайт не сможет ничего записать).
- Перед каждой записью файла — копия в data/voice-backup/ (не в git);
  «Вернуть исходную» восстанавливает самую первую копию.
- Изменения Джарвис подхватит после перезапуска (кнопка на странице).

Запуск (только стандартная библиотека Python 3.11+):
  py -3.12 tools\\voice\\pack_editor.py           # откроет страницу в браузере
"""

from __future__ import annotations

import json
import re
import secrets
import shutil
import subprocess
import sys
import time
import tomllib
import webbrowser
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlparse

ROOT = Path(__file__).resolve().parents[2]
PAGE = Path(__file__).with_name("pack_editor.html")
BACKUP = ROOT / "data" / "voice-backup"
COMMANDS_MY = ROOT / "commands" / "my"
HOST, PORT = "127.0.0.1", 8765
TOKEN = secrets.token_urlsafe(24)
MAX_UPLOAD = 20 * 1024 * 1024
NAME_RE = re.compile(r"^[\w.-]+\.wav$", re.ASCII)

# Ответы пакета — порядок и подписи на странице.
REPLIES = {
    "wake": "Отклик на «Джарвис»",
    "ok": "Выполнено",
    "not_understood": "Не понял (просьба повторить)",
    "not_heard": "Не расслышал (тишина после «Джарвис»)",
    "failed": "Не удалось",
    "ask": "Вопрос-разрешение",
    "denied": "После отказа",
    "out_of_range": "Значение недопустимо",
}
GREETINGS = {"morning": "Утро", "day": "День", "evening": "Вечер", "night": "Ночь"}


def pack_dir() -> Path:
    """Папка звукового пакета, который включён в настройках (user.toml поверх default.toml)."""
    pack = "default"
    for name in ("default.toml", "user.toml"):
        path = ROOT / "config" / name
        if path.exists():
            pack = tomllib.loads(path.read_text(encoding="utf-8")).get("voice", {}).get("pack", pack)
    return ROOT / "sounds" / pack


def read_pack() -> dict:
    return tomllib.loads((pack_dir() / "pack.toml").read_text(encoding="utf-8"))


def write_pack(data: dict) -> None:
    """pack.toml в том же виде, что пишет make_pack.py (через временный файл)."""

    def files(names: list[str]) -> str:
        return ", ".join(json.dumps(n, ensure_ascii=False) for n in names)

    lines = [f"name = {json.dumps(data['name'], ensure_ascii=False)}", "", "[replies]"]
    replies = data.get("replies", {})
    # Порядок строк — как был в файле (dict помнит порядок чтения).
    lines += [f"{k} = [{files(v)}]" for k, v in replies.items() if v]
    greetings = data.get("greetings", {})
    if greetings:
        lines += ["", "[greetings]"]
        lines += [f"{k} = [{files(v)}]" for k, v in greetings.items() if v]
    path = pack_dir() / "pack.toml"
    tmp = path.with_suffix(".toml.tmp")
    tmp.write_text("\n".join(lines) + "\n", encoding="utf-8")
    tmp.replace(path)


def number(name: str) -> tuple:
    return tuple(int(n) for n in re.findall(r"\d+", name)) or (0,)


def group_of(name: str) -> str | None:
    """К какому ответу относится файл по имени (самый длинный подходящий ключ)."""
    keys = [k for k in REPLIES if name.startswith(k + "_")]
    return max(keys, key=len) if keys else None


def inventory() -> dict:
    """Что показать на странице: ответы (с выключенными вариантами), приветствия, команды."""
    data = read_pack()
    replies = data.get("replies", {})
    folder = pack_dir()
    used = {f for files in replies.values() for f in files}
    groups = []
    for key, title in REPLIES.items():
        enabled = list(replies.get(key, []))
        spare = sorted(
            (p.name for p in folder.glob("*.wav") if p.name not in used and group_of(p.name) == key),
            key=number,
        )
        clips = [{"area": "pack", "name": n, "enabled": True} for n in enabled]
        clips += [{"area": "pack", "name": n, "enabled": False} for n in spare]
        if clips:
            groups.append({"key": key, "title": title, "toggle": True, "clips": clips})
    for key, title in GREETINGS.items():
        names = data.get("greetings", {}).get(key, [])
        if names:
            groups.append(
                {
                    "key": f"greeting_{key}",
                    "title": f"Приветствие: {title.lower()}",
                    "toggle": False,
                    "clips": [{"area": "pack", "name": n, "enabled": True} for n in names],
                }
            )
    if COMMANDS_MY.is_dir():
        names = sorted(p.name for p in COMMANDS_MY.glob("*.wav"))
        if names:
            groups.append(
                {
                    "key": "commands_my",
                    "title": "Фразы личных команд (commands/my)",
                    "toggle": False,
                    "clips": [{"area": "cmd", "name": n, "enabled": True} for n in names],
                }
            )
    for g in groups:
        for c in g["clips"]:
            c["backups"] = len(backups(c["area"], c["name"]))
    return {"pack": data.get("name", folder.name), "groups": groups}


def clip_path(area: str, name: str) -> Path:
    """Путь к записи — только известные папки и простые имена (без `..` и подпапок)."""
    if not NAME_RE.match(name):
        raise ValueError(f"недопустимое имя файла: {name}")
    folder = {"pack": pack_dir(), "cmd": COMMANDS_MY}.get(area)
    if folder is None:
        raise ValueError(f"неизвестная папка: {area}")
    return folder / name


def backups(area: str, name: str) -> list[Path]:
    folder = BACKUP / area / name
    return sorted(folder.glob("*.wav")) if folder.is_dir() else []


def save_clip(area: str, name: str, wav: bytes) -> None:
    if len(wav) < 44 or wav[:4] != b"RIFF" or wav[8:12] != b"WAVE":
        raise ValueError("это не WAV")
    path = clip_path(area, name)
    if not path.is_file():
        raise ValueError(f"записи {name} нет")
    folder = BACKUP / area / name
    folder.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(path, folder / f"{time.strftime('%Y%m%d-%H%M%S')}-{time.time_ns() % 10**6:06d}.wav")
    tmp = path.with_suffix(".wav.tmp")
    tmp.write_bytes(wav)
    tmp.replace(path)


def restore_clip(area: str, name: str) -> None:
    """Вернуть исходную запись — самую первую копию; копии остаются."""
    first = backups(area, name)
    if not first:
        raise ValueError("исходной копии нет — запись не менялась")
    path = clip_path(area, name)
    shutil.copyfile(path, BACKUP / area / name / f"{time.strftime('%Y%m%d-%H%M%S')}-before-restore.wav")
    shutil.copyfile(first[0], path)


def toggle(name: str, enabled: bool) -> None:
    key = group_of(name)
    if key is None:
        raise ValueError(f"{name} не относится ни к одному ответу")
    clip_path("pack", name)  # проверка имени
    data = read_pack()
    replies = data.setdefault("replies", {})
    files = list(replies.get(key, []))
    if enabled and name not in files:
        files = sorted(files + [name], key=number)
    elif not enabled and name in files:
        if key == "wake" and len(files) == 1:
            raise ValueError("у отклика на «Джарвис» должен остаться хотя бы один вариант")
        files.remove(name)
    replies[key] = files
    write_pack(data)


def restart_jarvis() -> None:
    # Тот же файл, что и двойной щелчок: остановит, дождётся, запустит.
    subprocess.Popen(
        ["cmd.exe", "/c", str(ROOT / "jarvis-start.cmd")],
        cwd=ROOT,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )


class Handler(BaseHTTPRequestHandler):
    server_version = "JarvisVoiceEditor"

    def log_message(self, fmt: str, *args) -> None:  # тише в консоли
        pass

    def send(self, status: int, body: bytes, kind: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def send_json(self, data: object, status: int = 200) -> None:
        self.send(status, json.dumps(data, ensure_ascii=False).encode("utf-8"), "application/json; charset=utf-8")

    def do_GET(self) -> None:
        url = urlparse(self.path)
        try:
            if url.path == "/":
                page = PAGE.read_text(encoding="utf-8").replace("{{TOKEN}}", TOKEN)
                self.send(200, page.encode("utf-8"), "text/html; charset=utf-8")
            elif url.path == "/api/pack":
                self.send_json(inventory())
            elif url.path.startswith("/audio/"):
                _, _, area, name = url.path.split("/", 3)
                self.send(200, clip_path(area, unquote(name)).read_bytes(), "audio/wav")
            else:
                self.send(HTTPStatus.NOT_FOUND, b"not found", "text/plain")
        except (ValueError, OSError) as e:
            self.send_json({"error": str(e)}, 400)

    def do_POST(self) -> None:
        # Ключ — в заголовке: чужой сайт не может отправить такой запрос без
        # предварительной проверки CORS, на которую сервер не отвечает.
        if not secrets.compare_digest(self.headers.get("X-Token", ""), TOKEN):
            self.send_json({"error": "нет ключа страницы — обновите страницу"}, 403)
            return
        url = urlparse(self.path)
        length = int(self.headers.get("Content-Length", "0"))
        if length > MAX_UPLOAD:
            self.send_json({"error": "слишком большой файл"}, 413)
            return
        body = self.rfile.read(length)
        try:
            if url.path.startswith("/api/save/"):
                _, _, _, area, name = url.path.split("/", 4)
                save_clip(area, unquote(name), body)
            elif url.path.startswith("/api/restore/"):
                _, _, _, area, name = url.path.split("/", 4)
                restore_clip(area, unquote(name))
            elif url.path == "/api/toggle":
                req = json.loads(body)
                toggle(str(req["name"]), bool(req["enabled"]))
            elif url.path == "/api/restart":
                restart_jarvis()
            else:
                self.send_json({"error": "неизвестная команда"}, 404)
                return
            self.send_json(inventory())
        except (ValueError, OSError, KeyError, json.JSONDecodeError) as e:
            self.send_json({"error": str(e)}, 400)


def main() -> None:
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    url = f"http://{HOST}:{PORT}/"
    print(f"Редактор голоса: {url}  (пакет: {pack_dir().name}). Остановить — Ctrl+C.")
    if "--no-browser" not in sys.argv:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
