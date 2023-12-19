use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing;
use axum::Router;
use axum_extra::extract::TypedHeader;
use axum_extra::headers::{authorization::Bearer, Authorization};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::api::API;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtcp::header::PacketType;
use webrtc::rtcp::sender_report::SenderReport;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

pub struct RTCState {
    api: API,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .format(|buf, record| writeln!(buf, "[{}] {}", record.level(), record.args()))
        .filter(None, log::LevelFilter::Error)
        .init();

    // prepare webrtc media engine and api-builder
    let mut m = MediaEngine::default();
    m.register_default_codecs().unwrap();
    let mut r = Registry::new();
    r = register_default_interceptors(r, &mut m).unwrap();
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(r)
        .build();

    // create axum application
    let state = Arc::new(RTCState { api });
    let app = Router::new()
        .route("/", routing::post(handle_offer))
        .with_state(state);

    println!(">>> running");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

pub async fn handle_offer(
    State(state): State<Arc<RTCState>>,
    TypedHeader(authorization): TypedHeader<Authorization<Bearer>>,
    body: String,
) -> impl IntoResponse {
    println!(">>> handle_offer");
    // get parameters from request
    let player_id = authorization.token().to_owned();
    let offer = body;

    // create peer connection
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let peer_connection = Arc::new(state.api.new_peer_connection(config).await.unwrap());

    // on track receive
    let peer_connection2 = peer_connection.clone();
    peer_connection.on_track(Box::new(move |track, receiver, _| {
        let peer_connection = peer_connection2.clone();
        println!(">>> on_track {:?}", Instant::now());
        Box::pin(async move {
            println!(">>> track timestamp {:?}", Instant::now());

            let rtcp = receiver.read_rtcp().await;
            match rtcp {
                Ok((packets, _)) => match packets.len() {
                    0 => (),
                    _ => {
                        let packet = packets[0].clone();
                        let header = packet.header();
                        match header.packet_type {
                            PacketType::SenderReport => {
                                let packet = packet.clone();
                                let packet =
                                    packet.as_any().downcast_ref::<SenderReport>().unwrap();
                                println!(">>> report timestamp {:?}", Instant::now());
                                println!(
                                    ">>> sender report {} {}",
                                    packet.ntp_time, packet.rtp_time
                                );
                            }
                            _ => (),
                        }
                    }
                },
                Err(_) => (),
            };

            let _ = peer_connection.get_stats().await;
            println!(">>> get stats {:?}", Instant::now());

            tokio::spawn(async move {
                let _ = track.read_rtp().await;
                println!(">>> first rtp packet {:?}", Instant::now());
                loop {
                    let rtp = track.read_rtp().await;
                    match rtp {
                        Ok(_) => (),
                        Err(_) => (),
                    }
                }
            });
        })
    }));

    peer_connection.on_peer_connection_state_change(Box::new(move |_| Box::pin(async move {})));

    // prepare answer
    let description = RTCSessionDescription::offer(offer.clone()).unwrap();
    peer_connection
        .set_remote_description(description)
        .await
        .unwrap();

    // prepare media recievers
    peer_connection
        .add_transceiver_from_kind(RTPCodecType::Audio, None)
        .await
        .unwrap();
    peer_connection
        .add_transceiver_from_kind(RTPCodecType::Video, None)
        .await
        .unwrap();

    // gather ICE candidates
    let answer = peer_connection.create_answer(None).await.unwrap();
    let mut gather_complete = peer_connection.gathering_complete_promise().await;
    peer_connection
        .set_local_description(answer.clone())
        .await
        .unwrap();
    let _ = gather_complete.recv().await;
    let local_description = peer_connection.local_description().await.unwrap();

    // prepare headers
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("application/sdp"));
    headers.insert("Location", HeaderValue::from_static("/"));

    // println!("{}", player_id);
    // println!();
    // println!("{}", offer);
    // println!();
    // println!("{}", local_description.sdp);

    (StatusCode::CREATED, headers, local_description.sdp)
}
