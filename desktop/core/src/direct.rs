//! Native bounded-buffer FLV download. One owner, cooperative stop and no subprocess.
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Clone)]
pub struct Download {
    pub stop: CancellationToken,
    pub done: CancellationToken,
    pub result: Arc<Mutex<Option<Result<(), String>>>>,
}
impl Download {
    pub async fn start(
        path: &Path,
        url: &str,
        proxy: Option<&str>,
        headers: Option<&str>,
        tracker: &TaskTracker,
    ) -> Result<Self, String> {
        crate::platforms::http::validate_url(url)?;
        let path = path.to_path_buf();
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .user_agent(crate::engine::FFMPEG_USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = proxy {
            client = client.proxy(reqwest::Proxy::all(proxy).map_err(|_| "录制代理无效")?);
        }
        let client = client.build().map_err(|_| "FLV 下载器初始化失败")?;
        let mut request = client.get(url);
        for line in headers
            .unwrap_or("")
            .split("\r\n")
            .filter(|s| !s.is_empty())
        {
            let (key, value) = line.split_once(':').ok_or("录制请求头无效")?;
            request = request.header(key.trim(), value.trim());
        }
        let download = Self {
            stop: CancellationToken::new(),
            done: CancellationToken::new(),
            result: Arc::new(Mutex::new(None)),
        };
        let worker = download.clone();
        tracker.spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut file: Option<tokio::fs::File> = None;
            let work = async {
                let mut response = request.send().await.map_err(|_| "FLV 拉流连接失败")?;
                if !response.status().is_success() {
                    return Err(format!(
                        "FLV 播放地址返回 HTTP {}",
                        response.status().as_u16()
                    ));
                }
                let mut prefix = Vec::with_capacity(9);
                let mut written = 0u64;
                while let Some(chunk) = response.chunk().await.map_err(|_| "FLV 拉流连接中断")?
                {
                    let mut start = 0;
                    if file.is_none() {
                        start = (9 - prefix.len()).min(chunk.len());
                        prefix.extend_from_slice(&chunk[..start]);
                        if prefix.len() < 9 {
                            continue;
                        }
                        let offset = u32::from_be_bytes(
                            prefix[5..9].try_into().map_err(|_| "FLV 文件头无效")?,
                        );
                        if &prefix[..3] != b"FLV" || prefix[3] != 1 || !(9..=4096).contains(&offset)
                        {
                            return Err("服务器响应不是有效 FLV，未创建录像文件".into());
                        }
                        let mut output = tokio::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                            .await
                            .map_err(|_| "无法创建 FLV，未覆盖已有文件")?;
                        output
                            .write_all(&prefix)
                            .await
                            .map_err(|_| "写入 FLV 失败")?;
                        written += prefix.len() as u64;
                        file = Some(output);
                    }
                    file.as_mut()
                        .expect("FLV output")
                        .write_all(&chunk[start..])
                        .await
                        .map_err(|_| "写入 FLV 失败，请检查剩余空间")?;
                    written += (chunk.len() - start) as u64;
                }
                if written <= 13 {
                    return Err("服务器未返回有效 FLV 媒体数据".into());
                }
                Ok(())
            };
            let result =
                tokio::select! {biased;_=worker.stop.cancelled()=>Ok(()),result=work=>result};
            let flush = if let Some(file) = &mut file {
                match file.flush().await {
                    Ok(()) => file
                        .sync_all()
                        .await
                        .map_err(|_| "FLV 收尾写盘失败".to_string()),
                    Err(_) => Err("FLV 收尾写盘失败".into()),
                }
            } else {
                Ok(())
            };
            let result = result.and(flush);
            drop(file);
            *worker.result.lock().await = Some(result);
            worker.done.cancel();
        });
        Ok(download)
    }
    pub async fn stop(&self) {
        self.stop.cancel();
        self.done.cancelled().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn invalid_responses_create_no_recording_and_existing_files_are_never_replaced() {
        use axum::{routing::get, Router};
        let root = tempfile::tempdir().unwrap();
        let app = Router::new()
            .route("/invalid", get(|| async { "not an FLV document" }))
            .route(
                "/valid",
                get(|| async { b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00fixture".to_vec() }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let tracker = TaskTracker::new();
        let target = root.path().join("invalid.flv");
        let job = Download::start(&target, &format!("{base}/invalid"), None, None, &tracker)
            .await
            .unwrap();
        job.done.cancelled().await;
        assert!(job.result.lock().await.as_ref().unwrap().is_err());
        assert!(!target.exists());
        let target = root.path().join("existing.flv");
        std::fs::write(&target, b"original").unwrap();
        let job = Download::start(&target, &format!("{base}/valid"), None, None, &tracker)
            .await
            .unwrap();
        job.done.cancelled().await;
        assert!(job.result.lock().await.as_ref().unwrap().is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"original");
        tracker.close();
        tracker.wait().await;
        server.abort();
        let _ = server.await;
    }
}
