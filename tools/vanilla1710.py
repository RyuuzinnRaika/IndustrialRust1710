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
from contextlib import contextmanager
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zipfile
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
    props = parse_properties(directory / "server.properties")
    with operation_lock(directory, "run"):
        if is_port_listening(props["server-ip"], int(props["server-port"])):
            raise SetupError("指定ポートは使用中です。二重起動はできません。")
        result = subprocess.run(
            [binary, "-Xms512M", "-Xmx" + heap,
             "-Dlog4j.configurationFile=" + LOG4J_NAME,
             "-jar", JAR_NAME, "nogui"],
            cwd=directory, check=False
        )
        return result.returncode



def encode_varint(value: int) -> bytes:
    result = bytearray()
    while True:
        byte = value & 127
        value >>= 7
        result.append(byte | (128 if value else 0))
        if not value:
            return bytes(result)


def read_exact(connection: socket.socket, size: int) -> bytes:
    result = bytearray()
    while len(result) < size:
        block = connection.recv(size - len(result))
        if not block:
            raise SetupError("サーバーからのステータス応答が途中で切断されました。")
        result.extend(block)
    return bytes(result)


def read_varint_socket(connection: socket.socket) -> int:
    value = 0
    for i in range(5):
        byte = read_exact(connection, 1)[0]
        if i == 4 and byte > 15:
            raise SetupError("無効なMinecraftステータスVarIntです。")
        value |= (byte & 127) << (7 * i)
        if byte & 128 == 0:
            return value
    raise SetupError("MinecraftステータスVarIntが長すぎます。")


def query_status(host: str, port: int, timeout: float = 3.0) -> dict:
    """Verify that a running server speaks Minecraft Java 1.7.10 Protocol 5."""
    if not 1 <= port <= 65535 or not 0 < timeout <= 30:
        raise SetupError("statusのポートまたはタイムアウトが不正です。")
    try:
        with socket.create_connection((host, port), timeout=timeout) as conn:
            conn.settimeout(timeout)
            address = host.encode("utf-8")
            if len(address) > 255:
                raise SetupError("サーバー名が長すぎます。")
            handshake = (
                b"\x00" + encode_varint(5) + encode_varint(len(address))
                + address + struct.pack(">H", port) + b"\x01"
            )
            conn.sendall(encode_varint(len(handshake)) + handshake + b"\x01\x00")
            size = read_varint_socket(conn)
            if not 2 <= size <= 65536:
                raise SetupError("Minecraftステータス応答の長さが不正です。")
            first = read_exact(conn, size)
            if first[0] != 0:
                raise SetupError("Minecraft Status Responseではないパケットを受信しました。")
            offset = 1
            length = 0
            for i in range(5):
                if offset >= len(first):
                    raise SetupError("Status JSON長が不足しています。")
                byte = first[offset]
                offset += 1
                length |= (byte & 127) << (7 * i)
                if byte & 128 == 0:
                    break
            if length != len(first) - offset or length > 65536:
                raise SetupError("Status JSON長が一致しません。")
            status = json.loads(first[offset:].decode("utf-8"))
            if not isinstance(status, dict) or status.get("version", {}).get("protocol") != 5:
                raise SetupError("Minecraft Java 1.7.10 (Protocol 5) のサーバーではありません。")
            return status
    except (OSError, ValueError, UnicodeError, json.JSONDecodeError) as exc:
        raise SetupError(f"サーバー接続・ステータス確認に失敗しました: {exc}") from exc


@contextmanager
def operation_lock(directory: Path, purpose: str):
    """Stop simultaneous launcher-run/backup. A stale lock requires manual review."""
    lock = directory / ".server-operation.lock"
    try:
        handle = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError as exc:
        raise SetupError(
            f"{lock} が存在します。サーバー起動中か別の操作中です。"
            "クラッシュ後の場合はプロセスが停止したと確認してからロックを処理してください。"
        ) from exc
    try:
        with os.fdopen(handle, "w", encoding="ascii") as output:
            output.write(f"pid={os.getpid()} purpose={purpose} at={int(time.time())}\n")
        yield
    finally:
        lock.unlink(missing_ok=True)


def is_port_listening(bind: str, port: int) -> bool:
    host = "127.0.0.1" if bind == "0.0.0.0" else bind
    try:
        with socket.create_connection((host, port), timeout=0.5):
            return True
    except (OSError, TimeoutError):
        return False


def backup_server(directory: Path, output: Path) -> int:
    """Create-only ZIP after checking the server is offline.

    Backups include world and essential user configuration but never copyrighted
    Mojang server JARs, private XML mitigation or other generated binaries.
    """
    directory = directory.resolve()
    output = output.resolve()
    if output == directory or directory in output.parents:
        raise SetupError("バックアップをサーバーディレクトリー内に保存できません。")
    if output.exists() or output.is_symlink():
        raise SetupError("バックアップ先は存在しない新規ZIPファイルを指定してください。")
    if not output.parent.is_dir():
        raise SetupError("バックアップ保存先の親ディレクトリーがありません。")
    props = parse_properties(directory / "server.properties")
    check_security(directory, require_eula=False)
    bind = props["server-ip"]
    port = int(props["server-port"])
    world_name = props.get("level-name", "world")
    if (
        not world_name or world_name in (".", "..")
        or Path(world_name).name != world_name or "\\" in world_name
    ):
        raise SetupError("バックアップ対象のlevel-nameが不正です。")
    world = directory / world_name
    if world.is_symlink() or not world.is_dir():
        raise SetupError(f"ワールドが見つかりません: {world}")
    with operation_lock(directory, "backup"):
        if is_port_listening(bind, port):
            raise SetupError("サーバーがTCPポートで稼働中です。コンソールで stop を実行してください。")
        files = []
        for path in world.rglob("*"):
            if path.is_symlink():
                raise SetupError(f"ワールドにシンボリックリンクがあります: {path}")
            if path.is_file():
                files.append(path)
        if not files:
            raise SetupError("ワールドファイルがありません。初回起動・プレイ後にバックアップしてください。")
        for name in ("server.properties", "whitelist.json", "ops.json", "banned-players.json",
                     "banned-ips.json", "usercache.json", "eula.txt"):
            path = directory / name
            if path.is_symlink():
                raise SetupError(f"設定にシンボリックリンクがあります: {path}")
            if path.is_file():
                files.append(path)
        staged = None
        try:
            fd, temp_name = tempfile.mkstemp(
                prefix=".vanilla-backup-", suffix=".zip", dir=output.parent
            )
            staged = Path(temp_name)
            with os.fdopen(fd, "wb") as stream:
                with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED, allowZip64=True) as archive:
                    for path in sorted(files):
                        archive.write(path, path.relative_to(directory).as_posix())
                stream.flush()
                os.fsync(stream.fileno())
            # If the server came online mid-copy, refuse to publish a possibly
            # inconsistent backup. The operation lock blocks our launcher.
            if is_port_listening(bind, port):
                raise SetupError("バックアップ中にサーバーが起動しました。保存を中止します。")
            os.link(staged, output)
        finally:
            if staged is not None:
                staged.unlink(missing_ok=True)
    print(f"バックアップ作成完了: {output} ({len(files)} ファイル)")
    print("Minecraft JAR/Log4j XMLは含まれません。必要ならprepareで公式配布元から再取得してください。")
    return len(files)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Mojang公式Minecraft Java 1.7.10（Modなし）の安全な導入・起動"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("prepare", "check", "run", "status", "backup"):
        p = sub.add_parser(command)
        p.add_argument("--dir", type=Path, default=DEFAULT_DIR,
                       help="公式サーバー専用の作業ディレクトリー")
        if command == "status":
            p.add_argument("--host", default="127.0.0.1")
            p.add_argument("--port", type=int, default=25565)
        if command == "backup":
            p.add_argument("--output", type=Path, required=True,
                           help="既存ファイルを上書きしないZIPバックアップの保存先")
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
        if args.command == "status":
            result = query_status(args.host, args.port)
            print("正常: Minecraft Java 1.7.10 (Protocol 5) 応答を確認")
            print(f"接続者: {result.get('players', {}).get('online', '?')}/{result.get('players', {}).get('max', '?')}")
            return 0
        if args.command == "backup":
            backup_server(directory, args.output)
            return 0
        return run_server(directory, args.java, args.memory)
    except (SetupError, ValueError) as exc:
        print("エラー: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
