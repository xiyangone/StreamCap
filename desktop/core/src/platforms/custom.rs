use super::http::validate_url;
use crate::resolver::{ResolveRequest, StreamInfo};

pub fn resolve(request: &ResolveRequest) -> Result<StreamInfo, String> {
    let url = validate_url(&request.url)?;
    let path = url.path().to_ascii_lowercase();
    if ![
        ".m3u8", ".flv", ".mp4", ".ts", ".mp3", ".m4a", ".aac", ".wav", ".mkv",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
    {
        return Err("此平台尚未迁移；自定义流必须是明确的媒体直链".into());
    }
    Ok(StreamInfo {
        platform: "自定义流".into(),
        anchor_name: "自定义直播".into(),
        is_live: true,
        record_url: url.to_string(),
        m3u8_url: if path.ends_with(".m3u8") {
            url.to_string()
        } else {
            String::new()
        },
        flv_url: if path.ends_with(".flv") {
            url.to_string()
        } else {
            String::new()
        },
        ..Default::default()
    })
}
