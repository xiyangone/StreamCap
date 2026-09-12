//! Owns the bound listener and background tasks. Shutdown is explicit, ordered and awaitable.
use crate::{
    api::{self, ApiState},
    GATEWAY_PORT,
};
use std::{io, net::SocketAddr, sync::atomic::Ordering, time::Duration};
use tokio::{
    sync::{watch, Mutex},
    task::JoinHandle,
};

#[derive(Clone, Copy)]
pub struct ServerOptions {
    pub port: u16,
    pub monitoring: bool,
}
impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            port: GATEWAY_PORT,
            monitoring: true,
        }
    }
}
struct Running {
    http: JoinHandle<io::Result<()>>,
    background: Vec<JoinHandle<()>>,
}
pub struct Server {
    state: ApiState,
    stop: watch::Sender<bool>,
    running: Mutex<Option<Running>>,
    address: SocketAddr,
}
impl Server {
    pub async fn start(state: ApiState, options: ServerOptions) -> io::Result<Self> {
        // Bind first so occupied ports never leave a partially running scheduler.
        let listener =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.port)).await?;
        let address = listener.local_addr()?;
        let (stop, receiver) = watch::channel(false);
        let cancel = state.resolver.cancellation();
        let mut background = Vec::new();
        if options.monitoring {
            let scheduler = state.scheduler.clone();
            background.push(tokio::spawn(scheduler.run(receiver.clone())));
            let scheduler = state.scheduler.clone();
            let mut receiver = receiver;
            background.push(tokio::spawn(async move{
                loop {
                    tokio::select!{biased;_=receiver.changed()=>return,_=tokio::time::sleep(Duration::from_secs(60))=>{}}
                    if let Err(error)=scheduler.check_free_space().await{log::warn!("空间检查: {error}");}
                }
            }));
        }
        let app = api::router(state.clone());
        let http = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await
        });
        Ok(Self {
            state,
            stop,
            running: Mutex::new(Some(Running { http, background })),
            address,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn state(&self) -> &ApiState {
        &self.state
    }
    pub fn request_shutdown(&self) {
        self.state.recording_enabled.store(false, Ordering::SeqCst);
        self.state.engine.begin_shutdown();
        self.state.resolver.begin_shutdown();
        let _ = self.stop.send(true);
    }
    pub async fn shutdown(&self) -> io::Result<()> {
        let mut guard = self.running.lock().await;
        let Some(mut running) = guard.take() else {
            return Ok(());
        };
        self.request_shutdown();
        let core_result = api::shutdown(&self.state).await;
        for task in running.background {
            task.await.map_err(io::Error::other)?;
        }
        match tokio::time::timeout(Duration::from_secs(5), &mut running.http).await {
            Ok(result) => result.map_err(io::Error::other)??,
            Err(_) => {
                running.http.abort();
                let _ = running.http.await;
                log::warn!("已关闭仍未完成的本地 HTTP 连接");
            }
        }
        core_result
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}
