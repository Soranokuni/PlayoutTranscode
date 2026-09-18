//! End-to-end tests for the SPA fallback (T0-1 / F-01).
//!
//! These drive the real router. `reqwest` normalises dot segments in a URL, so
//! every traversal attempt goes over a raw socket — the equivalent of
//! `curl --path-as-is`.

mod common;

use common::{raw_body, spawn_test_server, status_line};

#[tokio::test]
async fn serves_real_assets() {
    let s = spawn_test_server().await;

    let r = s.get("/assets/app.js").await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "application/javascript; charset=utf-8"
    );
    assert!(r.text().await.unwrap().contains("export const x"));
}

#[tokio::test]
async fn serves_index_for_root_and_spa_routes() {
    let s = spawn_test_server().await;

    for path in ["/", "/library", "/some/deep/spa/route"] {
        let r = s.get(path).await;
        assert_eq!(r.status(), 200, "{}", path);
        let ct = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert_eq!(ct, "text/html; charset=utf-8", "{}", path);
        assert!(r.text().await.unwrap().contains("<title>spa</title>"), "{}", path);
    }
}

#[tokio::test]
async fn dot_dot_traversal_cannot_escape_the_spa_root() {
    let s = spawn_test_server().await;
    // secret.toml sits one level above web-ui/dist.
    let host = format!("127.0.0.1:{}", s.port);

    for target in [
        "GET /../secret.toml HTTP/1.1",
        "GET /../../secret.toml HTTP/1.1",
        "GET /assets/../../secret.toml HTTP/1.1",
        "GET /./../secret.toml HTTP/1.1",
    ] {
        let resp = s.raw_request(target, &host).await;
        assert!(
            !raw_body(&resp).contains("do-not-serve"),
            "{} leaked the file:\n{}",
            target,
            resp
        );
        assert!(
            status_line(&resp).contains("200"),
            "{} should fall through to index.html, got: {}",
            target,
            status_line(&resp)
        );
        assert!(
            raw_body(&resp).contains("<title>spa</title>"),
            "{} should return index.html",
            target
        );
    }
}

#[tokio::test]
async fn absolute_and_drive_qualified_paths_are_rejected() {
    let s = spawn_test_server().await;
    let host = format!("127.0.0.1:{}", s.port);

    for target in [
        "GET /C:/Windows/win.ini HTTP/1.1",
        "GET /c:/windows/win.ini HTTP/1.1",
        "GET //etc/passwd HTTP/1.1",
    ] {
        let resp = s.raw_request(target, &host).await;
        let body = raw_body(&resp);
        assert!(
            !body.contains("[fonts]") && !body.contains("root:x:"),
            "{} leaked a system file:\n{}",
            target,
            resp
        );
        assert!(
            body.contains("<title>spa</title>"),
            "{} should return index.html, got:\n{}",
            target,
            resp
        );
    }
}

#[tokio::test]
async fn missing_spa_build_does_not_leak_the_directory_path() {
    let s = common::spawn_test_server_with(common::TestServerOptions {
        with_web_ui: false,
        ..Default::default()
    })
    .await;

    let r = s.get("/").await;
    assert_eq!(r.status(), 404);
    let body = r.text().await.unwrap();
    assert_eq!(body, "web UI not built");
    assert!(
        !body.contains(':') && !body.contains('/') && !body.contains('\\'),
        "404 body must not contain a filesystem path: {}",
        body
    );
}

/// SB-04: a content-hashed bundle may be cached forever; nothing else may.
#[tokio::test]
async fn hashed_assets_are_immutable_and_everything_else_revalidates() {
    let s = spawn_test_server().await;

    let r = s.get("/assets/index-AbCd1234.js").await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );

    // app.js carries no content hash, so a rebuild could change it under the
    // same name: it must be revalidated, exactly like index.html.
    for path in ["/", "/assets/app.js", "/some/spa/route"] {
        let r = s.get(path).await;
        assert_eq!(
            r.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-cache"),
            "{} must be revalidated",
            path
        );
    }
}

/// A matching `If-None-Match` saves the body, and the validator itself carries
/// no filesystem path (F-09).
#[tokio::test]
async fn a_matching_etag_is_answered_with_304() {
    let s = spawn_test_server().await;

    let first = s.get("/assets/app.js").await;
    assert_eq!(first.status(), 200);
    let etag = first
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("etag on a static asset")
        .to_string();
    // Only the `W/` weak marker, hex digits and a dash: no path, ever.
    let opaque = etag
        .strip_prefix("W/")
        .expect("weak validator")
        .trim_matches('"');
    assert!(
        opaque.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "etag leaked something other than size-mtime: {}",
        etag
    );

    let second = s
        .client()
        .get(s.url("/assets/app.js"))
        .header("if-none-match", &etag)
        .send()
        .await
        .expect("conditional GET");
    assert_eq!(second.status(), 304);
    assert_eq!(second.text().await.unwrap(), "");

    let stale = s
        .client()
        .get(s.url("/assets/app.js"))
        .header("if-none-match", "W/\"0-0\"")
        .send()
        .await
        .expect("conditional GET");
    assert_eq!(stale.status(), 200);
}

/// A missing bundle or favicon is a real 404, not a 200 of index.html served
/// as `text/html`. The body stays the fixed string.
#[tokio::test]
async fn a_missing_static_asset_is_a_real_404_without_a_path() {
    let s = spawn_test_server().await;

    for path in ["/assets/index-ZZZZ9999.js", "/favicon.svg", "/favicon.ico"] {
        let r = s.get(path).await;
        assert_eq!(r.status(), 404, "{}", path);
        let body = r.text().await.unwrap();
        assert_eq!(body, "not found", "{}", path);
        assert!(!body.contains(':') && !body.contains('/') && !body.contains('\\'));
    }

    // An extensionless path is still an SPA route and still gets index.html.
    let r = s.get("/library/deep").await;
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().contains("<title>spa</title>"));
}

/// BO-03 + SB-04: the build writes `.br`/`.gz` siblings and the handler serves
/// them when the client asks, with the original's Content-Type.
#[tokio::test]
async fn precompressed_siblings_are_served_only_when_accepted() {
    let s = spawn_test_server().await;

    // No Accept-Encoding: the original, uncompressed.
    let plain = s.get("/assets/index-AbCd1234.js").await;
    assert_eq!(plain.status(), 200);
    assert!(plain.headers().get("content-encoding").is_none());
    let plain_etag = plain
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert_eq!(plain.text().await.unwrap(), "export const y = 2;");

    // Brotli is preferred over gzip when both are offered.
    let br = s
        .client()
        .get(s.url("/assets/index-AbCd1234.js"))
        .header("accept-encoding", "gzip, deflate, br")
        .send()
        .await
        .expect("GET");
    assert_eq!(br.status(), 200);
    assert_eq!(
        br.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("br")
    );
    // The type is the original's -- only the bytes are encoded.
    assert_eq!(
        br.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        br.headers().get("vary").and_then(|v| v.to_str().ok()),
        Some("Accept-Encoding")
    );
    let br_etag = br
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    // A different representation gets a different validator, so a client that
    // switches encodings cannot reuse the wrong body.
    assert_ne!(br_etag, plain_etag);
    assert_eq!(br.text().await.unwrap(), "BROTLI-BYTES");

    // gzip only.
    let gz = s
        .client()
        .get(s.url("/assets/index-AbCd1234.js"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .expect("GET");
    assert_eq!(
        gz.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    assert_eq!(gz.text().await.unwrap(), "GZIP-BYTES");

    // `br;q=0` is a refusal, not an offer.
    let refused = s
        .client()
        .get(s.url("/assets/index-AbCd1234.js"))
        .header("accept-encoding", "br;q=0")
        .send()
        .await
        .expect("GET");
    assert!(refused.headers().get("content-encoding").is_none());
    assert_eq!(refused.text().await.unwrap(), "export const y = 2;");

    // A file with no sibling is served plain however eager the client is.
    let no_sibling = s
        .client()
        .get(s.url("/assets/app.js"))
        .header("accept-encoding", "br, gzip")
        .send()
        .await
        .expect("GET");
    assert!(no_sibling.headers().get("content-encoding").is_none());
}
