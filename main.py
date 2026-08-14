import argparse
import multiprocessing
import os
from collections.abc import Callable

import flet as ft
from dotenv import load_dotenv
from screeninfo import get_monitors

from app.app_manager import App, execute_dir
from app.auth.auth_manager import AuthManager
from app.core.runtime.backend_services import BackendServices
from app.core.runtime.bundled_env import patch_macos_flet_launcher, setup_bundled_flet_view
from app.core.runtime.paths import prepend_user_bin_dirs, resource_dir
from app.lifecycle.app_close_handler import handle_app_close
from app.lifecycle.tray_manager import TrayManager
from app.ui.components.common.save_progress_overlay import SaveProgressOverlay
from app.ui.layout.responsive_layout import setup_responsive_layout
from app.ui.views.login_view import LoginPage
from app.utils.logger import logger

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 6006
MACOS_WINDOW_SCALE = 0.56
WINDOWS_WINDOW_SCALE = 0.68
MIN_WIDTH = 950
MIN_HEIGHT = 620
ASSETS_DIR = "assets"
# 窗口最多占逻辑屏幕的比例，避免 DPI 探测失败时窗口撑出屏幕
MAX_SCREEN_RATIO = 0.94


def get_desktop_window_scale(page: ft.Page) -> float:
    is_macos = page.platform is not None and page.platform.value == "macos"
    return MACOS_WINDOW_SCALE if is_macos else WINDOWS_WINDOW_SCALE


def get_display_scale_factor() -> float:
    """返回主显示器的缩放系数。

    screeninfo 报告的是物理像素，而 flet 的 window.width/height 使用逻辑像素。
    高 DPI 屏上若不换算，窗口会超出屏幕表现为全屏无边框。
    非 Windows 或查询失败时返回 1.0（macOS 的 screeninfo 已是逻辑点）。
    """
    if os.name != "nt":
        return 1.0

    import ctypes

    class _Point(ctypes.Structure):
        _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]

    # GetScaleFactorForMonitor 不受调用进程 DPI 感知级别影响，优先使用
    try:
        monitor_default_to_primary = 1
        monitor = ctypes.windll.user32.MonitorFromPoint(_Point(0, 0), monitor_default_to_primary)
        factor = ctypes.c_int()
        if ctypes.windll.shcore.GetScaleFactorForMonitor(monitor, ctypes.byref(factor)) == 0:
            if factor.value > 0:
                return factor.value / 100.0
    except Exception:
        pass

    try:
        dpi = ctypes.windll.user32.GetDpiForSystem()
        if dpi > 0:
            return dpi / 96.0
    except Exception:
        pass

    return 1.0


def get_logical_screen_size() -> tuple[int, int] | None:
    """取系统报告的屏幕尺寸，用于校验换算结果。

    DPI 不感知的进程里 GetSystemMetrics 返回的已是虚拟化后的逻辑尺寸，
    可作为独立于缩放探测的上界，兜住探测失败的情形。
    """
    if os.name != "nt":
        return None
    try:
        import ctypes

        user32 = ctypes.windll.user32
        width = user32.GetSystemMetrics(0)
        height = user32.GetSystemMetrics(1)
        if width > 0 and height > 0:
            return width, height
    except Exception:
        pass
    return None


def calc_window_size(screen_width: int, screen_height: int, window_scale: float) -> tuple[int, int]:
    """由物理屏幕尺寸算出合适的逻辑窗口尺寸。"""
    scale_factor = get_display_scale_factor() or 1.0
    logical_width = screen_width / scale_factor
    logical_height = screen_height / scale_factor

    # 取两个来源的较小值作为逻辑屏上界，任一来源失准都不会导致窗口撑出屏幕
    system_size = get_logical_screen_size()
    if system_size:
        logical_width = min(logical_width, system_size[0])
        logical_height = min(logical_height, system_size[1])

    width = min(logical_width * window_scale, logical_width * MAX_SCREEN_RATIO)
    height = min(logical_height * window_scale, logical_height * MAX_SCREEN_RATIO)

    return int(width), int(height)


async def setup_desktop_window(page: ft.Page, app: App) -> None:
    if page.web:
        return

    page.window.icon = os.path.join(resource_dir, ASSETS_DIR, "icon.ico")
    page.window.skip_task_bar = False
    page.window.always_on_top = False
    try:
        window_scale = get_desktop_window_scale(page)
        page.window.min_width = MIN_WIDTH
        page.window.min_height = max(MIN_HEIGHT, MIN_WIDTH * window_scale)

        saved_width = app.settings.user_config.get("window_width")
        saved_height = app.settings.user_config.get("window_height")
        if app.settings.user_config.get("remember_window_size") and saved_width and saved_height:
            page.window.width = int(saved_width)
            page.window.height = int(saved_height)
        else:
            screen = get_monitors()[0]
            page.window.width, page.window.height = calc_window_size(
                screen.width, screen.height, window_scale
            )

        page.update()
        await page.window.center()
        await page.window.to_front()
        page.window.visible = True
        page.update()
    except IndexError:
        logger.warning("No monitors detected, using default window size.")


def get_route_handler() -> dict[str, str]:
    return {
        "/": "home",
        "/home": "home",
        "/recordings": "recordings",
        "/settings": "settings",
        "/storage": "storage",
        "/about": "about",
    }


def handle_route_change(page: ft.Page, app: App) -> Callable:
    route_map = get_route_handler()

    def route_change(e: ft.RouteChangeEvent) -> None:
        tr = ft.TemplateRoute(e.route)
        page_name = route_map.get(tr.route)
        if page_name:
            page.run_task(app.switch_page, page_name)

    return route_change


def handle_window_event(page: ft.Page, app: App, save_progress_overlay: "SaveProgressOverlay") -> Callable:
    async def on_window_event(e) -> None:
        if e.type == ft.WindowEventType.CLOSE:
            if app.settings.user_config.get("remember_window_size"):
                app.settings.user_config["window_width"] = page.window.width
                app.settings.user_config["window_height"] = page.window.height
                await app.config_manager.save_user_config(app.settings.user_config)
            await handle_app_close(page, app, save_progress_overlay)

    return on_window_event


def handle_disconnect(page: ft.Page, app: App) -> Callable:
    """Handle disconnection for web mode."""

    async def disconnect(_: ft.ControlEvent) -> None:
        page.pubsub.unsubscribe_all()
        app.settings.user_config["last_route"] = page.route
        await app.config_manager.save_user_config(app.settings.user_config)
        logger.info(f"Saved last route: {page.route}")

        if app.services is not None:
            app.services.unregister_ui_bridge(app)

    return disconnect


def handle_page_resize(page: ft.Page, app: App) -> Callable:
    """handle page resize"""

    def on_resize(_: ft.ControlEvent) -> None:
        setup_responsive_layout(page, app)
        page.update()

    return on_resize


async def main(page: ft.Page) -> None:
    page.title = "StreamCap"
    # 尽早隐藏窗口，避免 Flet 初始化阶段闪出默认的小窗口；
    # setup_desktop_window 设好尺寸后会再设为可见
    if not page.web:
        page.window.visible = False

    _services = BackendServices.get()
    app = App(page, services=_services)
    page.data = app
    app.is_web_mode = page.web
    app.is_mobile = False
    await setup_desktop_window(page, app)

    if not page.web:
        try:
            app.tray_manager = TrayManager(app)
            logger.info("Tray manager initialized successfully")
        except Exception as e:
            logger.error(f"Failed to initialize tray manager: {e}")

    theme_mode = app.settings.user_config.get("theme_mode", "light")
    if theme_mode == "dark":
        page.theme_mode = ft.ThemeMode.DARK
    else:
        page.theme_mode = ft.ThemeMode.LIGHT

    save_progress_overlay = SaveProgressOverlay(app)
    page.overlay.append(save_progress_overlay.overlay)

    async def load_app():
        if page.web:
            setup_responsive_layout(page, app)
            page.on_resize = handle_page_resize(page, app)
            page.on_disconnect = handle_disconnect(page, app)

        page.add(app.complete_page)

        page.on_route_change = handle_route_change(page, app)
        page.window.prevent_close = True
        page.window.on_event = handle_window_event(page, app, save_progress_overlay)
        if page.web:
            rm = _services.recording_manager
            if rm is not None and not rm.is_periodic_task_running():
                logger.info("Starting periodic tasks for the first time in web mode (via session)")
                page.run_task(app.start_periodic_tasks)
            else:
                logger.info("Periodic tasks already running (BackendServices), skipping")
        else:
            logger.info("Starting periodic tasks in desktop mode")
            page.run_task(app.start_periodic_tasks)

            if page.platform and page.platform.value == "windows":
                if app.tray_manager is not None:
                    try:
                        app.tray_manager.start(page, save_progress_overlay)
                    except Exception as err:
                        logger.error(f"Failed to start tray manager: {err}")

        page.update()

        if page.route in ["/", ""]:
            last_route = app.settings.user_config.get("last_route", "/home")
            logger.info(f"Restored last route: {last_route}")
            await page.push_route(last_route)

        if page.web:
            page.run_task(app.switch_page, page.route[1:])

    if page.web:
        auth_manager = AuthManager(app)
        app.auth_manager = auth_manager
        await auth_manager.initialize()

        login_required = app.settings.get_config_value("login_required", False)

        if login_required:
            session_token = await page.shared_preferences.get("session_token")
            if not isinstance(session_token, str) or not auth_manager.validate_session(session_token):

                async def on_login_success(token: str):
                    _session_info = auth_manager.active_sessions.get(token, {})
                    app.current_username = _session_info.get("username")

                    page.controls.clear()
                    await load_app()

                page.controls.clear()

                login_page = LoginPage(page, auth_manager, on_login_success)
                page.add(login_page.get_view())
                return
            else:
                session_info = auth_manager.active_sessions.get(session_token, {})
                app.current_username = session_info.get("username")
        else:
            app.current_username = "admin"

    await load_app()


if __name__ == "__main__":
    multiprocessing.freeze_support()

    load_dotenv()
    platform = os.getenv("PLATFORM")
    default_host = os.getenv("HOST", DEFAULT_HOST)
    default_port = int(os.getenv("PORT", DEFAULT_PORT))
    assets_dir = os.path.join(resource_dir, ASSETS_DIR)
    prepend_user_bin_dirs()

    parser = argparse.ArgumentParser(description="Run the Flet app with optional web mode.")
    parser.add_argument("--web", action="store_true", help="Run the app in web mode")
    parser.add_argument("--host", type=str, default=default_host, help=f"Host address (default: {default_host})")
    parser.add_argument("--port", type=int, default=default_port, help=f"Port number (default: {default_port})")
    args = parser.parse_args()

    services = BackendServices.bootstrap(execute_dir)

    is_web = args.web or platform == "web"
    if is_web:
        services.start_background_loop()
        logger.debug("Running in web mode on http://" + args.host + ":" + str(args.port))
        ft.run(
            main=main,
            view=ft.AppView.WEB_BROWSER,
            host=args.host,
            port=args.port,
            assets_dir=assets_dir,
            web_renderer=ft.WebRenderer.CANVAS_KIT,
            no_cdn=True,
        )
    else:
        setup_bundled_flet_view()
        patch_macos_flet_launcher()
        ft.run(main=main, view=ft.AppView.FLET_APP_HIDDEN, assets_dir=assets_dir)
