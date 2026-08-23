//! Phase 3 wave A tests: probe parsing, reject-path decision, magic-byte
//! sniffing, and upload dedup (axum oneshot against the server router + a real
//! Postgres via the same `TOOTTOK_TEST_DB` harness the db crate uses).
//!
//! Fixture-dependent tests (real ffprobe/ffmpeg) print a note and return early
//! when the ffmpeg binary is missing. The dedup integration test is the only
//! one that needs Postgres; it panics loudly on setup failure unless
//! `TOOTTOK_TEST_SKIP=1`.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use toottok_db::clip::Clip;
use toottok_db::job::Job;
use toottok_media::probe::{self, ProbeInfo};
use toottok_media::store::LocalStore;
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
                eprintln!(
                    "toottok-media test setup failed ({e}); TOOTTOK_TEST_SKIP=1 set, skipping"
                );
                None
            } else {
                panic!("toottok-media test setup failed: {e}");
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

/// Generate a tiny 1s 64x64 mp4 with ffmpeg. `None` when ffmpeg is unavailable.
fn fixture_mp4(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping fixture-dependent probe test");
        return None;
    }
    let path = dir.join("fixture.mp4");
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc=duration=1:size=64x64:rate=10")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(path)
}

#[tokio::test]
async fn probe_parses_real_fixture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(path) = fixture_mp4(tmp.path()) else {
        return;
    };

    let info = probe::probe(&path)
        .await
        .expect("ffprobe should parse the fixture");
    let duration = info.duration_s.expect("fixture has a duration");
    assert!(
        (duration - 1.0).abs() < 0.3,
        "expected ~1s, got {duration}s"
    );
    assert_eq!(info.width, Some(64));
    assert_eq!(info.height, Some(64));
}

#[test]
fn decide_rejects_oversized_and_undecodable() {
    let over_cap = ProbeInfo {
        duration_s: Some(200.0),
        width: Some(64),
        height: Some(64),
        has_audio: false,
    };
    assert!(matches!(
        probe::decide(&over_cap, 180.0),
        probe::ProbeDecision::Reject(_)
    ));

    let within_cap = ProbeInfo {
        duration_s: Some(10.0),
        width: Some(64),
        height: Some(64),
        has_audio: false,
    };
    assert!(matches!(
        probe::decide(&within_cap, 180.0),
        probe::ProbeDecision::Accept
    ));

    let no_duration = ProbeInfo::default();
    assert!(matches!(
        probe::decide(&no_duration, 180.0),
        probe::ProbeDecision::Reject(_)
    ));

    let negative = ProbeInfo {
        duration_s: Some(-1.0),
        ..ProbeInfo::default()
    };
    assert!(matches!(
        probe::decide(&negative, 180.0),
        probe::ProbeDecision::Reject(_)
    ));
}

#[test]
fn magic_bytes_sniffing() {
    let mp4 = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2mp41";
    let container = probe::sniff_container(mp4).expect("mp4 ftyp accepted");
    assert_eq!(container.ext, "mp4");
    assert_eq!(container.mime, "video/mp4");

    let mov = b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00qt  ";
    let container = probe::sniff_container(mov).expect("mov ftyp accepted");
    assert_eq!(container.ext, "mov");

    let webm = b"\x1a\x45\xdf\xa3\x01\x00\x00\x00\x00\x00\x00\x00\x1f\x43\xb6\x75";
    let container = probe::sniff_container(webm).expect("webm EBML accepted");
    assert_eq!(container.ext, "webm");
    assert_eq!(container.mime, "video/webm");

    for garbage in [&b""[..], b"not-a-video", b"\x00\x00\x00\x00garbage"] {
        assert!(
            probe::sniff_container(garbage).is_err(),
            "garbage header must be rejected"
        );
    }
}

/// A minimal-but-valid mp4 magic header (ftyp isom), no ffmpeg needed. The
/// upload handler only sniffs magic bytes; probing is the worker's job.
const DEDUP_FIXTURE: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2mp41";

#[tokio::test]
async fn upload_dedup_rejects_duplicate_bytes() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(LocalStore::new(tmp.path().join("media")));
    let state = toottok::AppState::test_default(Some(pool.clone()), store);
    let app = toottok::app(state);

    let Some((cookie, csrf)) =
        toottok::testutil::register_and_login(&app, "dedup", "password123").await
    else {
        panic!("register+login should succeed");
    };

    let boundary = "toottok-test-boundary";
    let body = multipart_body(boundary, DEDUP_FIXTURE);

    let first = app
        .clone()
        .oneshot(upload_request(boundary, body.clone(), &cookie, &csrf))
        .await
        .expect("oneshot request");
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .expect("created response is json");
    assert_eq!(first_json["status"], "pending");
    assert!(first_json["clip_id"].is_i64());
    let clip_id = first_json["clip_id"].as_i64().unwrap();

    let second = app
        .oneshot(upload_request(boundary, body, &cookie, &csrf))
        .await
        .expect("oneshot request");
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "identical bytes must be deduped"
    );

    let clip = Clip::fetch_by_id(&pool, clip_id)
        .await
        .expect("fetch clip")
        .expect("clip exists");
    assert_eq!(clip.status, "pending");
    assert_eq!(clip.origin, "local");
    assert_eq!(clip.size_bytes, Some(DEDUP_FIXTURE.len() as i64));

    // Attribution: the upload belongs to the authed actor, not a system actor.
    let actor = toottok_db::actor::Actor::fetch_by_username_local(&pool, "dedup")
        .await
        .expect("fetch actor")
        .expect("actor exists");
    assert_eq!(
        clip.actor_id, actor.id,
        "clip is attributed to the uploader"
    );

    let probe_job = Job::fetch_by_id(&pool, first_job_id(&pool).await).await;
    let job = probe_job.expect("job fetch").expect("probe job exists");
    assert_eq!(job.kind, "probe");
    assert_eq!(job.payload["clip_id"], serde_json::json!(clip_id));

    let stored = std::fs::read_dir(tmp.path().join("media/original")).expect("original dir exists");
    assert_eq!(stored.count(), 1, "exactly one stored original");
}

async fn first_job_id(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT id FROM jobs ORDER BY id LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("job row")
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
