use std::env;
use std::process::Command;
use std::sync::Arc;

use axum::routing;
use axum::Router;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use dotenvy::dotenv;

mod handler;
mod util;

pub enum AppEvent {
    Created(sqlx::types::Uuid, String, String, DateTime<Utc>),
    Recording(sqlx::types::Uuid, String, String, DateTime<Utc>),
    Completed(sqlx::types::Uuid, String, String, DateTime<Utc>, Duration),
}

pub struct RTCState {
    tx: tokio::sync::mpsc::Sender<AppEvent>,
}

#[tokio::main]
async fn main() {
    // TODO: dotenv shit
    let _ = dotenv();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AppEvent>(100);
    tokio::spawn(async move {
        let _ = std::fs::create_dir(env::var("GETREC_TMP_PATH").unwrap());
        let postgres_url = env::var("DATABASE_URL").unwrap();
        let postgres = sqlx::postgres::PgPoolOptions::new()
            .max_connections(24)
            .connect(&postgres_url)
            .await
            .unwrap();
        // let s3_region =
        //     aws_config::meta::region::RegionProviderChain::default_provider().or_else("auto");
        // let s3_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        //     .region(s3_region)
        //     .endpoint_url(env::var("AWS_ENDPOINT_URL").unwrap())
        //     .load()
        //     .await;
        let s3_config = aws_config::from_env()
            .endpoint_url(env::var("S3_URL").unwrap())
            .load()
            .await;
        let s3 = aws_sdk_s3::Client::new(&s3_config);

        while let Some(event) = rx.recv().await {
            match event {
                // upsert everything
                AppEvent::Created(recording_id, player_id, account_id, timestamp) => {
                    println!(
                        "{:} [{:}] {:} {:} {:}",
                        Utc::now().to_rfc3339(),
                        "CREATED",
                        &account_id,
                        &player_id,
                        &recording_id
                    );
                    let _ = sqlx::query("
                        insert into recordings (recording_id, player_id, account_id, created_tstamp, state)
                        values ($1, $2, $3, $4::timestamptz, $5)
                        on conflict (recording_id)
                        do update set player_id = $2, account_id = $3, created_tstamp = $4::timestamptz, state = $5
                    ").bind(&recording_id)
                    .bind(&player_id)
                    .bind(&account_id)
                    .bind(timestamp.to_rfc3339())
                    .bind("CREATED")
                    .fetch_optional(&postgres)
                    .await;
                }
                AppEvent::Recording(recording_id, player_id, account_id, timestamp) => {
                    println!(
                        "{:} [{:}] {:} {:} {:}",
                        Utc::now().to_rfc3339(),
                        "RECORDING",
                        &account_id,
                        &player_id,
                        &recording_id
                    );
                    let _ = sqlx::query("
                        insert into recordings (recording_id, player_id, account_id, start_tstamp, state)
                        values ($1, $2, $3, $4::timestamptz, $5)
                        on conflict (recording_id)
                        do update set player_id = $2, account_id = $3, start_tstamp = $4::timestamptz, state = $5
                    ").bind(&recording_id)
                    .bind(&player_id)
                    .bind(&account_id)
                    .bind(timestamp.to_rfc3339())
                    .bind("RECORDING")
                    .fetch_optional(&postgres)
                    .await;
                }
                AppEvent::Completed(
                    recording_id,
                    player_id,
                    account_id,
                    timestamp,
                    audio_offset,
                ) => {
                    // TODO: handle missing files baby.
                    println!(
                        "{:} [{:}] {:} {:} {:}",
                        Utc::now().to_rfc3339(),
                        "PROCESSING",
                        &account_id,
                        &player_id,
                        &recording_id
                    );
                    let _ = sqlx::query("
                        insert into recordings (recording_id, player_id, account_id, start_tstamp, state)
                        values ($1, $2, $3, $4::timestamptz, $5)
                        on conflict (recording_id)
                        do update set player_id = $2, account_id = $3, start_tstamp = $4::timestamptz, state = $5
                    ").bind(&recording_id)
                    .bind(&player_id)
                    .bind(&account_id)
                    .bind(timestamp.to_rfc3339())
                    .bind("PROCESSING")
                    .fetch_optional(&postgres)
                    .await;

                    let (opus_path, h264_path, mp4_path, r2_path) =
                        util::get_paths(&recording_id.to_string());

                    // let’s ffmpeg
                    let _ = Command::new("ffmpeg")
                        .args([
                            "-y",
                            "-fflags",
                            "+genpts",
                            "-r",
                            "30",
                            "-i",
                            h264_path.as_str(),
                            "-ss",
                            format!("{}ms", audio_offset.num_milliseconds()).as_str(),
                            "-i",
                            opus_path.as_str(),
                            "-map",
                            "0:v",
                            "-map",
                            "1:a",
                            "-c:v",
                            "copy",
                            "-c:a",
                            "copy",
                            mp4_path.as_str(),
                        ])
                        .output();

                    // let’s probe it!
                    let probe = Command::new("ffprobe")
                        .args([
                            "-i",
                            mp4_path.as_str(),
                            "-show_entries",
                            "format=duration",
                            "-of",
                            "csv=p=0",
                        ])
                        .output()
                        .unwrap();
                    let duration_seconds = String::from_utf8(probe.stdout).unwrap();
                    let duration_seconds = duration_seconds.trim();
                    let duration_seconds = duration_seconds.parse::<f32>().unwrap();
                    let end_timestamp =
                        timestamp + Duration::milliseconds((duration_seconds * 1000.0) as i64);

                    // let’s upload it
                    let body = aws_sdk_s3::primitives::ByteStream::from_path(mp4_path.as_str())
                        .await
                        .unwrap();
                    let _ = &s3
                        .put_object()
                        .bucket(env::var("S3_BUCKET").unwrap())
                        .key(r2_path.as_str())
                        .content_type("video/mp4")
                        .body(body)
                        .send()
                        .await;

                    // let’s update it
                    println!(
                        "{:} [{:}] {:} {:} {:}",
                        Utc::now().to_rfc3339(),
                        "COMPLETED",
                        &account_id,
                        &player_id,
                        &recording_id
                    );
                    let _ = sqlx::query("
                        insert into recordings (recording_id, player_id, account_id, start_tstamp, end_tstamp, state)
                        values ($1, $2, $3, $4::timestamptz, $5::timestamptz, $6)
                        on conflict (recording_id)
                        do update set player_id = $2, account_id = $3, start_tstamp = $4::timestamptz, end_tstamp = $5::timestamptz, state = $6
                    ").bind(&recording_id)
                    .bind(&player_id)
                    .bind(&account_id)
                    .bind(timestamp.to_rfc3339())
                    .bind(end_timestamp.to_rfc3339())
                    .bind("COMPLETED")
                    .fetch_optional(&postgres)
                    .await;

                    // let’s clean up
                    if env::var("GETREC_CLEAN_RAW_FILES")
                        .unwrap_or(String::from("true"))
                        .parse::<bool>()
                        .unwrap()
                    {
                        let _ = std::fs::remove_file(opus_path.as_str());
                        let _ = std::fs::remove_file(h264_path.as_str());
                    }
                    if env::var("GETREC_CLEAN_PACKAGED_FILES")
                        .unwrap_or(String::from("true"))
                        .parse::<bool>()
                        .unwrap()
                    {
                        let _ = std::fs::remove_file(mp4_path.as_str());
                    }
                }
            }
        }
    });

    let state = Arc::new(RTCState { tx });
    let app = Router::new()
        .route("/v1/:account_id", routing::post(handler::handle_offer))
        .with_state(state);
    let address = format!("{:}:{:}", "0.0.0.0", env::var("GETREC_PORT").unwrap());
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
