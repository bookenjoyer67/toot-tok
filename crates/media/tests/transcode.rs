//! Phase 3 wave B tests: transcode ladder (fixture-driven, real ffmpeg) with
//! faststart atom-order assertion, asset serving with HTTP Range support, and
//! the clip metadata endpoint (axum oneshot against the server router + a real
//! Postgres via the same `TOOTTOK_TEST_DB` harness the db crate uses).
//!
//! Fixture-dependent tests (real ffprobe/ffmpeg) print a note and return early
//! when the ffmpeg binary is missing. DB-touching tests panic loudly on setup
//! failure unless `TOOTTOK_TEST_SKIP=1`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use toottok_db::actor::Actor;
use toottok_db::clip::Clip;
use toottok_db::job::Job;
use toottok_db::media_asset::MediaAsset;
use toottok_db::settings::Setting;
use toottok_media::probe;
use toottok_media::store::LocalStore;
use toottok_media::transcode;
use tower::ServiceExt;

const DEFAULT_TEST_URL: &str = "postgres://toottok:toottok@127.0.0.1:5433/toottok_test";

/// Serializes DB-touching tests; each one drops/recreates the schema.
fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Drop + recreate a per-process test database, then migrate. Returns `None`
/// (after printing a hint) only when `TOOTTOK_TEST_SKIP=1`; otherwise any
/// setup failure panics so a missing database is never silently ignored.
async fn setup() -> Option<sqlx::PgPool> {
    match setup_inner().await {
        Ok(pool) => Some(pool),
        Err(e) => {
            if std::env::var("TOOTTOK_TEST_SKIP").as_deref() == Ok("1") {
                eprintln!("toottok-media transcode test setup failed ({e}); TOOTTOK_TEST_SKIP=1 set, skipping");
                None
            } else {
                panic!("toottok-media transcode test setup failed: {e}");
            }
        }
    }
}

async fn setup_inner() -> Result<sqlx::PgPool, Box<dyn std::error::Error>> {
    let url = std::env::var("TOOTTOK_TEST_DB").unwrap_or_else(|_| DEFAULT_TEST_URL.to_string());
    let options: PgConnectOptions = url.parse()?;
    let db_name = format!("toottok_media_test_{}", std::process::id());

    let maintenance = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await?;

    for sql in [
        format!("DROP DATABASE IF EXISTS {db_name} WITH (FORCE);"),
        format!("CREATE DATABASE {db_name};"),
    ] {
        sqlx::query(&sql).execute(&maintenance).await?;
    }
    maintenance.close().await;

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options.clone().database(&db_name))
        .await?;
    toottok_db::migrate(&pool).await?;
    Ok(pool)
}

async fn make_actor(pool: &sqlx::PgPool, username: &str) -> Actor {
    let ap_id = format!("https://{username}.example/actor/{username}");
    Actor::create(
        pool,
        username,
        None,
        "person",
        "PUBKEY",
        None,
        &format!("{ap_id}/inbox"),
        None,
        &format!("{ap_id}/outbox"),
        &format!("{ap_id}/followers"),
        &ap_id,
    )
    .await
    .expect("actor insert should succeed")
}

/// Generate a 1s 128x720 mp4 (video + AAC audio) with ffmpeg. `None` when
/// ffmpeg is unavailable.
fn fixture_720(dir: &Path) -> Option<PathBuf> {
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping fixture-dependent transcode test");
        return None;
    }
    let path = dir.join("fixture-720.mp4");
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc=duration=1:size=128x720:rate=10")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("anullsrc=r=44100:cl=stereo")
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-shortest")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(path)
}

/// Assert the `-movflags +faststart` contract: within the first 64KiB, `moov`
/// must appear before `mdat` (or `mdat` may be absent from the window, but
/// `moov` must be present).
fn assert_faststart(path: &Path) {
    use std::io::Read;
    let mut f = std::fs::File::open(path).expect("open output");
    let mut head = vec![0u8; 64 * 1024];
    let n = f.read(&mut head).expect("read output head");
    let head = &head[..n];

    let find = |tag: &[u8; 4]| head.windows(4).position(|w| w == tag);
    let moov = find(b"moov").expect("moov atom must appear within first 64KiB");
    if let Some(mdat) = find(b"mdat") {
        assert!(
            moov < mdat,
            "moov at {moov} must precede mdat at {mdat} (faststart contract)"
        );
    }
}

#[tokio::test]
async fn transcode_ladder_and_poster_faststart() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(src) = fixture_720(tmp.path()) else {
        return;
    };
    let work = tempfile::tempdir().expect("workdir");
    let outputs = transcode::transcode(&src, work.path())
        .await
        .expect("transcode succeeds");

    let renditions: Vec<&str> = outputs.videos.iter().map(|v| v.rendition).collect();
    assert!(
        renditions.contains(&"720"),
        "128x720 source must yield a 720p rung"
    );
    assert!(
        renditions.contains(&"480"),
        "128x720 source must yield a 480p rung"
    );

    for v in &outputs.videos {
        assert!(v.path.exists(), "{} exists", v.path.display());
        let info = probe::probe(&v.path).await.expect("ffprobe on rendition");
        let expected = if v.rendition == "720" { 720 } else { 480 };
        assert_eq!(info.height, Some(expected), "{} height", v.rendition);
        assert!(info.has_audio, "{} keeps an audio stream", v.rendition);
        assert_faststart(&v.path);
    }

    assert!(outputs.poster_path.exists(), "poster.jpg exists");
    let poster = std::fs::read(&outputs.poster_path).expect("read poster");
    assert!(
        poster.starts_with(&[0xFF, 0xD8]),
        "poster is a JPEG (magic FFD8)"
    );
}

#[tokio::test]
async fn transcode_sub_480_source_scales_up_to_480p() {
    let tmp = tempfile::tempdir().expect("tempdir");
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping small-source transcode test");
        return;
    }
    let src = tmp.path().join("small.mp4");
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc=duration=1:size=64x64:rate=10")
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(&src)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("ffmpeg spawn");
    assert!(status.success());

    let work = tempfile::tempdir().expect("workdir");
    let outputs = transcode::transcode(&src, work.path())
        .await
        .expect("transcode succeeds");
    assert_eq!(
        outputs.videos.len(),
        1,
        "sub-480p source must yield exactly one ladder rung (scaled up to 480p)"
    );
    assert_eq!(outputs.videos[0].rendition, "480");
    let info = probe::probe(&outputs.videos[0].path)
        .await
        .expect("ffprobe on rendition");
    assert_eq!(info.height, Some(480), "sub-480p source scales up to 480p");
    assert_faststart(&outputs.videos[0].path);
    assert!(outputs.poster_path.exists(), "poster still produced");
}

#[tokio::test]
async fn transcode_silent_source_an_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping silent-source transcode test");
        return;
    }
    let src = tmp.path().join("silent.mp4");
    // No audio input, no `-c:a`: exercises the `-an` branch.
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc=duration=1:size=320x240:rate=10")
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(&src)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("ffmpeg spawn");
    assert!(status.success());

    let work = tempfile::tempdir().expect("workdir");
    let outputs = transcode::transcode(&src, work.path())
        .await
        .expect("silent transcode succeeds");
    assert_eq!(
        outputs.videos.len(),
        1,
        "320x240 silent source yields the single 480p rung"
    );
    assert_eq!(outputs.videos[0].rendition, "480");
    let info = probe::probe(&outputs.videos[0].path)
        .await
        .expect("ffprobe on rendition");
    assert_eq!(info.height, Some(480));
    assert!(!info.has_audio, "-an branch must not add an audio stream");
    assert_faststart(&outputs.videos[0].path);
    assert!(outputs.poster_path.exists(), "poster still produced");
}

#[test]
fn byte_range_parsing() {
    use toottok::assets::{parse_byte_range, ByteRange, RangeError, RangeSpec};

    let size = 1000u64;
    let partial = |s, e| RangeSpec::Partial(ByteRange { start: s, end: e });
    assert_eq!(parse_byte_range("bytes=0-99", size), Ok(partial(0, 99)));
    assert_eq!(parse_byte_range("bytes=100-", size), Ok(partial(100, 999)));
    assert_eq!(parse_byte_range("bytes=-100", size), Ok(partial(900, 999)));
    assert_eq!(
        parse_byte_range("bytes=900-5000", size),
        Ok(partial(900, 999))
    );
    assert_eq!(parse_byte_range("bytes=0-0", 1), Ok(partial(0, 0)));

    // F12 edge cases.
    assert_eq!(
        parse_byte_range("bytes=-5000", 200),
        Ok(RangeSpec::Full),
        "suffix longer than the file maps to full-content 200"
    );
    assert_eq!(
        parse_byte_range("bytes=0-", 1),
        Ok(partial(0, 0)),
        "open-ended range on 1-byte file"
    );
    assert_eq!(
        parse_byte_range("bytes=-0", size),
        Err(RangeError),
        "zero-length suffix is unsatisfiable"
    );
    assert_eq!(
        parse_byte_range("BYTES=0-99", size),
        Ok(partial(0, 99)),
        "the bytes= unit prefix is matched case-insensitively"
    );
    assert_eq!(
        parse_byte_range("bytes=-1", 1),
        Ok(RangeSpec::Full),
        "suffix == size is full content"
    );

    for bad in [
        "bytes=1000-",
        "bytes=100-0",
        "bytes=foo",
        "bytes=0-1,3-4",
        "items=0-1",
        "-",
        "bytes=",
    ] {
        assert!(
            parse_byte_range(bad, size).is_err(),
            "{bad} must be rejected"
        );
    }
    assert!(
        parse_byte_range("bytes=0-1", 0).is_err(),
        "empty file ranges rejected"
    );
}

#[tokio::test]
async fn asset_serving_ranges_and_404() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> =
        Arc::new(LocalStore::new(tmp.path().join("media")));

    let actor = make_actor(&pool, "serve").await;
    let clip = Clip::create_local(
        &pool,
        actor.id,
        "https://toot.local/clips/serve",
        Some("<p>hi</p>"),
        "public",
        "ready",
        None,
    )
    .await
    .expect("clip insert");

    let key = format!("renditions/{}/720.mp4", clip.id);
    let data: Vec<u8> = (0..200u8).collect();
    let stored = store.save_bytes(&key, &data).await.expect("store asset");
    MediaAsset::create(
        &pool,
        clip.id,
        "video_mp4",
        "720",
        &key,
        "video/mp4",
        Some(stored.size_bytes as i64),
        None,
        Some("h264"),
    )
    .await
    .expect("asset insert");

    let state = toottok::AppState::test_default(Some(pool.clone()), store);
    let app = toottok::app(state);
    let uri = format!("/assets/{}/720.mp4", clip.id);

    let full = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(
        full.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "video/mp4"
    );
    assert_eq!(
        full.headers()[header::ACCEPT_RANGES].to_str().unwrap(),
        "bytes"
    );
    assert_eq!(
        full.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    let full_body = axum::body::to_bytes(full.into_body(), 1 << 20)
        .await
        .expect("body");
    assert_eq!(full_body.len(), 200);

    // HEAD shares the GET handler: same headers, empty body.
    let head = app
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(&uri)
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        head.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        "200"
    );
    assert_eq!(
        head.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        axum::body::to_bytes(head.into_body(), 1 << 20)
            .await
            .expect("body")
            .len(),
        0
    );

    let range = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=0-99")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes 0-99/200")
    );
    let part = axum::body::to_bytes(range.into_body(), 1 << 20)
        .await
        .expect("body");
    assert_eq!(part.len(), 100);
    assert_eq!(&part[..], &data[..100]);

    let suffix = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=-50")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
    let suffix_body = axum::body::to_bytes(suffix.into_body(), 1 << 20)
        .await
        .expect("body");
    assert_eq!(suffix_body.len(), 50);
    assert_eq!(&suffix_body[..], &data[150..]);

    let bad = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=999999-")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(bad.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        bad.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes */200"
    );

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/assets/{}/nope.mp4", clip.id))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/problem+json"
    );
}

#[tokio::test]
async fn asset_range_edge_cases() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> =
        Arc::new(LocalStore::new(tmp.path().join("media")));

    let actor = make_actor(&pool, "edge").await;
    let clip = Clip::create_local(
        &pool,
        actor.id,
        "https://toot.local/clips/edge",
        None,
        "public",
        "ready",
        None,
    )
    .await
    .expect("clip insert");

    // One 1-byte asset exercises the F12 edge cases.
    let key = format!("renditions/{}/tiny.mp4", clip.id);
    let data = vec![0xABu8];
    let stored = store.save_bytes(&key, &data).await.expect("store asset");
    MediaAsset::create(
        &pool,
        clip.id,
        "video_mp4",
        "720",
        &key,
        "video/mp4",
        Some(stored.size_bytes as i64),
        None,
        Some("h264"),
    )
    .await
    .expect("asset insert");

    let state = toottok::AppState::test_default(Some(pool.clone()), store);
    let app = toottok::app(state);
    let uri = format!("/assets/{}/tiny.mp4", clip.id);

    // bytes=-0 => 416 (zero-length suffix).
    let zero = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=-0")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(
        zero.status(),
        StatusCode::RANGE_NOT_SATISFIABLE,
        "bytes=-0 must be 416"
    );

    // suffix > size => 200 full content (RFC 7233 §2.1 choice, documented).
    let over = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=-5")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(
        over.status(),
        StatusCode::OK,
        "suffix longer than the file serves full 200"
    );
    let over_body = axum::body::to_bytes(over.into_body(), 1 << 20)
        .await
        .expect("body");
    assert_eq!(over_body.len(), 1);
    assert_eq!(&over_body[..], &data);

    // bytes=0- on a 1-byte file => 206, length 1.
    let open = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::RANGE, "bytes=0-")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(open.status(), StatusCode::PARTIAL_CONTENT);
    let open_body = axum::body::to_bytes(open.into_body(), 1 << 20)
        .await
        .expect("body");
    assert_eq!(open_body.len(), 1);
    assert_eq!(&open_body[..], &data);

    // Case-insensitive header name + bytes= prefix (ByTeS=...) => 206.
    let ci = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header("RANGE", "ByTeS=0-0")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(ci.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        ci.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 0-0/1",
        "case-insensitive Range header + unit prefix"
    );
}

#[tokio::test]
async fn clip_metadata_endpoint_lists_assets_and_404s() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> =
        Arc::new(LocalStore::new(tmp.path().join("media")));

    let actor = make_actor(&pool, "meta").await;
    let clip = Clip::create_local(
        &pool,
        actor.id,
        "https://toot.local/clips/meta",
        Some("<p>caption</p>"),
        "public",
        "processing",
        None,
    )
    .await
    .expect("clip insert");

    for (kind, rendition, filename, mime) in [
        ("video_mp4", "720", "720.mp4", "video/mp4"),
        ("video_mp4", "480", "480.mp4", "video/mp4"),
        ("video_mp4", "orig", "orig-uuid.mp4", "video/mp4"),
        ("poster", "orig", "poster.jpg", "image/jpeg"),
    ] {
        let key = format!("renditions/{}/{filename}", clip.id);
        let stored = store.save_bytes(&key, &[0u8; 16]).await.expect("store");
        MediaAsset::create(
            &pool,
            clip.id,
            kind,
            rendition,
            &key,
            mime,
            Some(stored.size_bytes as i64),
            None,
            None,
        )
        .await
        .expect("asset insert");
    }

    let state = toottok::AppState::test_default(Some(pool.clone()), store);
    let app = toottok::app(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/clips/{}", clip.id))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    assert_eq!(json["id"], serde_json::json!(clip.id));
    assert_eq!(json["status"], "processing");
    assert_eq!(json["caption_html"], "<p>caption</p>");
    assert_eq!(json["assets"].as_array().expect("assets array").len(), 4);
    let kinds: Vec<&str> = json["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"video_mp4"));
    assert!(kinds.contains(&"poster"));
    let has_poster_url = json["assets"].as_array().unwrap().iter().any(|a| {
        a["url"]
            .as_str()
            .is_some_and(|u| u.ends_with("/poster.jpg"))
    });
    assert!(has_poster_url, "assets carry a servable url");

    let missing = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/clips/99999999")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/problem+json"
    );
}

/// Drive the full upload -> probe -> transcode -> finalize pipeline through the
/// real server router and the worker's job-processing function.
#[tokio::test]
async fn worker_end_to_end_upload_to_ready() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> =
        Arc::new(LocalStore::new(tmp.path().join("media")));
    let app = toottok::app(toottok::AppState::test_default(
        Some(pool.clone()),
        store.clone(),
    ));

    let Some(fixture) = fixture_720(tmp.path()) else {
        return;
    };
    let data = std::fs::read(&fixture).expect("read fixture");

    let Some((cookie, csrf)) =
        toottok::testutil::register_and_login(&app, "e2e", "password123").await
    else {
        panic!("register+login should succeed");
    };

    let boundary = "toottok-e2e";
    let resp = app
        .clone()
        .oneshot(upload_request(
            boundary,
            multipart_body(boundary, &data),
            &cookie,
            &csrf,
        ))
        .await
        .expect("upload");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    assert_eq!(json["status"], "pending");
    let clip_id = json["clip_id"].as_i64().expect("clip_id");

    let job_timeout = std::time::Duration::from_secs(120);
    let mut stalled = 0;
    loop {
        let clip = Clip::fetch_by_id(&pool, clip_id)
            .await
            .expect("fetch clip")
            .expect("clip exists");
        if clip.status == "ready" {
            break;
        }
        assert_ne!(clip.status, "failed", "pipeline must not fail the clip");
        let Some(job) = Job::claim_next_due(&pool, "e2e-worker")
            .await
            .expect("claim")
        else {
            stalled += 1;
            assert!(stalled < 1000, "pipeline stalled without a claimable job");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        };
        toottok::worker::process_job(&pool, &store, job, job_timeout).await;
    }

    let assets = MediaAsset::fetch_for_clip(&pool, clip_id)
        .await
        .expect("assets");
    let kinds: HashSet<&str> = assets.iter().map(|a| a.kind.as_str()).collect();
    assert!(kinds.contains("video_mp4"), "mp4 rungs registered");
    assert!(kinds.contains("poster"), "poster registered");
    assert!(
        assets
            .iter()
            .any(|a| a.kind == "video_mp4" && a.rendition == "orig"),
        "orig row registered"
    );

    for a in assets
        .iter()
        .filter(|a| a.kind == "video_mp4" && a.rendition != "orig")
    {
        let on_disk = store.open(&a.path).await.expect("asset on disk");
        assert!(on_disk.path.exists(), "{} on disk", a.path);
        assert_eq!(
            on_disk.path.extension().and_then(|e| e.to_str()),
            Some("mp4"),
            "every served mp4 rung is a real mp4 file"
        );
        assert_faststart(&on_disk.path);
    }
}

/// REJECT path: `clip_max_seconds` lowered to 0 ⇒ probe rejects ⇒ clip
/// `failed` and the stored original is deleted from disk.
#[tokio::test]
async fn worker_reject_path_fails_clip_and_deletes_original() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> =
        Arc::new(LocalStore::new(tmp.path().join("media")));
    let app = toottok::app(toottok::AppState::test_default(
        Some(pool.clone()),
        store.clone(),
    ));

    Setting::set(&pool, "clip_max_seconds", &serde_json::json!(0))
        .await
        .expect("set clip_max_seconds to 0");

    let Some(fixture) = fixture_720(tmp.path()) else {
        return;
    };
    let data = std::fs::read(&fixture).expect("read fixture");

    let Some((cookie, csrf)) =
        toottok::testutil::register_and_login(&app, "reject", "password123").await
    else {
        panic!("register+login should succeed");
    };

    let boundary = "toottok-reject";
    let resp = app
        .clone()
        .oneshot(upload_request(
            boundary,
            multipart_body(boundary, &data),
            &cookie,
            &csrf,
        ))
        .await
        .expect("upload");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    let clip_id = json["clip_id"].as_i64().expect("clip_id");

    let job = Job::claim_next_due(&pool, "reject-worker")
        .await
        .expect("claim")
        .expect("probe job is due");
    toottok::worker::process_job(&pool, &store, job, std::time::Duration::from_secs(60)).await;

    let clip = Clip::fetch_by_id(&pool, clip_id)
        .await
        .expect("fetch clip")
        .expect("clip exists");
    assert_eq!(clip.status, "failed", "over-cap clip must be failed");

    let orig_dir = tmp.path().join("media/original");
    let entries = std::fs::read_dir(&orig_dir).expect("original dir exists");
    assert_eq!(
        entries.count(),
        0,
        "rejected original file must be deleted from disk"
    );
}

/// A minimal-but-valid mp4 magic header (ftyp isom) — the upload handler only
/// sniffs magic bytes, so no real ffmpeg/ffprobe is needed to reach the GC.
const GC_UPLOAD_FIXTURE: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2mp41";

/// GC grace/early-registration: an in-flight upload's original survives a GC
/// tick (registered as an `orig` media_assets row at upload time, unready), a
/// fresh unreferenced orphan is spared by the grace window, and a stale
/// (2h-old) orphan is swept.
#[tokio::test]
async fn media_gc_grace_period_protects_fresh_and_sweeps_stale_orphans() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let media_dir = tmp.path().join("media");
    let store: Arc<dyn toottok_media::store::Store> = Arc::new(LocalStore::new(&media_dir));
    let app = toottok::app(toottok::AppState::test_default(
        Some(pool.clone()),
        store.clone(),
    ));

    let Some((cookie, csrf)) =
        toottok::testutil::register_and_login(&app, "gcuser", "password123").await
    else {
        panic!("register+login should succeed");
    };

    // Upload through the real router; the orig asset row must land immediately.
    let boundary = "toottok-gc";
    let resp = app
        .clone()
        .oneshot(upload_request(
            boundary,
            multipart_body(boundary, GC_UPLOAD_FIXTURE),
            &cookie,
            &csrf,
        ))
        .await
        .expect("upload");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    let clip_id = json["clip_id"].as_i64().expect("clip_id");

    let assets = MediaAsset::fetch_for_clip(&pool, clip_id)
        .await
        .expect("assets");
    let orig = assets
        .iter()
        .find(|a| a.kind == "video_mp4" && a.rendition == "orig")
        .expect("orig asset row registered at upload time");
    assert!(
        orig.ready_at.is_none(),
        "orig row stays unready until finalize"
    );
    let original_on_disk = media_dir.join(&orig.path);
    assert!(original_on_disk.exists(), "original file on disk");

    // GC tick: the referenced original survives.
    toottok::worker::media_gc(&pool, &media_dir).await;
    assert!(original_on_disk.exists(), "referenced original survives gc");

    // A fresh unreferenced orphan is kept by the 3600s grace window.
    let fresh_orphan = media_dir.join("original/ghost-fresh.mp4");
    std::fs::create_dir_all(fresh_orphan.parent().expect("parent")).expect("mkdir");
    std::fs::write(&fresh_orphan, b"orphan").expect("write fresh orphan");
    toottok::worker::media_gc(&pool, &media_dir).await;
    assert!(fresh_orphan.exists(), "fresh orphan spared by grace period");

    // Age the orphan to 2h: the next GC tick sweeps it.
    set_mtime_2h_ago(&fresh_orphan);
    toottok::worker::media_gc(&pool, &media_dir).await;
    assert!(!fresh_orphan.exists(), "stale orphan removed by gc");
    assert!(
        original_on_disk.exists(),
        "referenced original survives even after stale sweep"
    );
}

/// N3 guard: a job the reaper already requeued must not be double-bumped by the
/// timed-out worker that used to hold the lock, and its clip must not be failed.
#[tokio::test]
async fn bump_job_after_failure_skips_requeued_job_and_control_still_bumps() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let actor = make_actor(&pool, "bump").await;

    let clip = Clip::create_pending_upload(
        &pool,
        actor.id,
        "https://toot.local/clips/bump-a",
        "deadbeef",
        10,
        None,
    )
    .await
    .expect("clip insert");
    let job = Job::create(
        &pool,
        "probe",
        &serde_json::json!({ "clip_id": clip.id }),
        None,
    )
    .await
    .expect("job insert");
    let claimed = Job::claim_next_due(&pool, "worker-a")
        .await
        .expect("claim")
        .expect("job is due");
    assert_eq!(claimed.state, "running");
    assert_eq!(claimed.locked_by.as_deref(), Some("worker-a"));

    // Reaper requeues the stale lock (bump to attempt 1) before the original
    // worker's timeout fires.
    sqlx::query(
        "UPDATE jobs SET state = 'queued', locked_by = NULL, locked_at = NULL, attempts = 1 WHERE id = $1",
    )
    .bind(job.id)
    .execute(&pool)
    .await
    .expect("simulate reaper");

    // The timed-out worker's bump must be refused: no double bump, clip intact.
    toottok::worker::bump_job_after_failure(&pool, &claimed, Some(clip.id), "job timed out").await;

    let after = Job::fetch_by_id(&pool, job.id)
        .await
        .expect("fetch")
        .expect("job");
    assert_eq!(after.attempts, 1, "requeued job must not be double-bumped");
    assert_eq!(after.state, "queued", "reaper requeue preserved");
    assert!(after.locked_by.is_none());
    let clip_after = Clip::fetch_by_id(&pool, clip.id)
        .await
        .expect("fetch clip")
        .expect("clip exists");
    assert_eq!(
        clip_after.status, "pending",
        "clip not failed by the timed-out worker"
    );

    // Control: a NEW worker reclaims the requeued job, then times out too. Its
    // bump is legit (it holds the lock) and it fails the clip.
    let re_claimed = Job::claim_next_due(&pool, "worker-b")
        .await
        .expect("claim")
        .expect("requeued job reclaimable");
    assert_eq!(re_claimed.id, job.id);
    toottok::worker::bump_job_after_failure(&pool, &re_claimed, Some(clip.id), "job timed out")
        .await;

    let after = Job::fetch_by_id(&pool, job.id)
        .await
        .expect("fetch")
        .expect("job");
    assert_eq!(after.attempts, 2, "owning worker bumps the attempt");
    assert_eq!(after.state, "queued");
    let clip_after = Clip::fetch_by_id(&pool, clip.id)
        .await
        .expect("fetch clip")
        .expect("clip exists");
    assert_eq!(clip_after.status, "failed", "owning worker fails the clip");
}

/// N4: an unknown job kind is dead-lettered AND flips the clip to `failed`
/// when the payload carries a clip_id.
#[tokio::test]
async fn unknown_job_kind_fails_clip_and_dead_letters() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> =
        Arc::new(LocalStore::new(tmp.path().join("media")));

    let actor = make_actor(&pool, "unk").await;
    let clip = Clip::create_pending_upload(
        &pool,
        actor.id,
        "https://toot.local/clips/unk",
        "12345678",
        10,
        None,
    )
    .await
    .expect("clip insert");
    let job = Job::create(
        &pool,
        "nonsense",
        &serde_json::json!({ "clip_id": clip.id }),
        None,
    )
    .await
    .expect("job insert");
    let claimed = Job::claim_next_due(&pool, "unk-worker")
        .await
        .expect("claim")
        .expect("job is due");

    toottok::worker::process_job(&pool, &store, claimed, std::time::Duration::from_secs(60)).await;

    let after = Job::fetch_by_id(&pool, job.id)
        .await
        .expect("fetch")
        .expect("job");
    assert_eq!(after.state, "dead");
    assert_eq!(
        after.last_error.as_deref(),
        Some("unknown job kind: nonsense")
    );
    let clip_after = Clip::fetch_by_id(&pool, clip.id)
        .await
        .expect("fetch clip")
        .expect("clip exists");
    assert_eq!(clip_after.status, "failed", "unknown-kind clip is failed");
}

/// Rewind a file's mtime by two hours (no filetime dependency; std only).
fn set_mtime_2h_ago(path: &Path) {
    let file = std::fs::File::open(path).expect("open for mtime");
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    file.set_modified(two_hours_ago).expect("set mtime");
}

fn multipart_body(boundary: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"clip.mp4\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: video/mp4\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn upload_request(boundary: &str, body: Vec<u8>, cookie: &str, csrf: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/clips/upload")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, cookie)
        .header("x-toottok-csrf", csrf)
        .body(Body::from(body))
        .expect("valid request")
}
