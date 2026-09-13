//! 诊断工具：校验 Rust 能否读取给定目录下的真实 recordings.json。
//!
//! 用法：cargo run --example load_check -- <配置目录>
//! 用途：在改动数据格式相关代码后，拿真实用户数据做只读验证。

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let dir = match std::env::args().nth(1) {
        Some(dir) => dir,
        None => {
            eprintln!("用法: load_check <含 recordings.json 的目录>");
            std::process::exit(2);
        }
    };

    let ws = streamcap_core::Workspace::from_repo_root(PathBuf::from(&dir));
    let store = streamcap_core::Store::new(ws);

    match store.load().await {
        Ok(count) => {
            println!("[OK] 载入成功: {count} 条任务");
            let all = store.all().await;
            for rec in all.iter().take(5) {
                println!(
                    "     - {} | {} | {} | 跟随全局={:?} | monitor_hours={:?}",
                    rec.streamer_name,
                    rec.platform.clone().unwrap_or_default(),
                    rec.url,
                    rec.inherited_fields,
                    rec.monitor_hours
                );
            }
            if all.len() > 5 {
                println!("     ... 其余 {} 条略", all.len() - 5);
            }

            // 传 --persist 时再走一遍写盘，用于验证「载入 → 回写」不会破坏真实数据
            if std::env::args().any(|a| a == "--persist") {
                match store.persist().await {
                    Ok(()) => {
                        let reopened = streamcap_core::Store::new(
                            streamcap_core::Workspace::from_repo_root(PathBuf::from(&dir)),
                        );
                        match reopened.load().await {
                            Ok(n) if n == count => {
                                println!("[OK] 回写后重新载入仍为 {n} 条，数据完整")
                            }
                            Ok(n) => {
                                println!("[FAIL] 回写后条数变为 {n}（原为 {count}）");
                                std::process::exit(1);
                            }
                            Err(err) => {
                                println!("[FAIL] 回写后无法重新载入: {err}");
                                std::process::exit(1);
                            }
                        }
                    }
                    Err(err) => {
                        println!("[FAIL] 写盘失败: {err}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Err(err) => {
            println!("[FAIL] 载入失败: {err}");
            std::process::exit(1);
        }
    }
}
