import asyncio

import flet as ft

from ..utils.logger import logger
from .tray_manager import TrayManager


async def _safe_destroy_window(page):
    try:
        await page.window.destroy()
    except Exception as ex:
        logger.error(f"close window error: {ex}")


async def _stop_active_recordings(app, timeout_seconds: float = 25.0) -> None:
    record_manager = getattr(app, "record_manager", None)
    if record_manager is None:
        return

    for recording in list(record_manager.recordings):
        if recording.is_recording:
            try:
                record_manager.stop_recording(recording, manually_stopped=True)
            except Exception as exc:
                logger.error(f"Failed to request recording stop for {recording.rec_id}: {exc}")

    for recorder in list(record_manager.active_recorders.values()):
        try:
            recorder.request_stop()
        except Exception as exc:
            logger.error(f"Failed to request recorder shutdown: {exc}")

    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout_seconds
    while record_manager.active_recorders and loop.time() < deadline:
        await asyncio.sleep(0.2)

    if record_manager.active_recorders:
        logger.warning(
            f"Timed out waiting for active recorders to stop: {list(record_manager.active_recorders.keys())}"
        )


async def handle_app_close(page: ft.Page, app, save_progress_overlay) -> None:
    _ = {}
    language = app.language_manager.language
    for key in ("app_close_handler", "base"):
        _.update(language.get(key, {}))

    if not getattr(app, "is_web_mode", False) and getattr(app, "tray_manager", None) is None:
        app.tray_manager = TrayManager(app)

    async def minimize_to_tray(e):
        page.window.visible = False
        page.update()
        await close_dialog(e)

    async def close_dialog_dismissed(e):
        app.recording_enabled = False

        app.settings.user_config["last_route"] = page.route
        try:
            await app.config_manager.save_user_config(app.settings.user_config)
            logger.info(f"Saved last route: {page.route}")
        except Exception as exc:
            logger.error(f"Failed to save last route during shutdown: {exc}")

        active_recorder_count = len(getattr(app.record_manager, "active_recorders", {}))
        active_process_count = len([p for p in app.process_manager.ffmpeg_processes if p.returncode is None])
        active_recordings_count = max(active_recorder_count, active_process_count)

        await close_dialog(e)
        if active_recordings_count:
            save_progress_overlay.show(
                _["saving_recordings"].format(active_recordings_count=active_recordings_count), cancellable=True
            )
            page.update()

        try:
            await _stop_active_recordings(app)
            await app.cleanup()
        except Exception as ex:
            logger.error(f"Application cleanup failed: {ex}")
        finally:
            if not getattr(app, "is_web_mode", False) and getattr(app, "tray_manager", None):
                app.tray_manager.stop()
            await _safe_destroy_window(page)

    async def close_dialog(_):
        close_confirm_dialog.open = False
        page.update()

    close_confirm_controls = [
        ft.Text(
            _["confirm_exit_content"],
            size=14,
            text_align=ft.TextAlign.CENTER,
        ),
        ft.Container(height=10),
    ]

    platform_value = page.platform.value if page.platform is not None else ""
    if platform_value != "macos":
        close_confirm_controls.append(
            ft.Text(
                _["minimize_to_tray_tip"],
                size=12,
                color=ft.Colors.GREY_500,
                text_align=ft.TextAlign.CENTER,
            )
        )

    close_confirm_actions = [
        ft.TextButton(
            content=ft.Text(_["cancel"], size=14),
            on_click=close_dialog,
            style=ft.ButtonStyle(
                color=ft.Colors.PRIMARY,
            ),
        ),
        ft.FilledButton(
            content=ft.Text(_["exit_program"], size=14),
            on_click=close_dialog_dismissed,
            style=ft.ButtonStyle(
                bgcolor=ft.Colors.ERROR,
                color=ft.Colors.WHITE,
            ),
        ),
    ]
    if platform_value != "macos":
        close_confirm_actions.insert(
            1,
            ft.TextButton(
                content=ft.Text(_["minimize_to_tray"], size=14),
                on_click=minimize_to_tray,
                style=ft.ButtonStyle(
                    color=ft.Colors.PRIMARY,
                ),
            ),
        )

    close_confirm_dialog = ft.AlertDialog(
        modal=True,
        title=ft.Text(
            _["confirm_exit"],
            size=18,
            weight=ft.FontWeight.BOLD,
            text_align=ft.TextAlign.CENTER,
        ),
        content=ft.Container(
            content=ft.Column(
                controls=close_confirm_controls,
                spacing=5,
                tight=True,
                horizontal_alignment=ft.CrossAxisAlignment.CENTER,
            ),
            padding=ft.Padding.symmetric(horizontal=20, vertical=10),
            width=400 if platform_value != "macos" else None,
        ),
        actions=close_confirm_actions,
        actions_alignment=ft.MainAxisAlignment.END,
        shape=ft.RoundedRectangleBorder(radius=10),
    )

    close_confirm_dialog.open = True
    app.dialog_area.content = close_confirm_dialog
    app.close_confirm_dialog = close_confirm_dialog
    page.update()
