import os
import shutil
import sys
from pathlib import Path

APP_NAME = "StreamCap"
EXECUTABLE_SUFFIX = ".exe" if sys.platform == "win32" else ""


def _executable_dir() -> Path:
    executable = sys.executable if getattr(sys, "frozen", False) else sys.argv[0]
    return Path(executable).resolve().parent


def _macos_bundle_contents_dir(executable_dir: Path) -> Path | None:
    contents_dir = executable_dir.parent
    app_dir = contents_dir.parent
    if (
        getattr(sys, "frozen", False)
        and sys.platform == "darwin"
        and executable_dir.name == "MacOS"
        and contents_dir.name == "Contents"
        and app_dir.suffix == ".app"
    ):
        return contents_dir
    return None


_EXECUTABLE_DIR = _executable_dir()
_CONTENTS_DIR = _macos_bundle_contents_dir(_EXECUTABLE_DIR)

if _CONTENTS_DIR is not None:
    resource_dir = _CONTENTS_DIR / "Resources"
    user_data_dir = Path.home() / "Library" / "Application Support" / APP_NAME
elif getattr(sys, "frozen", False) and sys.platform == "win32":
    internal_dir = _EXECUTABLE_DIR / "_internal"
    resource_dir = internal_dir if internal_dir.is_dir() else _EXECUTABLE_DIR
    user_data_dir = Path(os.environ.get("APPDATA", Path.home() / "AppData" / "Roaming")) / APP_NAME
else:
    resource_dir = _EXECUTABLE_DIR
    user_data_dir = _EXECUTABLE_DIR

if getattr(sys, "frozen", False) and sys.platform == "win32":
    default_recordings_dir = _EXECUTABLE_DIR / "downloads"
else:
    default_recordings_dir = user_data_dir / "downloads"


# 用户数据文件：由程序在运行中写入，任何情况下都不允许被包内同名文件覆盖。
# 其余文件（default_settings.json / language.json / version.json 等）随版本
# 分发，需要覆盖更新，才能让新增的默认配置项生效。
USER_OWNED_FILES = frozenset(
    {
        "recordings.json",
        "cookies.json",
        "accounts.json",
        "user_settings.json",
        "web_auth.json",
    }
)


def prepare_user_data_dir() -> None:
    """Copy bundled defaults to the writable user data directory when needed."""
    user_data_dir.mkdir(parents=True, exist_ok=True)
    if user_data_dir == resource_dir:
        return

    for directory in ("config", "locales"):
        source = resource_dir / directory
        target = user_data_dir / directory
        if source.is_dir():
            shutil.copytree(source, target, dirs_exist_ok=True, ignore=_ignore_user_owned)

    prepare_bundled_ffmpeg()
    prepare_bundled_node()


def _ignore_user_owned(src: str, names: list[str]) -> set[str]:
    """跳过已存在于用户数据目录的用户数据文件，避免覆盖真实配置。

    打包时若不慎把源码目录里运行产生的 recordings.json 等一并打进包
    （从源码跑过一次就会生成），这里是最后一道防线。
    """
    source_dir = Path(src)
    relative = source_dir.relative_to(resource_dir)
    target_dir = user_data_dir / relative
    return {name for name in names if name in USER_OWNED_FILES and (target_dir / name).exists()}


def prepare_bundled_ffmpeg() -> None:
    source_executable = resource_dir / "ffmpeg" / f"ffmpeg{EXECUTABLE_SUFFIX}"
    target_dir = user_data_dir / "ffmpeg"
    target_executable = target_dir / f"ffmpeg{EXECUTABLE_SUFFIX}"
    if not source_executable.is_file() or target_executable.exists():
        return
    if shutil.which("ffmpeg"):
        return

    target_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source_executable, target_executable)
    if sys.platform != "win32":
        target_executable.chmod(target_executable.stat().st_mode | 0o755)


def prepare_bundled_node() -> None:
    source_executable = resource_dir / "node" / f"node{EXECUTABLE_SUFFIX}"
    target_dir = user_data_dir / "node"
    target_executable = target_dir / f"node{EXECUTABLE_SUFFIX}"
    if not source_executable.is_file() or target_executable.exists():
        return
    if shutil.which("node"):
        return

    target_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source_executable, target_executable)
    if sys.platform != "win32":
        target_executable.chmod(target_executable.stat().st_mode | 0o755)


def prepend_user_bin_dirs() -> None:
    for directory in (user_data_dir / "ffmpeg", user_data_dir / "node"):
        if directory.is_dir():
            os.environ["PATH"] = str(directory) + os.pathsep + os.environ.get("PATH", "")
