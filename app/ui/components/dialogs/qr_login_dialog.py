import asyncio
import time

import flet as ft

from ....core.platforms.kuaishou_qr_login import KuaishouQrLogin, QrExpiredError, QrLoginError
from ....utils.logger import logger


class KuaishouQrLoginDialog(ft.AlertDialog):
    """快手扫码登录弹窗。二维码 60 秒失效，过期后自动换新码。"""

    def __init__(self, app, on_success):
        self.app = app
        self.on_success = on_success
        self.app.language_manager.add_observer(self)
        self._ = {}
        self.load()

        self._task: asyncio.Task | None = None
        self._closed = False

        self.qr_image = ft.Image(src="", width=260, height=260, fit=ft.BoxFit.CONTAIN)
        self.qr_placeholder = ft.Container(
            content=ft.ProgressRing(width=32, height=32),
            width=260,
            height=260,
            alignment=ft.Alignment.CENTER,
        )
        self.qr_area = ft.Container(
            content=self.qr_placeholder,
            width=260,
            height=260,
            bgcolor=ft.Colors.WHITE,
            border_radius=8,
            padding=6,
        )
        self.status_text = ft.Text(self._["qr_loading"], size=13, text_align=ft.TextAlign.CENTER)
        self.refresh_button = ft.TextButton(self._["qr_refresh"], on_click=self.restart, visible=False)

        super().__init__(
            modal=True,
            title=ft.Text(self._["kuaishou_qr_login"], size=18, weight=ft.FontWeight.BOLD),
            content=ft.Container(
                content=ft.Column(
                    controls=[
                        ft.Text(self._["qr_tip"], size=12, color=ft.Colors.GREY, text_align=ft.TextAlign.CENTER),
                        self.qr_area,
                        self.status_text,
                        self.refresh_button,
                    ],
                    spacing=10,
                    tight=True,
                    horizontal_alignment=ft.CrossAxisAlignment.CENTER,
                ),
                width=320,
                padding=ft.Padding.symmetric(horizontal=10, vertical=6),
            ),
            actions=[ft.TextButton(self._["cancel"], on_click=self.close_dialog)],
            actions_alignment=ft.MainAxisAlignment.END,
            shape=ft.RoundedRectangleBorder(radius=10),
        )

    def load(self):
        language = self.app.language_manager.language
        for key in ("qr_login_dialog", "base"):
            self._.update(language.get(key, {}))

    def start(self):
        self._closed = False
        self._task = self.app.page.run_task(self._run)

    async def close_dialog(self, _=None):
        self._closed = True
        if self._task is not None and not self._task.done():
            self._task.cancel()
        self.open = False
        self.app.page.update()

    async def restart(self, _=None):
        if self._task is not None and not self._task.done():
            self._task.cancel()
        self.refresh_button.visible = False
        self._task = self.app.page.run_task(self._run)

    def _set_status(self, text: str, color: str | None = None):
        if self._closed:
            return
        self.status_text.value = text
        self.status_text.color = color
        self.app.page.update()

    async def _run(self):
        proxy = None
        user_config = self.app.settings.user_config
        if user_config.get("enable_proxy") and "kuaishou" in str(user_config.get("default_platform_with_proxy", "")):
            proxy = user_config.get("proxy_address") or None

        try:
            async with KuaishouQrLogin(proxy=proxy) as session:
                while not self._closed:
                    try:
                        await self._one_round(session)
                        return
                    except QrExpiredError:
                        # 60 秒有效期太短，过期直接换新码，不打断用户
                        if self._closed:
                            return
                        self._set_status(self._["qr_expired_retry"], ft.Colors.ORANGE)
                        await asyncio.sleep(0.8)
        except asyncio.CancelledError:
            raise
        except QrLoginError as e:
            self._fail(str(e))
        except Exception as e:
            logger.error(f"Kuaishou QR login failed: {e}")
            self._fail(f"{self._['qr_failed']}: {e}")

    async def _one_round(self, session: KuaishouQrLogin):
        self.qr_area.content = self.qr_placeholder
        self._set_status(self._["qr_loading"])

        qr = await session.create_qr()
        if self._closed:
            return
        self.qr_image.src = qr.image_base64
        self.qr_area.content = self.qr_image
        deadline = time.time() + qr.seconds_left
        self._set_status(self._["qr_waiting_scan"].replace("{seconds}", str(qr.seconds_left)))

        countdown = self.app.page.run_task(self._countdown, deadline)
        try:
            await session.wait_for_scan(qr)
        finally:
            countdown.cancel()
        if self._closed:
            return

        self._set_status(self._["qr_waiting_confirm"], ft.Colors.BLUE)
        qr_token = await session.wait_for_accept(qr)
        if self._closed:
            return

        self._set_status(self._["qr_exchanging"], ft.Colors.BLUE)
        cookies = await session.exchange_cookies(qr_token)
        user_name = await session.verify_cookies(cookies)
        if self._closed:
            return

        if user_name:
            self._set_status(self._["qr_success"].replace("{name}", user_name), ft.Colors.GREEN)
        else:
            # Cookie 已拿到但校验没通过，仍然保存，由用户自行判断是否可用
            self._set_status(self._["qr_success_unverified"], ft.Colors.ORANGE)

        await self.on_success(cookies)
        await asyncio.sleep(1.5)
        await self.close_dialog()

    async def _countdown(self, deadline: float):
        try:
            while not self._closed:
                left = int(deadline - time.time())
                if left <= 0:
                    return
                self._set_status(self._["qr_waiting_scan"].replace("{seconds}", str(left)))
                await asyncio.sleep(1)
        except asyncio.CancelledError:
            pass

    def _fail(self, message: str):
        if self._closed:
            return
        self.status_text.value = message
        self.status_text.color = ft.Colors.RED
        self.refresh_button.visible = True
        self.app.page.update()
