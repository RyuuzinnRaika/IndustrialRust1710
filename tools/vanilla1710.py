#!/usr/bin/env python3
"""Run the unmodified *official* Minecraft Java 1.7.10 dedicated server.

The Rust server in this repo is an experimental implementation. This launcher
offers complete vanilla survival gameplay NOW by fetching Mojang's own server
binary after confirming its pinned SHA-1. No server JAR is stored in Git.

The EULA is NOT accepted automatically. The operator must separately read
and accept it by editing eula.txt. Online authentication + whitelist stay on.
"""
from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from urllib.request import Request, urlopen

VERSION = "1.7.10"
SERVER_SHA1 = "952438ac4e01b4d115c5fc38f891710c4941df29"
SERVER_SIZE = 9_605_030
JAR_NAME = "minecraft_server.1.7.10.jar"
# Mojang's official Log4j mitigation for Minecraft 1.7 through 1.11.2.
# See https://www.minecraft.net/en-us/article/important-message--security-vulnerability-java-edition
LOG4J_NAME = "log4j2_17-111.xml"
LOG4J_SHA1 = "dd2b723346a8dcd48e7f4d245f6bf09e98db9696"
LOG4J_MAX_SIZE = 64 * 1024
LOG4J_URLS = (
    f"https://launcher.mojang.com/v1/objects/{LOG4J_SHA1}/{LOG4J_NAME}",
    f"https://piston-data.mojang.com/v1/objects/{LOG4J_SHA1}/{LOG4J_NAME}",
)
DEFAULT_DIR = Path("runtime/vanilla-1.7.10")
EULA_URL = "https://www.minecraft.net/en-us/eula"
# Both URLs are Mojang-operated CDN frontends for the same official object.
MOJANG_URLS = (
    f"https://piston-data.mojang.com/v1/objects/{SERVER_SHA1}/server.jar",
    f"https://launcher.mojang.com/v1/objects/{SERVER_SHA1}/server.jar",
)


class SetupError(Exception):
    """An actionable installation or security error."""


def properties_text(bind: str, port: int, players: int) -> str:
    """Minecraft 1.7.10-compatible properties; never disable online identity."""
    return "\n".join((
        "# Generated once by IndustrialRust1710's official vanilla launcher.",
        "# Changes to this file are preserved by subsequent prepare commands.",
        "online-mode=true",
        "white-list=true",
        "server-ip=" + bind,
        "server-port=" + str(port),
        "level-name=world",
        "level-type=DEFAULT",
        "generate-structures=true",
        "gamemode=0",
        "difficulty=2",
        "hardcore=false",
        "max-players=" + str(players),
        "view-distance=8",
        "spawn-monsters=true",
        "spawn-animals=true",
        "spawn-npcs=true",
        "pvp=true",
        "enable-command-block=false",
        "enable-rcon=false",
        "enable-query=false",
        "allow-flight=false",
        "spawn-protection=0",
        "motd=IndustrialRust1710 Official Vanilla 1.7.10",
        "snooper-enabled=false",
        "",
    ))


def parse_properties(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise SetupError(f"{path} がありません。先に prepare を実行してください。")
    values: dict[str, str] = {}
    for line in path.read_text(encoding="latin-1").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("#", "!")):
            continue
        if "=" not in stripped:
            raise SetupError(f"server.properties に不正な行があります: {stripped[:80]}")
        name, value = stripped.split("=", 1)
        values[name.strip()] = value.strip()
    return values


def check_security(directory: Path, *, require_eula: bool) -> None:
    props = parse_properties(directory / "server.properties")
    if props.get("online-mode") != "true":
        raise SetupError("online-mode=true が必要です（なりすまし防止）。")
    if props.get("white-list") != "true":
        raise SetupError("white-list=true が必要です（許可リスト）。")
    if props.get("enable-rcon", "false") != "false":
        raise SetupError("enable-rcon=false が必要です。")
    try:
        port = int(props.get("server-port", ""))
        if not 1 <= port <= 65535:
            raise ValueError("port out of range")
    except ValueError as exc:
        raise SetupError("server-port が不正です。") from exc
    try:
        ipaddress.IPv4Address(props.get("server-ip", ""))
    except ipaddress.AddressValueError as exc:
        raise SetupError("server-ip は明示的なIPv4アドレスを使用してください。") from exc
    wl = directory / "whitelist.json"
    if not wl.is_file():
        raise SetupError("whitelist.json がありません（初期値 [] が必要）。")
    try:
        data = json.loads(wl.read_text(encoding="utf-8"))
    except (ValueError, UnicodeError) as exc:
        raise SetupError("whitelist.json のJSONが壊れています。") from exc
    if not isinstance(data, list) or any(not isinstance(x, dict) for x in data):
        raise SetupError("whitelist.json はJSON配列でなければなりません。")
    if require_eula:
        eula = directory / "eula.txt"
        if not eula.is_file() or not re.search(
            r"(?m)^\s*eula\s*=\s*true\s*$", eula.read_text(encoding="utf-8")
        ):
            raise SetupError(
                f"Minecraft EULAへの同意が必要です。{EULA_URL} を読み、"
                f"{eula} の eula=false をご自身で eula=true に変更してください。"
            )


def verify_jar(path: Path, *, sha1: str = SERVER_SHA1, size: int = SERVER_SIZE) -> None:
    if path.is_symlink() or not path.is_file():
        raise SetupError(f"公式JARが見つかりません: {path}")
    if path.stat().st_size != size:
        raise SetupError(f"公式JARのサイズが一致しません: {path}")
    digest = hashlib.sha1()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    if digest.hexdigest() != sha1:
        raise SetupError(f"公式JARのSHA-1が一致しません: {path}")


def download_server(path: Path) -> None:
    if path.exists() or path.is_symlink():
        verify_jar(path, sha1=SERVER_SHA1, size=SERVER_SIZE)
        print(f"検証済み公式1.7.10 JARを再利用: {path}")
        return
    error = None
    for url in MOJANG_URLS:
        temp_path: Path | None = None
        try:
            fd, temp_name = tempfile.mkstemp(prefix=".minecraft-1710-", suffix=".jar", dir=path.parent)
            temp_path = Path(temp_name)
            hasher = hashlib.sha1()
            total = 0
            with os.fdopen(fd, "wb") as output:
                request = Request(url, headers={"User-Agent": "IndustrialRust1710-Vanilla-Setup/1.0"})
                with urlopen(request, timeout=30) as response:
                    while True:
                        block = response.read(128 * 1024)
                        if not block:
                            break
                        total += len(block)
                        if total > SERVER_SIZE:
                            raise SetupError("公式JARのダウンロードサイズが想定を超えました。")
                        hasher.update(block)
                        output.write(block)
                output.flush()
                os.fsync(output.fileno())
            if total != SERVER_SIZE or hasher.hexdigest() != SERVER_SHA1:
                raise SetupError("Mojang公式JARの長さまたはSHA-1が一致しません。")
            # Refuse to overwrite another process's or user's existing binary.
            if path.exists():
                verify_jar(path, sha1=SERVER_SHA1, size=SERVER_SIZE)
                return
            # Atomic hard link creation refuses an existing destination.
            os.link(temp_path, path)
            verify_jar(path, sha1=SERVER_SHA1, size=SERVER_SIZE)
            print(f"Mojang公式サーバーJARを取得・SHA-1検証しました: {path}")
            return
        except (OSError, SetupError) as exc:
            error = exc
        finally:
            if temp_path is not None:
                temp_path.unlink(missing_ok=True)
    raise SetupError(f"Mojang公式JARを取得できませんでした: {error}")



def verify_log4j(path: Path, *, sha1: str = LOG4J_SHA1) -> None:
    if path.is_symlink() or not path.is_file():
        raise SetupError("Mojang公式Log4j対策XMLが存在しません。")
    if not 0 < path.stat().st_size <= LOG4J_MAX_SIZE:
        raise SetupError("Log4j対策XMLのサイズが不正です。")
    digest = hashlib.sha1(path.read_bytes()).hexdigest()
    if digest != sha1:
        raise SetupError("Mojang公式Log4j対策XMLのSHA-1が一致しません。")


def download_log4j(path: Path) -> None:
    """Fetch Mojang's patched logging config; never start a vulnerable 1.7.10 server."""
    if path.exists() or path.is_symlink():
        verify_log4j(path, sha1=LOG4J_SHA1)
        return
    last_error = None
    for url in LOG4J_URLS:
        temporary = None
        try:
            fd, name = tempfile.mkstemp(prefix=".log4j-safe-", suffix=".xml", dir=path.parent)
            temporary = Path(name)
            total = 0
            digest = hashlib.sha1()
            with os.fdopen(fd, "wb") as output:
                request = Request(url, headers={"User-Agent": "IndustrialRust1710-Vanilla-Setup/1.0"})
                with urlopen(request, timeout=30) as response:
                    while True:
                        block = response.read(8192)
                        if not block:
                            break
                        total += len(block)
                        if total > LOG4J_MAX_SIZE:
                            raise SetupError("Mojang Log4j XMLが許容サイズを超えました。")
                        digest.update(block)
                        output.write(block)
                output.flush()
                os.fsync(output.fileno())
            if total == 0 or digest.hexdigest() != LOG4J_SHA1:
                raise SetupError("MojangのLog4j XMLハッシュが期待値と一致しません。")
            if path.exists():
                verify_log4j(path, sha1=LOG4J_SHA1)
                return
            os.link(temporary, path)
            verify_log4j(path, sha1=LOG4J_SHA1)
            print(f"Mojang公式Log4j脆弱性対策を導入: {path}")
            return
        except (OSError, SetupError) as exc:
            last_error = exc
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
    raise SetupError(f"重要: Log4j対策XMLを取得できません。脆弱なサーバーは起動しません: {last_error}")


def prepare(directory: Path, bind: str, port: int, players: int) -> None:
    ipaddress.IPv4Address(bind)
    if not 1 <= port <= 65535:
        raise SetupError("port は1から65535で指定してください。")
    if not 1 <= players <= 100:
        raise SetupError("max-players は1から100で指定してください。")
    directory.mkdir(parents=True, exist_ok=True)
    props = directory / "server.properties"
    if not props.exists():
        props.write_text(properties_text(bind, port, players), encoding="ascii")
    eula = directory / "eula.txt"
    if not eula.exists():
        eula.write_text("# Read " + EULA_URL + "\neula=false\n", encoding="ascii")
    whitelist = directory / "whitelist.json"
    if not whitelist.exists():
        whitelist.write_text("[]\n", encoding="utf-8")
    check_security(directory, require_eula=False)
    download_server(directory / JAR_NAME)
    download_log4j(directory / LOG4J_NAME)
    print("設定済み: online-mode=true, white-list=true, survival, Java Edition 1.7.10")
    if bind != "127.0.0.1":
        print("注意: ループバック外で待受。OSファイアウォール等で許可端末だけに接続を制限してください。")
    print(f"次に {eula} でEULAを確認・明示同意し、runコマンドで起動します。")


def find_java(explicit: str | None) -> str:
    if explicit:
        return explicit
    home = os.environ.get("JAVA_HOME")
    if home:
        candidate = Path(home) / "bin" / ("java.exe" if os.name == "nt" else "java")
        if candidate.is_file():
            return str(candidate)
    found = shutil.which("java")
    if not found:
        raise SetupError("Java 8が見つかりません。Java 8をインストールし、--java PATHで指定してください。")
    return found


def check_java(binary: str) -> None:
    try:
        completed = subprocess.run(
            [binary, "-version"], capture_output=True, text=True, timeout=15, check=False
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise SetupError(f"Javaを起動できません: {binary}: {exc}") from exc
    version = completed.stdout + completed.stderr
    if completed.returncode != 0 or not re.search(r'version\s+"1\.8(?:[._"]|$)', version):
        raise SetupError(
            "Minecraft 1.7.10用にJava 8を指定してください。"
            f" 現在の結果: {version.strip()[:240]}"
        )


def run_server(directory: Path, java: str | None, heap: str) -> int:
    check_security(directory, require_eula=True)
    verify_jar(directory / JAR_NAME)
    verify_log4j(directory / LOG4J_NAME)
    if not re.fullmatch(r"[1-9][0-9]*[MG]", heap):
        raise SetupError("--memory は 2G や 1024M の形式で指定してください。")
    binary = find_java(java)
    check_java(binary)
    print("Minecraft 1.7.10公式バニラサーバーを起動します。")
    print("コンソールで whitelist add <プレイヤー名> を実行して参加者を許可してください。")
    print("終了時は stop を入力してください。Ctrl+Cでの強制終了は避けてください。", flush=True)
    result = subprocess.run(
        [binary, "-Xms512M", "-Xmx" + heap,
         "-Dlog4j.configurationFile=" + LOG4J_NAME,
         "-jar", JAR_NAME, "nogui"],
        cwd=directory, check=False
    )
    return result.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Mojang公式Minecraft Java 1.7.10（Modなし）の安全な導入・起動"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("prepare", "check", "run"):
        p = sub.add_parser(command)
        p.add_argument("--dir", type=Path, default=DEFAULT_DIR,
                       help="公式サーバー専用の作業ディレクトリー")
        if command == "prepare":
            p.add_argument("--bind", default="127.0.0.1", help="初期値は自分のPCからのみ")
            p.add_argument("--port", type=int, default=25565)
            p.add_argument("--max-players", type=int, default=8)
        if command == "run":
            p.add_argument("--java", default=None, help="Java 8実行ファイルへのパス")
            p.add_argument("--memory", default="2G", help="最大メモリ 2G, 4G, 1024M 等")
    args = parser.parse_args(argv)
    directory = args.dir.resolve()
    try:
        if args.command == "prepare":
            prepare(directory, args.bind, args.port, args.max_players)
            return 0
        if args.command == "check":
            check_security(directory, require_eula=True)
            verify_jar(directory / JAR_NAME)
            verify_log4j(directory / LOG4J_NAME)
            print("正常: Mojang公式JAR・Log4j対策・online-mode・ホワイトリスト・EULA設定")
            return 0
        return run_server(directory, args.java, args.memory)
    except (SetupError, ValueError) as exc:
        print("エラー: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
