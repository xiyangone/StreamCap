import os
import threading

import flet as ft

from ..utils.logger import logger


class TrayManager:
    def __init__(self, app):
        self.app = app
        self.icon = None
        self.icon_path = None
        self.tray_thread = None
        self.is_running = False
        self.save_progress_overlay = None
        self.assets_dir = getattr(app, "assets_dir", os.path.join(os.getcwd(), "assets"))

    def create_image(self):
        try:
            from PIL import Image

            self.icon_path = os.path.join(self.assets_dir, "icons", "tray_icon.ico")
            if os.path.exists(self.icon_path):
                return Image.open(self.icon_path)
            logger.warning(f"Tray icon file not found: {self.icon_path}, using fallback image")
            return Image.new("RGB", (32, 32), color=(255, 255, 255))
        except Exception as e:
            logger.error(f"Failed to load icon file: {e}")
            try:
                from PIL import Image

                image = Image.new("RGB", (32, 32), color=(255, 255, 255))
                return image
            except Exception as e:
                logger.error("PIL not available, unable to create tray icon")
                raise e

    def create_tray_icon(self, page: ft.Page):
        if self.is_running:
            return

        try:
            import pystray

            def on_restore(_icon, _item):
                page.window.visible = True
                page.window.minimized = False
                page.window.focused = True
                page.run_task(page.window.to_front)
                page.update()

            def on_exit(_icon, _item):
                # pystray 回调运行在托盘线程，必须经 page.run_task 切回
                # Flet 事件循环；退出统一走 handle_app_close 的确认弹窗流程
                from .app_close_handler import handle_app_close

                on_restore(_icon, _item)
                page.run_task(handle_app_close, page, self.app, self.save_progress_overlay)

            language = self.app.language_manager.language
            _ = {}
            for key in ("tray_manager", "base"):
                _.update(language.get(key, {}))

            menu = pystray.Menu(pystray.MenuItem(_["restore"], on_restore), pystray.MenuItem(_["exit"], on_exit))

            self.icon = pystray.Icon("StreamCap", self.create_image(), "StreamCap", menu)
            self.is_running = True
            self.icon.run()
        except ImportError as e:
            logger.error(e)
            self.is_running = False
            page.run_task(page.window.destroy)
            raise e
        except Exception as e:
            logger.error(f"Tray icon failed to start: {e}")
            self.is_running = False

    def start(self, page: ft.Page, save_progress_overlay=None):
        if getattr(self.app, "is_web_mode", False):
            logger.info("Tray icon not available in web mode")
            return False

        self.save_progress_overlay = save_progress_overlay
        if self.tray_thread is None or not self.tray_thread.is_alive():
            self.tray_thread = threading.Thread(target=self.create_tray_icon, args=(page,), daemon=True)
            self.tray_thread.start()
            return True
        return False

    def stop(self):
        if self.icon and self.is_running:
            self.is_running = False
            try:
                self.icon.stop()
                return True
            except Exception as e:
                logger.error(f"Error stopping tray icon: {e}")
        return False
