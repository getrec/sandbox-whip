use std::fs::File;
use std::io::ErrorKind;
use std::io::Write;
use std::net::UdpSocket;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::util;
use crate::AppEvent;
use crate::RTCState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum_extra::headers::authorization::Bearer;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use chrono::Utc;
use str0m::change::SdpOffer;
use str0m::net::Protocol;
use str0m::net::Receive;
use str0m::Candidate;
use str0m::Event;
use str0m::IceConnectionState;
use str0m::Input;
use str0m::Output;
use str0m::Rtc;
use str0m::RtcConfig;
use str0m::RtcError;
use stunclient::StunClient;

pub async fn handle_offer(
    State(state): State<Arc<RTCState>>,
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
    Path(account_id): Path<String>,
    body: String,
) -> impl IntoResponse {
    let state = state.clone();
    let tx = state.tx.clone();
    let recording_id = sqlx::types::Uuid::new_v4();
    let player_id = authorization.token().to_owned();
    let account_id = account_id.clone();
    let offer = SdpOffer::from_sdp_string(body.as_str()).expect("parse offer SDP");

    let mut rtc = RtcConfig::new()
        .clear_codecs()
        .set_reordering_size_video(240);
    let config = rtc.codec_config();
    config.enable_h264(true);
    config.enable_opus(true);
    let mut rtc = rtc.build();

    let local_address = util::select_host_address();
    let socket = UdpSocket::bind(format!("{local_address}:0")).expect("hello random UDP port");
    let candidate =
        Candidate::host(socket.local_addr().unwrap(), "udp").expect("hello local candidate");
    rtc.add_local_candidate(candidate);

    // TODO: I’m not sure if our STUN candidate actually works
    let client = StunClient::with_google_stun_server();
    let external_address = client.query_external_address(&socket).unwrap();
    let candidate = Candidate::server_reflexive(external_address, "udp").unwrap();
    rtc.add_local_candidate(candidate);

    let answer = rtc
        .sdp_api()
        .accept_offer(offer)
        .expect("offer to be accepted")
        .to_sdp_string();

    thread::spawn(move || run((recording_id, player_id, account_id), rtc, socket, tx));

    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("application/sdp"));
    headers.insert("Location", HeaderValue::from_static("/"));

    (StatusCode::CREATED, headers, answer)
}

fn run(
    (recording_id, player_id, account_id): (sqlx::types::Uuid, String, String),
    mut rtc: Rtc,
    socket: UdpSocket,
    tx: tokio::sync::mpsc::Sender<AppEvent>,
) -> Result<(), RtcError> {
    // packet/sample buffer
    let mut buf = Vec::new();
    // timings
    let start_ts = Utc::now();
    let mut added_ts = Utc::now();
    let mut keyframe_ts = Utc::now();

    // writers
    let (opus_path, h264_path, _, _) = util::get_paths(&recording_id.to_string());
    let mut opus_writer = File::create(opus_path.as_str()).unwrap();
    let mut h264_writer = File::create(h264_path.as_str()).unwrap();

    // state
    // state: h264
    let mut has_keyframe = false;
    let mut last_keyframe = vec![];
    // state: opus
    let mut page_index = 0;
    let mut previous_timestamp: i64 = 1;
    let mut previous_granule_position: i64 = 1;

    // opus: id header page
    let page = util::get_opus_id_page(page_index);
    page_index += 1;
    let _ = opus_writer.write_all(&page);
    // opus: comment header page
    let page = util::get_opus_comment_page(page_index);
    page_index += 1;
    let _ = opus_writer.write_all(&page);

    loop {
        let timeout = match rtc.poll_output()? {
            Output::Timeout(d) => d,
            Output::Transmit(d) => {
                socket.send_to(&d.contents, d.destination)?;
                continue;
            }
            Output::Event(d) => {
                match d {
                    Event::IceConnectionStateChange(state) => {
                        println!(
                            "{:} [{:?}] {:} {:} {:}",
                            Utc::now().to_rfc3339(),
                            state,
                            &account_id,
                            &player_id,
                            &recording_id
                        );
                        match state {
                            IceConnectionState::Checking => {}
                            IceConnectionState::Connected => {
                                let _ = tx.blocking_send(AppEvent::Created(
                                    recording_id.clone(),
                                    player_id.clone(),
                                    account_id.clone(),
                                    start_ts,
                                ));
                            }
                            IceConnectionState::Disconnected => {
                                let _ = tx.blocking_send(AppEvent::Completed(
                                    recording_id.clone(),
                                    player_id.clone(),
                                    account_id.clone(),
                                    keyframe_ts,
                                    keyframe_ts - added_ts,
                                ));
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                    Event::MediaAdded(_) => {
                        added_ts = Utc::now();
                    }
                    Event::MediaData(d) => {
                        let kind = match d.pt.to_ascii_lowercase() {
                            111 => "audio",
                            _ => "video",
                        };
                        match kind {
                            "audio" => {
                                if previous_timestamp != 1 {
                                    let increment = d.time.numer() - previous_timestamp;
                                    previous_granule_position += increment;
                                }
                                previous_timestamp = d.time.numer();

                                // opus: data page
                                let page = util::get_opus_page(
                                    &d.data,
                                    util::PAGE_HEADER_TYPE_CONTINUATION_OF_STREAM,
                                    previous_granule_position as u64,
                                    page_index,
                                );
                                page_index += 1;
                                let _ = opus_writer.write_all(&page);
                            }
                            "video" => {
                                let is_keyframe = util::get_is_key_frame(&d.data[4..8])
                                    || util::get_is_key_frame(&d.data[10..14]);
                                if !has_keyframe {
                                    // whip-go: 4..8
                                    // obs: 10..14
                                    has_keyframe = is_keyframe;
                                    if has_keyframe {
                                        // first keyframe
                                        keyframe_ts = Utc::now();
                                        last_keyframe = d.data.clone();
                                        let _ = tx.blocking_send(AppEvent::Recording(
                                            recording_id.clone(),
                                            player_id.clone(),
                                            account_id.clone(),
                                            Utc::now(),
                                        ));
                                    }
                                }
                                if has_keyframe {
                                    if is_keyframe {
                                        last_keyframe = d.data.clone();
                                    }
                                    match d.contiguous {
                                        true => {
                                            let _ = h264_writer.write_all(&d.data);
                                        }
                                        false => {
                                            // insert last keyframe to compensate for the missing frame
                                            let _ = h264_writer.write_all(&last_keyframe);
                                            let _ = h264_writer.write_all(&d.data);
                                        }
                                    }
                                }
                            }
                            _ => (),
                        }
                    }
                    _ => (),
                }
                continue;
            }
        };

        let timeout = timeout - Instant::now();

        if timeout.is_zero() {
            rtc.handle_input(Input::Timeout(Instant::now()))?;
            continue;
        }

        socket.set_read_timeout(Some(timeout))?;
        buf.resize(2000, 0);

        let input = match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                buf.truncate(n);
                Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: socket.local_addr().unwrap(),
                        contents: buf.as_slice().try_into()?,
                    },
                )
            }
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Input::Timeout(Instant::now()),
                _ => return Err(e.into()),
            },
        };

        rtc.handle_input(input)?;
    }
}
