use clap::{Arg, Command};

use bytes::Bytes;
use log::warn;
use renet::{
    transport::{ClientAuthentication, ICERequest, NetcodeClientTransport, NetcodeTransportError},
    ConnectionConfig, DefaultChannel, RenetClient,
};
use reqwest::{Client as HttpClient, Response as HttpResponse};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    sync::Arc,
    thread,
    time::{Duration, SystemTime},
};
use tokio::time::sleep;
use webrtc::{
    api::{setting_engine::SettingEngine, APIBuilder},
    data::data_channel::DataChannel,
    data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, RTCDataChannel},
    dtls_transport::dtls_role::DTLSRole,
    error::Error as RTCError,
    ice::mdns::MulticastDnsMode,
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription,
    },
    sdp::description::session::ATTR_KEY_CANDIDATE,
};

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
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    // let client_id: u64 = 0;
    let client_id = current_time.as_millis() as u64;
    let username = Username("Nick".to_string());
    let authentication = ClientAuthentication::Unsecure {
        server_addr: SERVER_ADDR,
        client_id,
        user_data: Some(username.to_netcode_user_data()),
        protocol_id: PROTOCOL_ID,
    };

    log::info!("creating renet transport client using id {}", client_id);

    let (tx, rx) = mpsc::sync_channel(1);
    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket, tx.clone())
        .await
        .unwrap();
    let offer = transport.create_offer().await.expect("offer creation failed");

    let http_client = HttpClient::new();
    let payload = match serde_json::to_string(&ICERequest::new(offer.sdp.clone(), client_id)) {
        Ok(payload) => payload,
        Err(e) => panic!("failed to serialize ICE request: {}", e),
    };

    let response: HttpResponse = loop {
        let request = http_client
            .post(server_url.clone())
            .header("content-type", "application/json; charset=utf-8")
            .body(payload.clone());
        // .header("Content-Length", offer.sdp.len())
        // .body(offer.sdp.clone());

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
    transport.set_answer(response_string).await.expect("setting answer failed");

    println!("#### Renet transport layer built");

    // loop {}

    let mut packet_seq = 0;

    loop {
        // log::debug!("transport.is_connected {}", transport.is_connected());
        // log::debug!("transport.is_connecting {}", transport.is_connecting());
        // log::debug!("transport.is_disconnected {}", transport.is_disconnected());

        let delta_time = Duration::from_millis(16);
        // Receive new messages and update client
        client.update(delta_time);
        if let Err(e) = transport.update(delta_time, &mut client, &rx).await {
            log::error!("transport update error {e}");
            break;
        };

        if transport.is_connected() {
            // Receive message from server
            while let Some(message) = client.receive_message(DefaultChannel::ReliableOrdered) {
                // Handle received message
                let msg_str = String::from_utf8(message.to_vec()).expect("received message should be parsable");
                println!("Received from server: {msg_str}");
            }

            // Send message
            if packet_seq % 50 == 0 {
                let message = format!("{}_{}", SLICE_MESSAGE, packet_seq);
                println!("Sending '{message}'");
                client.send_message(DefaultChannel::ReliableOrdered, message.as_bytes().to_vec());
            }
            if packet_seq == 30 {
                transport.disconnect().await;
                // break
            }
            packet_seq += 1;
        }

        // Send packets to server
        if transport.is_data_channel_open() {
            match transport.send_packets(&mut client).await {
                Ok(()) => {}
                Err(e) => {
                    println!("sending error {e}")
                }
            };
        }

        thread::sleep(Duration::from_millis(50));
    }

    log::info!("Closing data channel");
    transport.close_rtc().await?;

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
