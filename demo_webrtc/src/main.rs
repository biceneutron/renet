use clap::{Arg, Command};
use log::warn;
use renet::{
    transport::{ClientAuthentication, NetcodeClientTransport, NetcodeTransportError, SignalingResponse, Str0mClient},
    ConnectionConfig, DefaultChannel, RenetClient,
};
use reqwest::{Client as HttpClient, Response as HttpResponse};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::mpsc::{self},
    thread,
    time::{Duration, Instant, SystemTime},
};
use tokio::time::sleep;

use rand::Rng;
use str0m::change::SdpAnswer;
use str0m::{Candidate, Rtc};

const PROTOCOL_ID: u64 = 7;
const NETCODE_USER_DATA_BYTES: usize = 256;

const SERVER_URL: &str = "http://127.0.0.1:8080";
const SERVER_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1111);

const SMALL_MESSAGE: &str = "CLIENT_PACKET";
const SLICE_MESSAGE: &str =
    "CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET";

#[tokio::main]
async fn main() -> Result<(), NetcodeTransportError> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let local_addr = Ipv4Addr::new(127, 0, 0, 1);
    let udp_port = rand::thread_rng().gen_range(50000..=65535);
    let udp_socket_addr = SocketAddr::from((local_addr.octets(), udp_port));
    let socket = UdpSocket::bind(udp_socket_addr).unwrap();
    let username = Username("Nick".to_string());

    // webrtc
    let (rtc, client_id) = create_rtc(udp_socket_addr).await;

    // renet
    let authentication = ClientAuthentication::Unsecure {
        server_addr: SERVER_ADDR,
        client_id,
        user_data: Some(username.to_netcode_user_data()),
        protocol_id: PROTOCOL_ID,
    };
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let mut client = RenetClient::new(ConnectionConfig::default());
    let str0m_client = Str0mClient::new(client_id, rtc);
    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket, str0m_client).unwrap();

    let mut last_updated = Instant::now();
    let mut packet_seq = 0;

    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        // Receive new messages and update client
        client.update(duration);
        if let Err(e) = transport.update(duration, &mut client) {
            log::error!("transport update error {e}");
            break;
        };

        // custom-logic
        if transport.is_connected() {
            // Receive message from server
            while let Some(message) = client.receive_message(DefaultChannel::Unreliable) {
                // Handle received message
                let msg_str = String::from_utf8(message.to_vec()).unwrap();
                log::info!("Received user data: {msg_str}");
            }

            // Send message
            if packet_seq <= 200 && packet_seq % 10 == 0 {
                let message = format!("{}_{}", SLICE_MESSAGE, packet_seq);
                log::info!("Sending user data '{message}'");
                client.send_message(DefaultChannel::Unreliable, message.as_bytes().to_vec());
            }
            if packet_seq == 230 {
                transport.disconnect();
            }
            packet_seq += 1;
        }

        // Send packets to server
        if transport.is_data_channel_open() {
            match transport.send_packets(&mut client) {
                Ok(()) => {}
                Err(e) => {
                    log::error!("Renet failed sending: {e}")
                }
            };
        }

        thread::sleep(Duration::from_millis(50));
    }

    transport.close_rtc();
    log::info!("Str0m is closed");

    Ok(())
}

async fn create_rtc(local_addr: SocketAddr) -> (Rtc, u64) {
    let mut rtc = Rtc::new();
    let local_candidate = match Candidate::host(local_addr) {
        Ok(c) => c,
        Err(e) => panic!("Str0m failed creating local candidate: {}", e),
    };
    rtc.add_local_candidate(local_candidate);

    let mut api = rtc.sdp_api();
    let _ = api.add_channel("data".into());
    let (offer, pending) = api.apply().unwrap();
    let offer_string = offer.to_sdp_string();

    let http_client = HttpClient::new();
    let response: HttpResponse = loop {
        let request = http_client
            .post(SERVER_URL)
            .header("Content-Length", offer_string.len())
            .body(offer_string.clone());

        match request.send().await {
            Ok(resp) => {
                break resp;
            }
            Err(e) => {
                warn!("Failed sending HTTP request: {:?}", e);
                // sleep(Duration::from_secs(1)).await;
                thread::sleep(Duration::from_millis(500));
            }
        };
    };
    let sig_res: SignalingResponse = serde_json::from_str(&response.text().await.unwrap()).unwrap();

    let answer = match SdpAnswer::from_sdp_string(&sig_res.sdp) {
        Ok(a) => a,
        Err(e) => panic!("Str0m failed parsing answer SDP: {}", e),
    };
    if let Err(e) = rtc.sdp_api().accept_answer(pending, answer) {
        panic!("Str0m failed accepting answer SDP: {}", e);
    };

    (rtc, sig_res.client_id)
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
