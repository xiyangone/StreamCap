import asyncio
import random
import threading
import time
from collections import defaultdict
from datetime import datetime, timedelta

from ...messages import desktop_notify, message_pusher
from ...models.recording.recording_model import Recording
from ...models.recording.recording_status_model import RecordingStatus
from ...utils import utils
from ...utils.logger import logger
from ..platforms.platform_handlers import get_platform_info
from ..runtime.process_manager import BackgroundService
from .stream_manager import LiveStreamRecorder


class GlobalRecordingState:
    recordings = []
    lock = threading.Lock()


class RecordingManager:
    def __init__(self, services):
        self.services = services
        self.settings = services.settings_config
        self.periodic_task_started = False
        self.loop_time_seconds = None
        self.services.language_manager.add_observer(self)
        self.load_recordings()
        self._ = {}
        self.load()
        self.initialize_dynamic_state(persist_migration=False)
        max_concurrent = int(self.settings.user_config.get("platform_max_concurrent_requests", 3))
        self.platform_semaphores = defaultdict(lambda: asyncio.Semaphore(max_concurrent))
        self.active_recorders = {}
        # 同平台相邻请求的最小间隔。多房间同时到期时会被连续检测，
        # 部分平台（如快手）按 IP 做突发限流，无间隔连打会返回错误页，
        # 表现为在播房间被误判为未开播。
        self.platform_request_interval = float(
            self.settings.user_config.get("platform_request_interval", 3)
        )
        self.platform_last_request = defaultdict(float)
        self.platform_pace_locks = defaultdict(asyncio.Lock)
        # 检测到平台风控/封IP后，暂停该平台的周期性检测一段时间，
        # 避免封禁期间持续撞墙、反而延长封禁时长
        self.platform_block_cooldown = 1800.0
        self.platform_block_until = defaultdict(float)

    @property
    def app(self):
        bridges = self.services.snapshot_bridges()
        return bridges[0] if bridges else None

    async def _pace_platform_request(self, platform_key: str) -> None:
        """确保同平台相邻请求之间至少间隔 platform_request_interval 秒。"""
        interval = self.platform_request_interval
        if interval <= 0:
            return

        async with self.platform_pace_locks[platform_key]:
            loop = asyncio.get_running_loop()
            # 间隔加随机抖动，避免精确等间隔的机器特征
            spacing = interval * random.uniform(1.0, 1.5)
            wait = self.platform_last_request[platform_key] + spacing - loop.time()
            if wait > 0:
                await asyncio.sleep(wait)
            self.platform_last_request[platform_key] = loop.time()

    def is_platform_blocked(self, platform_key: str | None) -> bool:
        """该平台是否处于风控冷却期（暂停周期性检测）。"""
        if not platform_key:
            return False
        return time.monotonic() < self.platform_block_until[platform_key]

    def _handle_platform_block(self, platform_key: str | None, platform_name: str | None):
        """记录平台被限制，进入冷却期并通知用户（同一冷却期内只通知一次）。"""
        if not platform_key:
            return
        now = time.monotonic()
        if now < self.platform_block_until[platform_key]:
            return
        self.platform_block_until[platform_key] = now + self.platform_block_cooldown
        minutes = int(self.platform_block_cooldown // 60)
        tip = self._.get("platform_blocked_tip", "{platform}: IP is restricted, checks paused for {minutes} min")
        text = tip.replace("{platform}", str(platform_name or platform_key)).replace("{minutes}", str(minutes))
        logger.warning(
            f"Platform block detected: {platform_key}, pause live checks for {minutes} minutes"
        )
        self.services.broadcast_snack(text)

    @property
    def recordings(self):
        return GlobalRecordingState.recordings

    @recordings.setter
    def recordings(self, value):
        raise AttributeError("Please use add_recording/update_recording methods to modify data")

    def load(self):
        language = self.services.language_manager.language
        for key in ("recording_manager", "video_quality"):
            self._.update(language.get(key, {}))

    def load_recordings(self):
        """Load recordings from a JSON file into objects."""
        recordings_data = self.services.config_manager.load_recordings_config()
        if not GlobalRecordingState.recordings:
            GlobalRecordingState.recordings = [Recording.from_dict(rec) for rec in recordings_data]
        logger.info(f"Live Recordings: Loaded {len(self.recordings)} items")

    def initialize_dynamic_state(self, persist_migration: bool = True):
        """Initialize dynamic state for all recordings.

        Args:
            persist_migration: 迁移出新的跟随字段时是否落盘。构造期事件循环
                还没起来，run_coro 会丢弃协程，故 __init__ 调用时传 False；
                判定本身幂等，跟随状态会在此后任意一次持久化时自然写入。
        """
        loop_time_seconds = self.settings.user_config.get("loop_time_seconds")
        self.loop_time_seconds = int(loop_time_seconds or 300)
        migrated = False
        for recording in self.recordings:
            recording.loop_time_seconds = self.loop_time_seconds
            migrated |= self.apply_global_defaults(recording)
            recording.update_title(self._.get(recording.quality, recording.quality))
            recording.showed_checking_status = True
        if migrated and persist_migration:
            # 让文件形态与语义一致：跟随字段写成 null
            self.services.run_coro(self.persist_recordings())

    @staticmethod
    def _same_setting_value(left, right) -> bool:
        """判断录制项字段值与全局设置值是否等价。

        两边类型并不统一：分段时长在配置里是字符串 "1800"、在对话框里可能是
        整数；录制格式存在 "ts"/"TS" 大小写差异。统一成规范化字符串再比。
        """
        if isinstance(left, bool) or isinstance(right, bool):
            return bool(left) == bool(right)
        return str(left).strip().upper() == str(right).strip().upper()

    def apply_global_defaults(self, recording) -> bool:
        """把全局设置投影到「跟随全局」的字段上，并完成一次性迁移判定。

        三种情形：
        1. 已标记跟随，或持久化值为 null -> 用全局值填充运行时属性
        2. 未标记但值恰好等于当前全局值 -> 判为跟随（老数据迁移，
           下次持久化就会写成 null）
        3. 值与全局不同 -> 保持固化，不受全局变动影响

        Returns:
            跟随标记集合是否发生变化（用于决定要不要落盘）
        """
        user_config = self.settings.user_config
        before = set(recording.inherited_fields)
        for attr, config_key in Recording.INHERITABLE_FIELDS.items():
            global_value = user_config.get(config_key)
            current = getattr(recording, attr, None)

            if attr in recording.inherited_fields or current is None:
                recording.inherited_fields.add(attr)
                if global_value is not None:
                    setattr(recording, attr, global_value)
            elif self._same_setting_value(current, global_value):
                recording.inherited_fields.add(attr)
        return recording.inherited_fields != before

    def apply_global_defaults_to_all(self) -> None:
        """全局设置变更后重新投影，让跟随中的录制项立即生效。"""
        for recording in self.recordings:
            self.apply_global_defaults(recording)
            recording.update_title(self._.get(recording.quality, recording.quality))

    async def add_recordings(self, recordings: list[Recording]) -> None:
        """Add recordings atomically from the user's perspective."""
        if not recordings:
            return

        for recording in recordings:
            # 新建项带的是对话框里的显式值，先判定哪些实际等同于全局设置
            self.apply_global_defaults(recording)

        with GlobalRecordingState.lock:
            GlobalRecordingState.recordings.extend(recordings)
        try:
            await self.persist_recordings()
        except Exception:
            with GlobalRecordingState.lock:
                for recording in recordings:
                    if recording in GlobalRecordingState.recordings:
                        GlobalRecordingState.recordings.remove(recording)
            raise

    async def add_recording(self, recording: Recording) -> None:
        await self.add_recordings([recording])

    async def remove_recording(self, recording: Recording):
        index = self.recordings.index(recording)
        with GlobalRecordingState.lock:
            GlobalRecordingState.recordings.remove(recording)
        try:
            await self.persist_recordings()
        except Exception:
            with GlobalRecordingState.lock:
                GlobalRecordingState.recordings.insert(index, recording)
            raise

    async def clear_all_recordings(self):
        previous_recordings = list(self.recordings)
        with GlobalRecordingState.lock:
            GlobalRecordingState.recordings.clear()
        try:
            await self.persist_recordings()
        except Exception:
            with GlobalRecordingState.lock:
                GlobalRecordingState.recordings[:] = previous_recordings
            raise

    async def persist_recordings(self):
        """Persist recordings to a JSON file."""
        data_to_save = [rec.to_dict() for rec in self.recordings]
        await self.services.config_manager.save_recordings_config(data_to_save)

    async def update_recording_card(self, recording: Recording, updated_info: dict):
        """Update an existing recording object and persist changes to a JSON file."""
        if recording is not None:
            tracked_attrs = set(updated_info) | set(Recording.INHERITABLE_FIELDS) | {"platform", "platform_key"}
            previous_values = {
                attr: getattr(recording, attr) for attr in tracked_attrs if hasattr(recording, attr)
            }
            previous_inherited_fields = set(recording.inherited_fields)

            try:
                recording.update(updated_info)
                recording.platform, recording.platform_key = get_platform_info(recording.url)
                # 对话框传回的是全部字段的显式值：先清空跟随标记再重判，
                # 否则用户把某个原本跟随的字段改成别的值时会被全局值覆盖回去
                recording.inherited_fields.clear()
                self.apply_global_defaults(recording)
                await self.persist_recordings()
            except Exception:
                for attr, value in previous_values.items():
                    setattr(recording, attr, value)
                recording.inherited_fields = previous_inherited_fields
                raise

    @staticmethod
    async def _update_recording(
        recording: Recording, monitor_status: bool, display_title: str, status_info: str, selected: bool
    ):
        attrs_update = {
            "monitor_status": monitor_status,
            "display_title": display_title,
            "status_info": status_info,
            "selected": selected,
        }
        for attr, value in attrs_update.items():
            setattr(recording, attr, value)

    async def start_monitor_recording(self, recording: Recording, auto_save: bool = True):
        """
        Start monitoring a single recording if it is not already being monitored.
        """
        if not recording.monitor_status:
            recording.is_checking = True
            recording.is_live = False
            recording.showed_checking_status = False
            await self._update_recording(
                recording=recording,
                monitor_status=True,
                display_title=recording.title,
                status_info=RecordingStatus.STATUS_CHECKING,
                selected=False,
            )

            self.services.broadcast_card_update(recording)
            self.services.broadcast_pubsub("update", recording)

            self.services.run_coro(self.check_if_live(recording))

            if auto_save:
                self.services.run_coro(self.persist_recordings())

    async def stop_monitor_recording(self, recording: Recording, auto_save: bool = True):
        """
        Stop monitoring a single recording if it is currently being monitored.
        """
        if recording.monitor_status:
            await self._update_recording(
                recording=recording,
                monitor_status=False,
                display_title=f"[{self._['monitor_stopped']}] {recording.title}",
                status_info=RecordingStatus.STOPPED_MONITORING,
                selected=False,
            )
            self.stop_recording(recording, manually_stopped=True)
            self.services.broadcast_card_update(recording)
            self.services.broadcast_pubsub("update", recording)
            if auto_save:
                self.services.run_coro(self.persist_recordings())

    async def start_monitor_recordings(self):
        """
        Start monitoring multiple recordings based on user selection or all recordings if none are selected.
        """
        selected_recordings = await self.get_selected_recordings()
        pre_start_monitor_recordings = selected_recordings or self.recordings
        cards_obj = self._get_visible_cards_obj()
        for recording in pre_start_monitor_recordings:
            if self._is_card_visible(cards_obj, recording):
                self.services.run_coro(self.start_monitor_recording(recording, auto_save=False))
        self.services.run_coro(self.persist_recordings())
        logger.info(f"Batch Start Monitor Recordings: {[i.rec_id for i in pre_start_monitor_recordings]}")

    async def stop_monitor_recordings(self, selected_recordings: list[Recording | None] | None = None):
        """
        Stop monitoring multiple recordings based on user selection or all recordings if none are selected.
        """
        if not selected_recordings:
            selected_recordings = await self.get_selected_recordings()
        pre_stop_monitor_recordings = selected_recordings or self.recordings
        cards_obj = self._get_visible_cards_obj()
        for recording in pre_stop_monitor_recordings:
            if recording is None:
                continue
            if self._is_card_visible(cards_obj, recording):
                self.services.run_coro(self.stop_monitor_recording(recording, auto_save=False))
        self.services.run_coro(self.persist_recordings())
        logger.info(f"Batch Stop Monitor Recordings:{[i.rec_id for i in pre_stop_monitor_recordings if i is not None]}")

    def _get_visible_cards_obj(self):
        app = self.app
        if app is None:
            return None
        return getattr(getattr(app, "record_card_manager", None), "cards_obj", None)

    @staticmethod
    def _is_card_visible(cards_obj, recording) -> bool:
        """When no UI session is available, treat all cards as "visible" so
        batch operations still affect the whole list."""
        if cards_obj is None:
            return True
        entry = cards_obj.get(recording.rec_id)
        if entry is None:
            return False
        card = entry.get("card")
        return getattr(card, "visible", True)

    async def get_selected_recordings(self):
        return [recording for recording in self.recordings if recording.selected]

    def can_edit_recording(self, recording: Recording | None) -> bool:
        """Return whether a recording can be edited without racing active work.

        Monitoring by itself does not lock an item: an offline room may be
        edited and the new configuration will be used by the next live check.
        Only an active check/live/recording lifecycle blocks editing.
        """
        if recording is None:
            return False
        return not (
            recording.is_recording
            or recording.is_live
            or recording.is_checking
            or recording.stopping_in_progress
            or recording.status_info == RecordingStatus.PREPARING_RECORDING
            or recording.rec_id in self.active_recorders
        )

    async def get_batch_target_recordings(self) -> list[Recording]:
        """批量操作的作用域：有选中就只作用于选中项，否则作用于当前筛选下可见的项。

        与批量开始/停止/删除保持同一套语义。
        """
        selected = await self.get_selected_recordings()
        candidates = selected or self.recordings
        cards_obj = self._get_visible_cards_obj()
        return [rec for rec in candidates if rec is not None and self._is_card_visible(cards_obj, rec)]

    async def batch_edit_recordings(self, changes: dict, follow_global: set[str]) -> tuple[int, int]:
        """批量修改录制项字段。

        Args:
            changes: 要固化的字段与值，如 {"quality": "HD"}
            follow_global: 要改回「跟随全局」的字段名集合

        Returns:
            (成功数, 因活跃检测/直播/录制而跳过数)

        仍在监控但处于离线状态的项允许编辑；正在检测、直播、准备录制或录制中的项
        会被跳过，以免运行参数在活跃任务中途发生变化。
        """
        targets = await self.get_batch_target_recordings()
        applied = skipped = 0
        changed_recordings: list[Recording] = []
        snapshots: list[tuple[Recording, dict, set[str]]] = []

        for recording in targets:
            if not self.can_edit_recording(recording):
                skipped += 1
                continue

            tracked_attrs = set(Recording.INHERITABLE_FIELDS) | {"title", "display_title"}
            snapshots.append(
                (
                    recording,
                    {attr: getattr(recording, attr) for attr in tracked_attrs},
                    set(recording.inherited_fields),
                )
            )
            for attr, value in changes.items():
                setattr(recording, attr, value)
                recording.inherited_fields.discard(attr)
            for attr in follow_global:
                recording.inherited_fields.add(attr)

            # 重新投影跟随项；批量对话框里的“指定值”是明确意图，
            # 即使等于当前全局值也必须保持固化。
            self.apply_global_defaults(recording)
            for attr in changes:
                recording.inherited_fields.discard(attr)
            recording.update_title(self._.get(recording.quality, recording.quality))
            changed_recordings.append(recording)
            applied += 1

        if applied:
            try:
                await self.persist_recordings()
            except Exception:
                for recording, values, inherited_fields in snapshots:
                    for attr, value in values.items():
                        setattr(recording, attr, value)
                    recording.inherited_fields = inherited_fields
                raise
            for recording in changed_recordings:
                self.services.broadcast_card_update(recording)
                self.services.broadcast_pubsub("update", recording)
        logger.info(f"Batch Edit Recordings: applied={applied} skipped={skipped} changes={changes}")
        return applied, skipped

    async def remove_recordings(self, recordings: list[Recording]):
        """Remove a recording from the list and update the JSON file."""
        for recording in recordings:
            if recording in self.recordings:
                await self.remove_recording(recording)
                logger.info(f"Delete Items: {recording.rec_id}-{recording.streamer_name}")

    def find_recording_by_id(self, rec_id: str):
        """Find a recording by its ID (hash of dict representation)."""
        for rec in self.recordings:
            if rec.rec_id == rec_id:
                return rec
        return None

    async def check_all_live_status(self):
        """Check the live status of all recordings and update their display titles."""
        for recording in self.recordings:
            if recording.monitor_status and not recording.is_recording:
                if self.is_platform_blocked(getattr(recording, "platform_key", None)):
                    continue
                due_seconds = getattr(recording, "next_check_due_seconds", None) or recording.loop_time_seconds
                is_exceeded = utils.is_time_interval_exceeded(recording.detection_time, due_seconds)
                if not recording.detection_time or is_exceeded:
                    self.services.run_coro(self.check_if_live(recording))

    _periodic_task_running = False

    @classmethod
    def is_periodic_task_running(cls):
        return cls._periodic_task_running

    @classmethod
    def set_periodic_task_running(cls, value=True):
        cls._periodic_task_running = value

    async def setup_periodic_live_check(self, interval: int = 180):
        """Set up a periodic task to check live status."""

        async def periodic_check():
            logger.info("Starting periodic live check background task")
            try:
                # 外层以固定小步长轮转，真正的检测节奏由每个房间的
                # next_check_due_seconds（loop_time + 抖动/退避）决定；
                # 这样各房间会自然错峰，而不是同一时刻集体到期
                tick = max(10, min(30, interval))
                delay_first_round = self.services.settings_config.user_config.get(
                    "check_live_on_browser_refresh", True
                )
                # 启动后先等待再首查，避免反复重启软件造成突发请求
                await asyncio.sleep(interval if delay_first_round else 10)
                last_space_check = 0.0
                while True:
                    try:
                        now = asyncio.get_running_loop().time()
                        # 磁盘检查维持原有低频：空间不足时它会持续弹提示，不能跟着 tick 走
                        if now - last_space_check >= interval:
                            last_space_check = now
                            await self.check_free_space()
                        if self.services.recording_enabled:
                            await self.check_all_live_status()
                    except asyncio.CancelledError:
                        raise
                    except Exception as exc:
                        logger.error(f"Periodic live check iteration failed: {exc}")
                    await asyncio.sleep(tick)
            finally:
                self.periodic_task_started = False
                RecordingManager.set_periodic_task_running(False)
                logger.info("Periodic live check background task stopped")

        if not RecordingManager.is_periodic_task_running():
            RecordingManager.set_periodic_task_running(True)
            self.periodic_task_started = True
            logger.info(f"Initializing periodic live check task with interval: {interval}s")
            asyncio.create_task(periodic_check())
        else:
            logger.info("Periodic live check task already running globally, skipping initialization")

    async def check_if_live(self, recording: Recording):
        """Check if the live stream is available, fetch stream data and update is_live status."""

        recording.manually_stopped = False
        if recording.is_recording or recording.stopping_in_progress:
            logger.debug(f"Skip check_if_live because recording is busy: {recording.url}")
            return

        if recording.rec_id in self.active_recorders:
            logger.debug(f"Skip check_if_live because recorder is active: {recording.url}")
            return

        if not recording.monitor_status:
            recording.display_title = f"[{self._['monitor_stopped']}] {recording.title}"
            recording.status_info = RecordingStatus.STOPPED_MONITORING
            recording.is_checking = False
            self.services.broadcast_card_update(recording)
            return

        recording.detection_time = datetime.now().time()
        base_loop_seconds = int(recording.loop_time_seconds or self.loop_time_seconds or 300)
        # 下次检测时间加 0-25% 随机抖动，避免所有房间长期同步到期
        recording.next_check_due_seconds = int(base_loop_seconds * random.uniform(1.0, 1.25))
        recording.is_checking = True

        if not recording.showed_checking_status:
            recording.status_info = RecordingStatus.STATUS_CHECKING
            recording.showed_checking_status = True
            self.services.broadcast_card_update(recording)

        if recording.scheduled_recording:
            scheduled_time_range_list = await self.get_scheduled_time_range(
                recording.scheduled_start_time, recording.monitor_hours
            )
            recording.scheduled_time_range = scheduled_time_range_list
            in_scheduled = False
            for scheduled_time_range in scheduled_time_range_list or []:
                in_scheduled = utils.is_current_time_within_range(scheduled_time_range)
                if in_scheduled:
                    break

            if not in_scheduled:
                recording.status_info = RecordingStatus.NOT_IN_SCHEDULED_CHECK
                recording.is_live = False
                recording.is_checking = False
                logger.info(f"Skip Detection: {recording.url} not in scheduled check range {scheduled_time_range_list}")
                self.services.broadcast_card_update(recording)
                return

        recording.status_info = RecordingStatus.STATUS_CHECKING
        platform, platform_key = get_platform_info(recording.url)

        if not platform or not platform_key:
            recording.is_checking = False
            recording.status_info = RecordingStatus.LIVE_STATUS_CHECK_ERROR
            self.services.broadcast_card_update(recording)
            self.services.broadcast_pubsub("update", recording)
            return

        if recording.platform is None or recording.platform_key is None:
            recording.platform = platform
            recording.platform_key = platform_key
            self.services.run_coro(self.persist_recordings())

        display_platform = platform_key if self.settings.user_config.get("language") != "zh_CN" else platform

        output_dir = self.settings.get_video_save_path()
        await self.check_free_space(output_dir)
        if not self.services.recording_enabled:
            recording.is_checking = False
            recording.status_info = RecordingStatus.NOT_RECORDING_SPACE
            return
        recording_info = {
            "platform": display_platform,
            "platform_key": platform_key,
            "live_url": recording.url,
            "output_dir": output_dir,
            "segment_record": recording.segment_record,
            "segment_time": recording.segment_time,
            "save_format": recording.record_format,
            "quality": recording.quality,
            "video_bitrate": recording.video_bitrate,
        }

        semaphore = self.platform_semaphores[platform_key]
        recorder = LiveStreamRecorder(self.services, recording, recording_info)
        async with semaphore:
            await self._pace_platform_request(platform_key)
            stream_info = await recorder.fetch_stream()
            logger.info(f"Stream Data: {stream_info}")
        if not stream_info or not stream_info.anchor_name:
            fetch_error = getattr(recorder, "last_fetch_error", None)
            logger.error(f"Fetch stream data failed: {recording.url} | {fetch_error or 'unknown reason'}")
            recording.is_checking = False
            failures = getattr(recording, "consecutive_check_failures", 0) + 1
            recording.consecutive_check_failures = failures
            # 连续失败按指数退避（封顶4倍循环时间），避免失败期间高频重试
            backoff = min(2 ** (failures - 1), 4)
            recording.next_check_due_seconds = int(base_loop_seconds * backoff * random.uniform(1.0, 1.25))
            if utils.is_platform_block_error(fetch_error):
                recording.status_info = RecordingStatus.PLATFORM_BLOCKED
                self._handle_platform_block(platform_key, platform)
            else:
                recording.status_info = RecordingStatus.LIVE_STATUS_CHECK_ERROR
            if recording.monitor_status:
                self.services.broadcast_card_update(recording)
                self.services.broadcast_pubsub("update", recording)
            return
        recording.consecutive_check_failures = 0
        if self.settings.user_config.get("remove_emojis"):
            stream_info.anchor_name = utils.clean_name(stream_info.anchor_name, self._["live_room"])

        if stream_info.is_live:
            recording.live_title = stream_info.title
            if recording.streamer_name.strip() == self._["live_room"]:
                recording.streamer_name = stream_info.anchor_name
            recording.title = f"{recording.streamer_name} - {self._[recording.quality]}"
            recording.display_title = f"[{self._['is_live']}] {recording.title}"

            if not recording.is_live:
                recording.is_live = stream_info.is_live
                recording.notified_live_start = False
                recording.notified_live_end = False

                tray_icon_path = self.services.tray_manager.icon_path if self.services.tray_manager is not None else ""
                if desktop_notify.should_push_notification(self.app):
                    desktop_notify.send_notification(
                        title=self._["notify"],
                        message=recording.streamer_name + " | " + self._["live_recording_started_message"],
                        app_icon=tray_icon_path,
                    )

            msg_manager = message_pusher.MessagePusher(self.settings)
            user_config = self.settings.user_config
            if (
                msg_manager.should_push_message(self.settings, recording, message_type="start")
                and not recording.notified_live_start
            ):
                push_content = self._["push_content"]
                begin_push_message_text = user_config.get("custom_stream_start_content")
                if begin_push_message_text:
                    push_content = begin_push_message_text

                push_at = datetime.today().strftime("%Y-%m-%d %H:%M:%S")
                push_content = (
                    push_content.replace("[room_name]", recording.streamer_name)
                    .replace("[time]", push_at)
                    .replace("[title]", recording.live_title or "None")
                )
                msg_title = user_config.get("custom_notification_title").strip()
                msg_title = msg_title or self._["status_notify"]

                BackgroundService.get_instance().add_task(msg_manager.push_messages_sync, msg_title, push_content)
                recording.notified_live_start = True

            if not recording.only_notify_no_record:
                recording.status_info = RecordingStatus.PREPARING_RECORDING
                recording.loop_time_seconds = self.loop_time_seconds
                self.start_update(recording)
                self.services.run_coro(recorder.start_recording(stream_info))
            else:
                if recording.notified_live_start:
                    notify_loop_time = user_config.get("notify_loop_time")
                    recording.loop_time_seconds = int(notify_loop_time or 600)
                else:
                    recording.loop_time_seconds = self.loop_time_seconds

                recording.cumulative_duration = timedelta()
                recording.last_duration = timedelta()
                recording.status_info = RecordingStatus.LIVE_BROADCASTING

        else:
            recording.is_recording = False
            if recording.is_live:
                recording.is_live = False
                asyncio.create_task(recorder.end_message_push())

            recording.status_info = RecordingStatus.MONITORING
            title = f"{stream_info.anchor_name or recording.streamer_name} - {self._[recording.quality]}"
            if recording.streamer_name == self._["live_room"] or f"[{self._['is_live']}]" in recording.display_title:
                recording.update(
                    {
                        "streamer_name": stream_info.anchor_name,
                        "title": title,
                        "display_title": title,
                    }
                )
                self.services.run_coro(self.persist_recordings())

        recording.is_checking = False
        self.services.broadcast_card_update(recording)
        self.services.broadcast_pubsub("update", recording)
        return

    @staticmethod
    def start_update(recording: Recording):
        """Start the recording process."""
        if recording.is_live and not recording.is_recording:
            # Reset cumulative and last durations for a fresh start
            recording.update(
                {
                    "cumulative_duration": timedelta(),
                    "last_duration": timedelta(),
                    "start_time": datetime.now(),
                    "is_recording": True,
                }
            )
            logger.info(f"Started recording for {recording.title}")

    def stop_recording(self, recording: Recording, manually_stopped: bool = True):
        """Stop the recording process."""
        recording.is_live = False
        if recording.is_recording:
            recording.stopping_in_progress = True

            logger.info(f"Trying to stop recorder for {recording.rec_id}, title: {recording.title}")
            logger.debug(f"Active recorders: {list(self.active_recorders.keys())}")

            if recording.rec_id in self.active_recorders:
                recorder = self.active_recorders[recording.rec_id]
                logger.debug(f"Found recorder instance - id: {id(recorder)}")
                recorder.request_stop()
                logger.info(f"Requested stop for recorder: {recording.rec_id}")
            else:
                logger.warning(f"No active recorder found for {recording.rec_id}, cannot request stop")
                recording.force_stop = True
                logger.info(f"Set force_stop=True for recording: {recording.rec_id}")

            if recording.start_time is not None:
                elapsed = datetime.now() - recording.start_time
                # Add the elapsed time to the cumulative duration.
                recording.cumulative_duration += elapsed
                # Update the last recorded duration.
                recording.last_duration = recording.cumulative_duration
            recording.start_time = None
            recording.is_recording = False
            recording.manually_stopped = manually_stopped
            recording.status_info = RecordingStatus.NOT_RECORDING
            logger.info(f"Stopped recording for {recording.title}")

            self.services.run_coro(self._reset_stopping_flag(recording))

    def get_duration(self, recording: Recording):
        """Get the duration of the current recording session in a formatted string."""
        if recording.is_recording and recording.start_time is not None:
            elapsed = datetime.now() - recording.start_time
            # If recording, add the current session time.
            total_duration = recording.cumulative_duration + elapsed
            return self._["recorded"] + " " + str(total_duration).split(".")[0]
        else:
            # If stopped, show the last recorded total duration.
            total_duration = recording.last_duration
            return str(total_duration).split(".")[0]

    async def delete_recording_cards(self, recordings: list[Recording]):
        self.services.broadcast_card_remove(recordings)
        self.services.broadcast_pubsub("delete", recordings)
        await self.remove_recordings(recordings)

    async def check_free_space(self, output_dir: str | None = None):
        disk_space_limit = float(self.settings.user_config.get("recording_space_threshold") or 0)
        output_dir = output_dir or self.settings.get_video_save_path()
        if utils.check_disk_capacity(output_dir) < disk_space_limit:
            self.services.recording_enabled = False
            logger.error(f"Disk space remaining is below {disk_space_limit} GB. Recording function disabled")
            self.services.broadcast_snack(
                self._["not_disk_space_tip"],
                duration=86400,
                show_close_icon=True,
            )

        else:
            self.services.recording_enabled = True

    @staticmethod
    async def get_scheduled_time_range(scheduled_start_time, monitor_hours) -> list | None:
        if not scheduled_start_time:
            return None
        scheduled_time_range_list = []
        monitor_hours_list = str(monitor_hours).split(",") if monitor_hours else []
        for index, start_time in enumerate(str(scheduled_start_time).split(",")):
            try:
                hours = monitor_hours_list[index] if index < len(monitor_hours_list) else ""
                if start_time and hours:
                    end_time = utils.add_hours_to_time(start_time, float(hours or 5))
                    scheduled_time_range = f"{start_time}~{end_time}"
                    scheduled_time_range_list.append(scheduled_time_range)
            except Exception:
                pass
        return scheduled_time_range_list

    @staticmethod
    async def _reset_stopping_flag(recording: Recording):
        recording.stopping_in_progress = False
        logger.debug(f"Reset stopping_in_progress flag for recording: {recording.rec_id}")
