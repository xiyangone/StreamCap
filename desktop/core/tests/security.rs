use streamcap_core::{
    api::bootstrap,
    security::loopback_host,
    service::{Server, ServerOptions},
    Workspace,
};
#[test]
fn only_explicit_loopback_authorities_are_allowed() {
    for host in ["127.0.0.1:6059", "localhost:1420", "[::1]:80"] {
        assert!(loopback_host(host), "{host}");
    }
    for host in [
        "evil.test:6059",
        "127.0.0.1.evil.test",
        "user@localhost:6059",
        "localhost@evil.test",
        "0.0.0.0",
        "",
        "localhost/evil",
    ] {
        assert!(!loopback_host(host), "{host}");
    }
}
#[tokio::test]
async fn rejects_host_and_origin_even_for_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    let server = Server::start(
        state,
        ServerOptions {
            port: 0,
            monitoring: false,
        },
    )
    .await
    .unwrap();
    let url = format!("http://{}/api/status", server.address());
    let client = reqwest::Client::new();
    for method in [reqwest::Method::GET, reqwest::Method::OPTIONS] {
        for (header, value) in [("host", "evil.test"), ("origin", "https://evil.test")] {
            let reply = client
                .request(method.clone(), &url)
                .header(header, value)
                .header("Access-Control-Request-Method", "GET")
                .send()
                .await
                .unwrap();
            assert_eq!(reply.status(), 403, "{method} {header}");
        }
    }
    let reply = client
        .get(&url)
        .header("Origin", "http://tauri.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.headers()["x-content-type-options"], "nosniff");
    assert_eq!(reply.headers()["cache-control"], "no-store");
    server.shutdown().await.unwrap();
}
