import flet as ft

from ....models.media.video_format_model import VideoFormat
from ....models.media.video_quality_model import VideoQuality

# 三态取值：不修改 / 跟随全局 / 指定值
KEEP = "__keep__"
FOLLOW = "__follow__"
SPECIFY = "__specify__"


class BatchEditDialog(ft.AlertDialog):
    """批量修改选中录制项的字段。

    每个字段三态：
      不修改   —— 保持各项现状，互不干扰
      跟随全局 —— 该字段改回跟随设置页，之后全局一变就跟着变
      指定值   —— 固化为该值，不再受全局影响

    作用域与批量开始/停止/删除一致：有选中作用于选中项，
    未选中则作用于当前筛选下可见的项。录制中/监控中的项会被跳过。
    """

    def __init__(self, app, on_done=None):
        self.app = app
        self.on_done = on_done
        self._ = {}
        self.load()

        self.quality_field = self._build_dropdown(
            self._["select_resolution"],
            [(q, self._.get(q, q)) for q in VideoQuality.get_qualities()],
        )
        self.format_field = self._build_dropdown(
            self._["select_record_format"],
            [(f, f) for f in VideoFormat.get_formats()],
        )
        self.segment_field = self._build_dropdown(
            self._["is_segment_enabled"],
            [("true", self._["yes"]), ("false", self._["no"])],
        )
        self.segment_time_field = ft.TextField(
            label=self._["segment_record_time"],
            hint_text=self._["input_segment_time"],
            keyboard_type=ft.KeyboardType.NUMBER,
            visible=False,
            expand=True,
        )
        self.segment_time_mode = ft.Dropdown(
            label=self._["segment_record_time"],
            options=[
                ft.dropdown.DropdownOption(KEEP, text=self._["batch_keep"]),
                ft.dropdown.DropdownOption(FOLLOW, text=self._["batch_follow_global"]),
                ft.dropdown.DropdownOption(SPECIFY, text=self._["batch_specify"]),
            ],
            value=KEEP,
            on_select=self.on_segment_time_mode_change,
            expand=True,
        )

        super().__init__(
            title=ft.Text(self._["batch_edit"]),
            content=ft.Container(
                content=ft.Column(
                    [
                        ft.Text(self._["batch_edit_tip"], size=12, opacity=0.7),
                        self.quality_field,
                        self.format_field,
                        self.segment_field,
                        ft.Row([self.segment_time_mode, self.segment_time_field], spacing=8),
                    ],
                    spacing=12,
                    tight=True,
                    scroll=ft.ScrollMode.AUTO,
                ),
                width=460,
            ),
            actions=[
                ft.TextButton(self._["cancel"], on_click=self.close_dialog),
                ft.TextButton(self._["sure"], on_click=self.confirm),
            ],
            actions_alignment=ft.MainAxisAlignment.END,
            modal=True,
        )

    def load(self):
        language = self.app.language_manager.language
        for key in ("recordings_page", "recording_dialog", "base", "video_quality"):
            self._.update(language.get(key, {}))

    def _build_dropdown(self, label: str, values: list[tuple[str, str]]):
        # 用 text= 而非 content=：Flet 0.85 下 content 形式的选项被选中后
        # 输入框会回落显示 key 本身（如 __keep__）而不是展示文本
        options = [
            ft.dropdown.DropdownOption(KEEP, text=self._["batch_keep"]),
            ft.dropdown.DropdownOption(FOLLOW, text=self._["batch_follow_global"]),
        ]
        options += [ft.dropdown.DropdownOption(k, text=t) for k, t in values]
        return ft.Dropdown(label=label, options=options, value=KEEP, expand=True)

    async def on_segment_time_mode_change(self, _):
        self.segment_time_field.visible = self.segment_time_mode.value == SPECIFY
        self.update()

    def _collect(self) -> tuple[dict, set[str], str | None]:
        """把三态选择归拢成 (要固化的字段, 要跟随全局的字段, 错误提示)。"""
        changes: dict = {}
        follow: set[str] = set()

        for field, attr in (
            (self.quality_field, "quality"),
            (self.format_field, "record_format"),
        ):
            if field.value == FOLLOW:
                follow.add(attr)
            elif field.value != KEEP:
                changes[attr] = field.value

        if self.segment_field.value == FOLLOW:
            follow.add("segment_record")
        elif self.segment_field.value != KEEP:
            changes["segment_record"] = self.segment_field.value == "true"

        if self.segment_time_mode.value == FOLLOW:
            follow.add("segment_time")
        elif self.segment_time_mode.value == SPECIFY:
            raw = (self.segment_time_field.value or "").strip()
            if not raw.isdigit() or int(raw) <= 0:
                return {}, set(), self._["input_segment_time"]
            changes["segment_time"] = raw

        return changes, follow, None

    async def confirm(self, _):
        changes, follow, error = self._collect()
        if error:
            await self.app.snack_bar.show_snack_bar(error)
            return
        if not changes and not follow:
            await self.close_dialog(None)
            return

        applied, skipped = await self.app.record_manager.batch_edit_recordings(changes, follow)
        await self.close_dialog(None)

        message = self._["batch_edit_done"].replace("{count}", str(applied))
        if skipped:
            message += self._["batch_edit_skipped"].replace("{count}", str(skipped))
        await self.app.snack_bar.show_snack_bar(message, bgcolor=ft.Colors.PRIMARY)

        if self.on_done:
            await self.on_done()

    async def close_dialog(self, _):
        self.open = False
        self.update()
