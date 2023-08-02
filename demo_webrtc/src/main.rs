use clap::{Arg, Command};

use log::warn;
use renet::{
    transport::{ClientAuthentication, ICEResponse, NetcodeClientTransport, NetcodeTransportError, Str0mClient},
    ConnectionConfig, DefaultChannel, RenetClient,
};
use reqwest::{Client as HttpClient, Response as HttpResponse};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::mpsc::{self},
    sync::Arc,
    thread,
    time::{Duration, SystemTime},
};
use tokio::time::sleep;
use webrtc::{
    api::{setting_engine::SettingEngine, APIBuilder},
    data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage},
    dtls_transport::dtls_role::DTLSRole,
    ice::mdns::MulticastDnsMode,
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription,
    },
    sdp::description::session::ATTR_KEY_CANDIDATE,
};

use rand::Rng;
use str0m::change::SdpAnswer;
use str0m::{Candidate, Rtc};

const PROTOCOL_ID: u64 = 7;
const NETCODE_USER_DATA_BYTES: usize = 256;

const SMALL_MESSAGE: &str = "CLIENT_PACKET";
const SLICE_MESSAGE: &str =
    "CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET";

#[tokio::main]
async fn main() -> Result<(), NetcodeTransportError> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let args: Vec<String> = std::env::args().collect();
    let exec_type = &args[1];
    // println!("exec_type {}", exec_type);

    let matches: clap::ArgMatches = Command::new("app")
        .arg(
            Arg::new("server")
                .short('s')
                .long("server")
                .takes_value(true)
                .required(false)
                .help("server address"),
        )
        .get_matches();

    let server_url: String = matches
        .value_of("server")
        .unwrap()
        .parse()
        .expect("could not parse server data address/port");

    let mut client = RenetClient::new(ConnectionConfig::default());

    const SERVER_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1111);
    let local_addr = Ipv4Addr::new(127, 0, 0, 1);
    let udp_port = rand::thread_rng().gen_range(50000..=65535);
    let udp_socket_addr = SocketAddr::from((local_addr.octets(), udp_port));
    let socket = UdpSocket::bind(udp_socket_addr).unwrap();
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let username = Username("Nick".to_string());
    let (tx, rx) = mpsc::sync_channel(1);

    // webrtc
    let mut rtc = Rtc::new();
    let local_candidate = match Candidate::host(udp_socket_addr) {
        Ok(c) => c,
        Err(e) => panic!("Str0m cannot create local candidate: {}", e),
    };
    rtc.add_local_candidate(local_candidate);

    let mut api = rtc.sdp_api();
    let cid = api.add_channel("data".into());
    let (offer, pending) = api.apply().unwrap();
    let offer_string = offer.to_sdp_string();

    let http_client = HttpClient::new();
    let response: HttpResponse = loop {
        let request = http_client
            .post(server_url.clone())
            // .header("content-type", "application/json; charset=utf-8")
            // .body(payload.clone());
            .header("Content-Length", offer_string.len())
            .body(offer_string.clone());

        match request.send().await {
            Ok(resp) => {
                break resp;
            }
            Err(err) => {
                warn!("Could not send request, original error: {:?}", err);
                sleep(Duration::from_secs(1)).await;
            }
        };
    };
    let response_string = response.text().await.unwrap();
    println!("response_string {}", response_string);

    let ice_response: ICEResponse = match serde_json::from_str(&response_string) {
        Ok(res) => res,
        Err(e) => panic!("failed parsing ICE response: {}", e),
    };

    let answer = match SdpAnswer::from_sdp_string(&ice_response.sdp) {
        Ok(a) => a,
        Err(e) => panic!("Str0m failed parsing answer SDP: {}", e),
    };
    if let Err(e) = rtc.sdp_api().accept_answer(pending, answer) {
        panic!("Str0m failed accepting answer SDP: {}", e);
    };

    // renet
    let authentication = ClientAuthentication::Unsecure {
        server_addr: SERVER_ADDR,
        client_id: ice_response.client_id,
        user_data: Some(username.to_netcode_user_data()),
        protocol_id: PROTOCOL_ID,
    };

    let str0m_client = Str0mClient::new(ice_response.client_id, rtc);
    log::info!("creating renet transport client using id {}", ice_response.client_id);

    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket, str0m_client).unwrap();

    println!("#### Renet transport layer built");

    let mut packet_seq = 0;

    loop {
        // log::debug!("transport.is_connected {}", transport.is_connected());
        // log::debug!("transport.is_connecting {}", transport.is_connecting());
        // log::debug!("transport.is_disconnected {}", transport.is_disconnected());

        let delta_time = Duration::from_millis(16);

        // Receive new messages and update client
        client.update(delta_time);
        if let Err(e) = transport.update(delta_time, &mut client, &rx) {
            log::error!("transport update error {e}");
            break;
        };

        // custom-logic
        if transport.is_connected() {
            // Receive message from server
            while let Some(message) = client.receive_message(DefaultChannel::ReliableOrdered) {
                // Handle received message
                let msg_str = String::from_utf8(message.to_vec()).expect("received message should be parsable");
                println!("Received from server: {msg_str}");
            }

            // Send message
            if packet_seq <= 100 && packet_seq % 10 == 0 {
                let message = format!("{}_{}", SLICE_MESSAGE, packet_seq);
                println!("Sending '{message}'");
                client.send_message(DefaultChannel::ReliableOrdered, message.as_bytes().to_vec());
            }
            // if packet_seq == 30 {
            //     transport.disconnect();
            //     // break
            // }
            packet_seq += 1;
        }

        // Send packets to server
        if transport.is_data_channel_open() {
            match transport.send_packets(&mut client) {
                Ok(()) => {}
                Err(e) => {
                    println!("sending error {e}")
                }
            };
        }

        thread::sleep(Duration::from_millis(50));
    }

    // #TODO handle str0m closing

    log::info!("Closing str0m");
    transport.close_rtc();

    log::info!("Disconnected, program terminated");
    Ok(())
}

struct Username(String);

impl Username {
    fn to_netcode_user_data(&self) -> [u8; NETCODE_USER_DATA_BYTES] {
        let mut user_data = [0u8; NETCODE_USER_DATA_BYTES];
        if self.0.len() > NETCODE_USER_DATA_BYTES - 8 {
            panic!("Username is too big");
        }
        user_data[0..8].copy_from_slice(&(self.0.len() as u64).to_le_bytes());
        user_data[8..self.0.len() + 8].copy_from_slice(self.0.as_bytes());

        user_data
    }

    fn from_user_data(user_data: &[u8; NETCODE_USER_DATA_BYTES]) -> Self {
        let mut buffer = [0u8; 8];
        buffer.copy_from_slice(&user_data[0..8]);
        let mut len = u64::from_le_bytes(buffer) as usize;
        len = len.min(NETCODE_USER_DATA_BYTES - 8);
        let data = user_data[8..len + 8].to_vec();
        let username = String::from_utf8(data).unwrap();
        Self(username)
    }
}
