//! Human-readable labels and pure presentation helpers.
use crate::api::gateway::NewRecording;
use crate::app::i18n::t;

pub const QUALITY_OPTIONS: &[(&str, &str)] = &[
    ("OD", "原画"),
    ("UHD", "超清"),
    ("HD", "高清"),
    ("SD", "标清"),
    ("LD", "流畅"),
];
pub const FORMAT_OPTIONS: &[&str] = &[
    "TS", "FLV", "MKV", "MOV", "MP4", "NUT", "MP3", "M4A", "AAC", "WAV", "WMA",
];
pub const PLATFORMS: &[(&str, &str)] = &[
    ("douyin", "抖音直播"),
    ("kuaishou", "快手直播"),
    ("bilibili", "哔哩哔哩"),
    ("huya", "虎牙直播"),
    ("douyu", "斗鱼直播"),
    ("tiktok", "TikTok"),
    ("yy", "YY直播"),
    ("rednote", "小红书"),
    ("bigo", "Bigo"),
    ("blued", "Blued"),
    ("soop", "SOOP"),
    ("netease", "网易CC"),
    ("qiandurebo", "千度热播"),
    ("pandatv", "PandaTV"),
    ("maoerfm", "猫耳FM"),
    ("look", "LOOK"),
    ("winktv", "WinkTV"),
    ("flextv", "FlexTV"),
    ("popkontv", "PopkonTV"),
    ("twitcasting", "TwitCasting"),
    ("baidu", "百度直播"),
    ("weibo", "微博直播"),
    ("kugou", "酷狗直播"),
    ("twitch", "Twitch"),
    ("liveme", "LiveMe"),
    ("huajiao", "花椒直播"),
    ("showroom", "SHOWROOM"),
    ("acfun", "AcFun"),
    ("inke", "映客直播"),
    ("yinbo", "音播直播"),
    ("changliao", "畅聊直播"),
    ("zhihu", "知乎直播"),
    ("chzzk", "CHZZK"),
    ("haixiu", "嗨秀直播"),
    ("vvxq", "VV星球"),
    ("17live", "17Live"),
    ("langlive", "浪Live"),
    ("piaopiao", "漂漂直播"),
    ("sixroom", "六间房"),
    ("lehai", "乐嗨直播"),
    ("huamao", "花猫直播"),
    ("shopee", "Shopee"),
    ("youtube", "YouTube"),
    ("taobao", "淘宝直播"),
    ("jd", "京东直播"),
    ("faceit", "FACEIT"),
    ("lianjie", "连接直播"),
    ("migu", "咪咕直播"),
    ("laixiu", "来秀直播"),
    ("picarto", "Picarto"),
    ("xindongrebo", "心动热播"),
    ("custom", "自定义流"),
];
pub fn quality_label(code: Option<&str>) -> String {
    let code = code.unwrap_or("").trim().to_ascii_uppercase();
    QUALITY_OPTIONS
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, name)| t(name).into())
        .unwrap_or_else(|| {
            if code.is_empty() {
                t("跟随全局").into()
            } else {
                code
            }
        })
}
pub fn format_label(code: Option<&str>) -> String {
    code.filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| t("跟随全局").into())
}
pub fn platform_label(key: &str) -> String {
    PLATFORMS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, name)| t(name).into())
        .unwrap_or_else(|| key.to_string())
}
pub fn human_size(bytes: u64) -> String {
    let mut size = bytes as f64;
    let mut unit = 0;
    let units = ["B", "KB", "MB", "GB", "TB"];
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", units[unit])
    }
}
pub fn modified_time(value: Option<f64>) -> String {
    value
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| {
            let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(seconds * 1000.0));
            format!(
                "{}-{:02}-{:02} {:02}:{:02}",
                date.get_full_year(),
                date.get_month() + 1,
                date.get_date(),
                date.get_hours(),
                date.get_minutes()
            )
        })
        .unwrap_or_else(|| "—".into())
}
pub fn valid_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|s| {
        let host = s.split(['/', '?', '#']).next().unwrap_or("");
        !host.is_empty() && !host.contains('@') && !url.chars().any(char::is_whitespace)
    })
}
pub fn parse_recordings(raw: &str) -> Result<Vec<NewRecording>, String> {
    let mut items = Vec::new();
    for (index, line) in raw
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        let normalized = line.replace('，', ",");
        let parts: Vec<_> = normalized.split(',').map(str::trim).collect();
        let (quality, url, name) = match parts.as_slice() {
            [url] => (None, *url, None),
            [url, name] if valid_url(url) => (None, *url, Some(*name)),
            [quality, url] => (Some(*quality), *url, None),
            [quality, url, name] => (Some(*quality), *url, Some(*name)),
            _ => {
                return Err(crate::tr_format!(
                    "第 {} 行格式不正确，请每行填写一个地址",
                    index + 1
                ))
            }
        };
        if !valid_url(url) {
            return Err(crate::tr_format!(
                "第 {} 行不是有效的 HTTP / HTTPS 地址",
                index + 1
            ));
        }
        let quality = quality.map(|q| {
            match q {
                "0" => "OD",
                "1" => "UHD",
                "2" => "HD",
                "3" => "SD",
                "4" => "LD",
                _ => q,
            }
            .to_ascii_uppercase()
        });
        if quality
            .as_ref()
            .is_some_and(|q| !QUALITY_OPTIONS.iter().any(|(code, _)| code == q))
        {
            return Err(crate::tr_format!("第 {} 行的清晰度无效", index + 1));
        }
        items.push(NewRecording {
            url: url.into(),
            streamer_name: name.filter(|s| !s.is_empty()).map(str::to_string),
            quality,
        });
    }
    if items.is_empty() {
        return Err(t("请至少填写一个直播间地址").into());
    }
    Ok(items)
}

/// Keep failures and active captures visible in both dashboard and task sorting.
pub fn recording_priority(record: &crate::api::gateway::Recording) -> u8 {
    use crate::api::gateway::TaskPhase;
    match record.task_phase() {
        TaskPhase::Attention => 0,
        TaskPhase::Recording => 1,
        TaskPhase::LiveIdle => 2,
        TaskPhase::Waiting => 3,
        TaskPhase::Paused => 4,
    }
}

pub fn recording_subtitle(record: &crate::api::gateway::Recording) -> String {
    if !record.is_recording && record.check_state == "rechecking" {
        return t("录制已结束，正在确认直播状态").into();
    }
    if !record.is_recording && record.check_error.as_ref().is_some_and(|s| !s.is_empty()) {
        return t(if record.recorded_seconds > 0.0 {
            "录制已结束，直播状态待确认"
        } else {
            "直播状态待确认"
        })
        .into();
    }
    record
        .live_title
        .as_ref()
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            t(if record.is_recording {
                "正在录制"
            } else if record.is_live {
                "直播未录制"
            } else {
                "等待开播"
            })
            .into()
        })
}

pub fn recording_media_job<'a>(
    record: &crate::api::gateway::Recording,
    jobs: &'a [crate::api::gateway::MediaJob],
) -> Option<&'a crate::api::gateway::MediaJob> {
    jobs.iter()
        .rev()
        .find(|job| job.task_id.as_deref() == Some(record.rec_id.as_str()))
        .filter(|job| !record.is_recording || job.state != "complete")
}

pub fn media_progress(job: &crate::api::gateway::MediaJob, now: i64) -> String {
    if job.state == "waiting" {
        return job
            .queue_position
            .map(|position| crate::tr_format!("等待处理 · 第 {} 位", position))
            .unwrap_or_else(|| t("等待处理").into());
    }
    let elapsed = job
        .started_at
        .zip(job.finished_at.or_else(|| job.pending().then_some(now)))
        .map(|(start, end)| end.saturating_sub(start).max(0) as u64)
        .or_else(|| (job.elapsed_seconds > 0).then_some(job.elapsed_seconds))
        .map(|seconds| crate::tr_format!("耗时 {}", duration(seconds as f64)))
        .unwrap_or_else(|| t("耗时未知").into());
    let progress = job
        .stage_percent
        .map(|percent| crate::tr_format!("本阶段 {}%", percent))
        .unwrap_or_else(|| {
            if job.pending() {
                t("本阶段进度待确认").into()
            } else {
                t("处理已结束").into()
            }
        });
    let eta = job
        .stage_eta_seconds
        .map(|seconds| crate::tr_format!("本阶段预计剩余 {}", duration(seconds as f64)))
        .unwrap_or_default();
    format!(
        "{} · {}{}",
        progress,
        elapsed,
        if eta.is_empty() {
            String::new()
        } else {
            format!(" · {eta}")
        }
    )
}

pub fn interval_label(value: &str) -> String {
    let Ok(seconds) = value.parse::<u64>() else {
        return "—".into();
    };
    let minutes = if seconds % 60 == 0 {
        (seconds / 60).to_string()
    } else {
        format!("{:.1}", seconds as f64 / 60.0)
    };
    crate::tr_format!("{} 秒（{} 分钟）", seconds, minutes)
}

pub fn queue_label(record: &crate::api::gateway::Recording) -> String {
    if record.is_recording {
        return t("录制进行中").into();
    }
    if !record.monitor_status {
        return t("已暂停，不安排检测").into();
    }
    if record.check_state == "checking" {
        return if record.recovery_attempt > 0 {
            crate::tr_format!("恢复检测中 · 第 {}/3 次", record.recovery_attempt)
        } else {
            t("正在请求直播状态").into()
        };
    }
    if record.check_state == "rechecking" {
        return t("正在确认直播是否结束").into();
    }
    if record.verification_required || record.check_wait_reason == "verification" {
        return t("等待手动验证或登录").into();
    }
    let reason = t(match record.check_wait_reason.as_str() {
        "platformBusy" => "等待同平台检测完成",
        "platformSpacing" => "同平台请求间隔",
        "cooldown" => "等待平台冷却",
        "capacity" => "等待检测空位",
        "schedule" => "等待定时窗口",
        "recovery" => "断流后退避等待",
        "recoveryExhausted" => "恢复尝试已用尽，等待常规检测",
        "queue" => "即将检测",
        _ => "等待下次检测",
    });
    let reason = if record.check_state == "recovering" {
        crate::tr_format!("恢复第 {}/3 次 · {}", record.recovery_attempt, reason)
    } else {
        reason.to_string()
    };
    record
        .queue_position
        .map(|position| crate::tr_format!("排队第 {} 位 · {}", position, reason))
        .unwrap_or(reason)
}

pub fn queue_time(record: &crate::api::gateway::Recording, now: i64) -> String {
    if record.is_recording
        || !record.monitor_status
        || record.verification_required
        || matches!(
            record.check_wait_reason.as_str(),
            "verification" | "recording"
        )
    {
        return String::new();
    }
    if matches!(record.check_state.as_str(), "checking" | "rechecking") {
        return t("结果返回后自动更新").into();
    }
    if matches!(
        record.check_wait_reason.as_str(),
        "platformBusy" | "capacity"
    ) {
        return t("等待前序任务释放，无固定完成时间").into();
    }
    let Some(at) = record
        .check_ready_at
        .or(record.recovery_at)
        .or(record.next_check_at)
    else {
        return String::new();
    };
    if at <= now {
        return t("已到时间，等待调度").into();
    }
    let remaining = duration((at - now) as f64);
    if record.check_wait_reason == "cooldown" {
        crate::tr_format!("冷却剩余 {}", remaining)
    } else if record.check_wait_reason == "platformSpacing" {
        crate::tr_format!("最早 {} 后执行", remaining)
    } else if record.check_state == "recovering" {
        crate::tr_format!("恢复倒计时 {}", remaining)
    } else if record.check_wait_reason == "schedule" {
        crate::tr_format!("距离窗口开始 {}", remaining)
    } else {
        crate::tr_format!("下次检测 {} 后", remaining)
    }
}

pub fn monitoring_label(record: &crate::api::gateway::Recording) -> &'static str {
    if !record.monitor_status {
        t("监控已暂停")
    } else if record.check_state == "rechecking" {
        t("正在复核，监控保持开启")
    } else if record.check_state == "checking" {
        t("检测中")
    } else if record.check_state == "recovering" {
        t("断流恢复中，监控保持开启")
    } else if record.verification_required {
        t("等待手动验证")
    } else if record.access_state == "cooldown" {
        t("等待平台冷却")
    } else if record.access_state == "pageCheck" {
        t("等待页面检查")
    } else if record.access_state == "loginRequired" {
        t("请在设置中检查登录状态")
    } else if record.access_state == "loginPrompt" {
        t("登录提示不代表登录失效")
    } else if record.check_state == "queued" {
        t("排队中")
    } else if record.check_state == "waiting" {
        t("等待首检")
    } else if record
        .check_error
        .as_ref()
        .is_some_and(|error| !error.is_empty())
    {
        t("等待检测恢复")
    } else if record.only_notify_no_record == Some(true) {
        t("仅通知，不录制")
    } else if record.scheduled_recording == Some(true) {
        t("按时间段自动监控")
    } else {
        t("自动监控，开播即录")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media_job(state: &str) -> crate::api::gateway::MediaJob {
        serde_json::from_value(serde_json::json!({
            "id": "job", "taskId": "task", "source": "old.ts", "output": "old.mp4",
            "state": state, "message": "status"
        }))
        .unwrap()
    }

    #[test]
    fn active_recording_hides_completed_history_without_reviving_older_failures() {
        let mut record = crate::api::gateway::Recording {
            rec_id: "task".into(),
            is_recording: true,
            ..Default::default()
        };
        let jobs = vec![media_job("failed"), media_job("complete")];
        assert!(recording_media_job(&record, &jobs).is_none());
        record.is_recording = false;
        assert_eq!(
            recording_media_job(&record, &jobs).unwrap().state,
            "complete"
        );
    }

    #[test]
    fn active_recording_preserves_pending_and_unsuccessful_processing_results() {
        let record = crate::api::gateway::Recording {
            rec_id: "task".into(),
            is_recording: true,
            ..Default::default()
        };
        for state in [
            "waiting",
            "running",
            "verifying",
            "cleaning",
            "failed",
            "cleanupFailed",
            "cancelled",
        ] {
            let jobs = vec![media_job(state)];
            assert_eq!(recording_media_job(&record, &jobs).unwrap().state, state);
        }
    }

    #[test]
    fn processing_result_selection_is_scoped_to_the_task() {
        let record = crate::api::gateway::Recording {
            rec_id: "task".into(),
            ..Default::default()
        };
        let mut unrelated = media_job("running");
        unrelated.task_id = Some("another-task".into());
        let jobs = vec![media_job("complete"), unrelated];
        assert_eq!(
            recording_media_job(&record, &jobs).unwrap().state,
            "complete"
        );
        assert!(recording_media_job(&record, &jobs[1..]).is_none());
        assert!(recording_media_job(&record, &[]).is_none());
    }

    #[test]
    fn missing_processing_timing_is_unknown_not_zero() {
        let job = media_job("complete");
        assert_eq!(media_progress(&job, 200), "处理已结束 · 耗时未知");
    }

    #[test]
    fn measured_zero_processing_time_remains_zero() {
        let mut job = media_job("complete");
        job.started_at = Some(100);
        job.finished_at = Some(100);
        assert_eq!(media_progress(&job, 200), "处理已结束 · 耗时 00:00:00");
    }

    #[test]
    fn completed_processing_without_finish_time_does_not_keep_counting() {
        let mut job = media_job("complete");
        job.started_at = Some(100);
        job.elapsed_seconds = 7;
        assert_eq!(media_progress(&job, 200), "处理已结束 · 耗时 00:00:07");
        assert_eq!(media_progress(&job, 900), media_progress(&job, 200));
        job.elapsed_seconds = 0;
        assert_eq!(media_progress(&job, 900), "处理已结束 · 耗时未知");
    }

    #[test]
    fn pending_processing_uses_the_clock_and_measured_stage_progress() {
        let mut job = media_job("running");
        job.started_at = Some(100);
        job.stage_percent = Some(25);
        job.stage_eta_seconds = Some(30);
        assert_eq!(
            media_progress(&job, 112),
            "本阶段 25% · 耗时 00:00:12 · 本阶段预计剩余 00:00:30"
        );
        job.state = "waiting".into();
        job.queue_position = Some(2);
        assert_eq!(media_progress(&job, 112), "等待处理 · 第 2 位");
    }
    #[test]
    fn queue_labels_distinguish_a_deadline_from_an_execution_estimate() {
        use crate::api::gateway::Recording;
        let mut record = Recording {
            monitor_status: true,
            next_check_at: Some(5500),
            check_wait_reason: "interval".into(),
            ..Default::default()
        };
        assert_eq!(interval_label("4500"), "4500 秒（75 分钟）");
        assert_eq!(queue_time(&record, 1000), "下次检测 01:15:00 后");
        assert_eq!(queue_time(&record, 5501), "已到时间，等待调度");
        record.check_state = "recovering".into();
        record.recovery_attempt = 2;
        record.queue_position = Some(3);
        record.check_wait_reason = "platformBusy".into();
        assert_eq!(
            queue_label(&record),
            "排队第 3 位 · 恢复第 2/3 次 · 等待同平台检测完成"
        );
        assert_eq!(
            queue_time(&record, 1000),
            "等待前序任务释放，无固定完成时间"
        );
        record.monitor_status = false;
        assert_eq!(queue_label(&record), "已暂停，不安排检测");
        assert!(queue_time(&record, 1000).is_empty());
    }
    #[test]
    fn an_ended_recording_never_displays_a_stale_live_title_as_current() {
        use crate::api::gateway::Recording;
        let mut r = Recording {
            is_live: true,
            live_title: Some("旧直播标题".into()),
            recorded_seconds: 100.0,
            check_state: "rechecking".into(),
            monitor_status: true,
            ..Default::default()
        };
        assert_eq!(recording_subtitle(&r), "录制已结束，正在确认直播状态");
        assert_eq!(monitoring_label(&r), "正在复核，监控保持开启");
        r.check_state = "idle".into();
        r.check_error = Some("empty".into());
        assert_eq!(recording_subtitle(&r), "录制已结束，直播状态待确认");
        r.check_error = None;
        r.is_live = false;
        r.live_title = None;
        assert_eq!(recording_subtitle(&r), "等待开播");
    }
    #[test]
    fn monitoring_labels_and_attention_order_are_explicit() {
        use crate::api::gateway::Recording;
        let mut record = Recording::default();
        assert_eq!(monitoring_label(&record), "监控已暂停");
        record.monitor_status = true;
        assert_eq!(monitoring_label(&record), "自动监控，开播即录");
        record.only_notify_no_record = Some(true);
        assert_eq!(monitoring_label(&record), "仅通知，不录制");
        record.only_notify_no_record = Some(false);
        record.scheduled_recording = Some(true);
        assert_eq!(monitoring_label(&record), "按时间段自动监控");
        record.verification_required = true;
        assert_eq!(monitoring_label(&record), "等待手动验证");
        assert_eq!(recording_priority(&record), 0);
        assert!(record.needs_attention());
        record.verification_required = false;
        record.check_error = Some("检测超时".into());
        assert_eq!(monitoring_label(&record), "等待检测恢复");
        assert_eq!(recording_priority(&record), 0);
        record.check_error = None;
        record.recording_error = Some("fixture".into());
        assert_eq!(recording_priority(&record), 0);
        record.is_recording = true;
        assert_eq!(recording_priority(&record), 1);
        assert_eq!(duration(3661.9), "01:01:01");
        assert_eq!(duration(f64::INFINITY), "00:00:00");
    }
    #[test]
    fn labels_are_readable() {
        assert_eq!(quality_label(Some("OD")), "原画");
        assert_eq!(quality_label(Some("ld")), "流畅");
        assert_eq!(format_label(Some("ts")), "TS");
    }
    #[test]
    fn imports_keep_per_line_names_and_quality() {
        let items = parse_recordings("https://live.douyin.com/1,主播甲\n2,https://live.bilibili.com/2,主播乙\nhttps://www.huya.com/3").unwrap();
        assert_eq!(items[0].streamer_name.as_deref(), Some("主播甲"));
        assert_eq!(items[1].quality.as_deref(), Some("HD"));
        assert_eq!(items[2].streamer_name, None);
        assert_eq!(items[2].quality, None);
    }
    #[test]
    fn imports_preserve_duplicates_for_server_accounting_and_reject_invalid_input() {
        assert!(parse_recordings("file:///private").is_err());
        let items = parse_recordings(
            "https://live.douyin.com/1,甲\nhttps://live.douyin.com/1,乙\nhttps://live.douyin.com/2",
        )
        .unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].url, items[1].url);
        assert_eq!(items[0].streamer_name.as_deref(), Some("甲"));
        assert_eq!(items[1].streamer_name.as_deref(), Some("乙"));
        assert!(parse_recordings("OD,https://live.douyin.com/1").is_ok());
        assert!(!valid_url("https://user:password@live.example.com"));
        assert!(!valid_url("https://"));
    }
    #[test]
    fn file_sizes_do_not_overflow() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(0), "0 B");
    }
}

pub fn duration(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}
