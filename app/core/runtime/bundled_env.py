import os
import platform
import shutil
import sys
from pathlib import Path


def patch_macos_flet_launcher() -> None:
    """Launch Flet.app directly in macOS bundles to avoid a Dock icon for /usr/bin/open."""
    if not (getattr(sys, "frozen", False) and platform.system() == "Darwin"):
        return

    import asyncio
    import plistlib
    import subprocess
    import tempfile

    import flet_desktop
    import flet_desktop.version
    from flet.utils import random_string

    from app.core.runtime.paths import resource_dir, user_data_dir

    def find_app_in_dir(client_dir: Path, excluded_path: Path | None = None) -> Path | None:
        if not client_dir.is_dir():
            return None

        candidates = [
            candidate for candidate in client_dir.iterdir() if candidate.suffix == ".app" and candidate.is_dir()
        ]
        for candidate in candidates:
            if excluded_path is None or candidate.resolve() != excluded_path.resolve():
                return candidate
        return None

    def find_cached_flet_app(excluded_path: Path) -> Path:
        flet_view_path = os.environ.get("FLET_VIEW_PATH")
        if flet_view_path:
            source_app = find_app_in_dir(Path(flet_view_path), excluded_path)
            if source_app:
                return source_app

        client_dir = flet_desktop.ensure_client_cached()
        source_app = find_app_in_dir(client_dir, excluded_path)
        if source_app:
            return source_app
        raise FileNotFoundError(f"Application bundle not found in {client_dir}")

    def has_materialized_framework_links(app_path: Path) -> bool:
        framework_root = app_path / "Contents" / "Frameworks"
        if not framework_root.is_dir():
            return False

        for framework in framework_root.glob("*.framework"):
            top_level_binary = framework / framework.stem
            current_version = framework / "Versions" / "Current"
            if top_level_binary.exists() and not top_level_binary.is_symlink():
                return True
            if current_version.exists() and not current_version.is_symlink():
                return True
        return False

    def write_file_picker_entitlements(target_dir: Path) -> Path:
        entitlements_path = target_dir / "StreamCapFlet.entitlements"
        entitlements = {
            "com.apple.security.files.user-selected.read-write": True,
        }
        with entitlements_path.open("wb") as file:
            plistlib.dump(entitlements, file)
        return entitlements_path

    def prepare_streamcap_flet_app() -> Path:
        target_app_name = "StreamCap Flet.app"
        target_app = Path(user_data_dir) / "flet_client" / flet_desktop.version.version / target_app_name
        source_app = find_cached_flet_app(target_app)
        target_icon = target_app / "Contents" / "Resources" / "AppIcon.icns"
        source_icon = Path(resource_dir) / "assets" / "icons" / "Appicon.icns"

        if target_app.exists() and has_materialized_framework_links(target_app):
            shutil.rmtree(target_app)

        if not target_app.exists():
            target_app.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(source_app, target_app, symlinks=True)

        if source_icon.is_file():
            shutil.copy2(source_icon, target_icon)

        info_plist = target_app / "Contents" / "Info.plist"
        with info_plist.open("rb") as file:
            info = plistlib.load(file)
        info["CFBundleName"] = "StreamCap"
        info["CFBundleDisplayName"] = "StreamCap"
        info["CFBundleIdentifier"] = "io.github.ihmily.streamcap.flet"
        info["CFBundleIconFile"] = "AppIcon"
        info.pop("CFBundleIconName", None)
        with info_plist.open("wb") as file:
            plistlib.dump(info, file)

        codesign = shutil.which("codesign")
        if codesign:
            entitlements_path = write_file_picker_entitlements(target_app.parent)
            subprocess.run(
                [
                    codesign,
                    "--force",
                    "--deep",
                    "--sign",
                    "-",
                    "--entitlements",
                    str(entitlements_path),
                    str(target_app),
                ],
                check=False,
            )

        return target_app

    def build_launch_args(page_url, assets_dir, hidden):
        pid_file = str(Path(tempfile.gettempdir()).joinpath(random_string(20)))
        app_path = prepare_streamcap_flet_app()
        info_plist = app_path / "Contents" / "Info.plist"
        with info_plist.open("rb") as file:
            info = plistlib.load(file)
        executable_name = info.get("CFBundleExecutable", app_path.stem)
        executable = app_path / "Contents" / "MacOS" / executable_name

        args = [str(executable), page_url, pid_file]
        if assets_dir:
            args.append(assets_dir)

        env = {**os.environ}
        if hidden:
            env["FLET_HIDE_WINDOW_ON_START"] = "true"
        return args, env, pid_file

    def open_flet_view(page_url, assets_dir, hidden):
        args, env, pid_file = build_launch_args(page_url, assets_dir, hidden)
        return subprocess.Popen(args, env=env), pid_file

    async def open_flet_view_async(page_url, assets_dir, hidden):
        args, env, pid_file = build_launch_args(page_url, assets_dir, hidden)
        return await asyncio.create_subprocess_exec(args[0], *args[1:], env=env), pid_file

    flet_desktop.open_flet_view = open_flet_view
    flet_desktop.open_flet_view_async = open_flet_view_async


def setup_bundled_flet_view() -> None:
    """Configure the bundled Flet view path for PyInstaller desktop packages."""
    if not getattr(sys, "frozen", False):
        return

    if platform.system() == "Darwin":
        # Let flet_desktop use its official macOS flow: locate bundled
        # flet-macos.tar.gz in the package app directory, extract it to the
        # Flet client cache, then launch the discovered .app bundle.
        return

    if hasattr(sys, "_MEIPASS"):
        # noinspection PyProtectedMember
        base = Path(vars(sys)["_MEIPASS"])
    else:
        base = Path(sys.executable).parent / "_internal"

    view_path = base / "flet_desktop" / "app" / "flet"
    if (view_path / "flet.exe").is_file():
        os.environ["FLET_VIEW_PATH"] = str(view_path)
