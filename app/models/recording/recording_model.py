from datetime import time, timedelta
from typing import Any, ClassVar


class Recording:
    # 可继承字段 -> 对应的全局设置键。
    # 语义：值等于当前全局值即视为「跟随全局」，持久化时写 null，
    # 之后改设置页会自动生效；手动改成别的值才固化为该录制项专属。
    # 不含 recording_dir —— 它是 stream_manager 写回的运行时输出目录缓存，
    # 不是用户设置。
    INHERITABLE_FIELDS: ClassVar[dict[str, str]] = {
        "record_format": "video_format",
        "quality": "record_quality",
        "segment_record": "segmented_recording_enabled",
        "segment_time": "video_segment_time",
        "flv_use_direct_download": "flv_use_direct_download",
        "only_notify_no_record": "only_notify_no_record",
    }

    def __init__(
        self,
        rec_id,
        url,
        streamer_name,
        record_format,
        quality,
        segment_record,
        segment_time,
        monitor_status,
        scheduled_recording,
        scheduled_start_time,
        monitor_hours,
        recording_dir,
        enabled_message_push,
        only_notify_no_record,
        flv_use_direct_download,
        video_bitrate=None,
    ):
        """
        Initialize a recording object.

        :param rec_id: Unique identifier for the recording task.
        :param url: URL address of the live stream.
        :param streamer_name: Name of the streamer.
        :param record_format: Format of the recorded file, e.g., 'mp4', 'ts', 'mkv'.
        :param quality: Quality of the recorded video, e.g., 'OD', 'UHD', 'HD'.
        :param segment_record: Whether to enable segmented recording.
        :param segment_time: Time interval (in seconds) for segmented recording if enabled.
        :param monitor_status: Monitoring status, whether the live room is being monitored.
        :param scheduled_recording: Whether to enable scheduled recording.
        :param scheduled_start_time: Scheduled start time for recording (string format like '18:30:00').
        :param monitor_hours: Number of hours to monitor from the scheduled recording start time, e.g., 3.
        :param recording_dir: Directory path where the recorded files will be saved.
        :param enabled_message_push: Whether to enable message push.
        :param only_notify_no_record: Whether to only notify when no record is made.
        :param flv_use_direct_download: Whether to use direct downloader to cache FLV stream.
        :param video_bitrate: Custom output video bitrate in kbps, or None to copy the source video stream.
        """

        self.rec_id = rec_id
        self.url = url
        self.quality = quality
        self.record_format = record_format
        self.monitor_status = monitor_status
        self.segment_record = segment_record
        self.segment_time = segment_time
        self.streamer_name = streamer_name
        self.scheduled_recording = scheduled_recording
        self.scheduled_start_time = scheduled_start_time
        self.monitor_hours = monitor_hours
        self.recording_dir = recording_dir
        self.enabled_message_push = enabled_message_push
        self.only_notify_no_record = only_notify_no_record
        self.flv_use_direct_download = flv_use_direct_download
        self.video_bitrate = video_bitrate
        self.scheduled_time_range: list[str] | None = None
        self.title = f"{streamer_name} - {self.quality}"
        self.speed = "X KB/s"
        self.is_live = False
        self.is_recording = False
        self.start_time = None
        self.manually_stopped = False
        self.force_stop = False
        self.stopping_in_progress = False
        self.stop_requested = False
        self.platform: str | None = None
        self.platform_key: str | None = None
        self.notified_live_start = False
        self.notified_live_end = False

        self.cumulative_duration = timedelta()  # Accumulated recording time
        self.last_duration = timedelta()  # Save the total time of the last recording
        self.display_title = self.title
        self.selected = False
        self.is_checking = False
        self.showed_checking_status = False
        self.status_info: str | None = None
        self.live_title: str | None = None
        self.detection_time: time | None = None
        self.loop_time_seconds: int | None = None
        # 下次检测的实际间隔（loop_time 叠加随机抖动或失败退避后的值）
        self.next_check_due_seconds: int | None = None
        self.consecutive_check_failures: int = 0
        self.use_proxy: bool | None = None
        self.record_url: str | None = None
        self.preview_url: str | None = None
        # 当前跟随全局设置的字段名集合，由 RecordingManager.apply_global_defaults 维护
        self.inherited_fields: set[str] = set()

    def _stored_value(self, attr: str) -> Any:
        """跟随全局的字段持久化为 null，避免把当时的全局值固化成专属值。"""
        return None if attr in self.inherited_fields else getattr(self, attr)

    def to_dict(self) -> dict[str, Any]:
        """Convert the Recording instance to a dictionary for saving."""
        return {
            "rec_id": self.rec_id,
            "url": self.url,
            "streamer_name": self.streamer_name,
            "record_format": self._stored_value("record_format"),
            "quality": self._stored_value("quality"),
            "segment_record": self._stored_value("segment_record"),
            "segment_time": self._stored_value("segment_time"),
            "monitor_status": self.monitor_status,
            "scheduled_recording": self.scheduled_recording,
            "scheduled_start_time": self.scheduled_start_time,
            "monitor_hours": self.monitor_hours,
            "recording_dir": self.recording_dir,
            "enabled_message_push": self.enabled_message_push,
            "platform": self.platform,
            "platform_key": self.platform_key,
            "last_duration": self.last_duration.total_seconds(),
            "only_notify_no_record": self._stored_value("only_notify_no_record"),
            "flv_use_direct_download": self._stored_value("flv_use_direct_download"),
            "video_bitrate": self.video_bitrate,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]):
        """Create a Recording instance from a dictionary."""
        recording = cls(
            data.get("rec_id"),
            data.get("url"),
            data.get("streamer_name"),
            data.get("record_format"),
            data.get("quality"),
            data.get("segment_record"),
            data.get("segment_time"),
            data.get("monitor_status"),
            data.get("scheduled_recording"),
            data.get("scheduled_start_time"),
            data.get("monitor_hours"),
            data.get("recording_dir"),
            data.get("enabled_message_push"),
            data.get("only_notify_no_record"),
            data.get("flv_use_direct_download"),
            data.get("video_bitrate"),
        )
        recording.title = data.get("title", recording.title)
        recording.display_title = data.get("display_title", recording.title)
        last_duration = data.get("last_duration")
        recording.platform = data.get("platform")
        recording.platform_key = data.get("platform_key")
        if last_duration is not None:
            recording.last_duration = timedelta(seconds=float(last_duration))
        return recording

    def update_title(self, quality_info, prefix=None):
        """Helper method to update the title."""
        self.title = f"{self.streamer_name} - {quality_info}"
        # prefix 参数保留兼容签名，但标题不再拼接状态前缀（状态由卡片独立标签展示）
        self.display_title = self.title

    def update(self, updated_info: dict[str, Any]):
        """Update the recording object with new information."""
        for attr, value in updated_info.items():
            if hasattr(self, attr):
                setattr(self, attr, value)
