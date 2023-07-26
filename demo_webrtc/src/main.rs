use clap::{Arg, Command};

use bytes::Bytes;
use log::warn;
use renet::{
    transport::{ClientAuthentication, NetcodeClientTransport},
    ConnectionConfig, DefaultChannel, RenetClient,
};
use reqwest::{Client as HttpClient, Response as HttpResponse};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::Arc,
    thread,
    time::{Duration, SystemTime},
};
use tokio::{sync::mpsc, time::sleep};
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

#[tokio::main]
async fn main() -> Result<(), RTCError> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("debug"));

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

    const SERVER_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3952);
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let client_id: u64 = 0;
    let authentication = ClientAuthentication::Unsecure {
        server_addr: SERVER_ADDR,
        client_id,
        user_data: None,
        protocol_id: 0,
    };

    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket).await.unwrap();
    let offer = transport.create_offer().await.expect("offer creation failed");

    let payload = match serde_json::to_string(&offer) {
        Ok(p) => p,
        Err(err) => panic!("{}", err),
    };

    let http_client = HttpClient::new();
    let response: HttpResponse = loop {
        // let request = http_client
        //     .post(server_url.clone())
        //     .header("Content-Length", offer.len())
        //     .body(offer.clone());

        let request = http_client
            .post(server_url.clone())
            .header("content-type", "application/json; charset=utf-8")
            .body(payload.clone());

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

    log::debug!("transport.is_connected {}", transport.is_connected());
    log::debug!("transport.is_connecting {}", transport.is_connecting());
    log::debug!("transport.is_disconnected {}", transport.is_disconnected());

    loop {}

    let mut packet_seq = 0;
    loop {
        let delta_time = Duration::from_millis(16);
        // Receive new messages and update client
        client.update(delta_time);
        transport
            .update(delta_time, &mut client)
            .map_err(|e| log::error!("transport update error {e}"))
            .unwrap();

        if transport.is_connected() {
            // Receive message from server
            while let Some(message) = client.receive_message(DefaultChannel::ReliableOrdered) {
                // Handle received message
                let msg_str = String::from_utf8(message.to_vec()).expect("received message should be parsable");
                println!("Received from server: {msg_str}");
            }

            // Send message
            if packet_seq % 100 == 0 {
                let message = format!("CLIENT_PACKET_{}", packet_seq);
                println!("Sending '{message}'");
                client.send_message(DefaultChannel::ReliableOrdered, message.as_bytes().to_vec());
            }
            packet_seq += 1;
        }

        // Send packets to server
        match transport.send_packets(&mut client).await {
            Ok(()) => {}
            Err(e) => {
                println!("sending error {e}")
            }
        };
        thread::sleep(Duration::from_millis(50));
    }

    // let timeout = tokio::time::sleep(Duration::from_secs(5));
    // tokio::pin!(timeout);

    // tokio::select! {
    //     _ = timeout.as_mut() =>{
    //         let mut packet_seq = 0;
    //         loop {
    //             let delta_time = Duration::from_millis(16);
    //             // Receive new messages and update client
    //             client.update(delta_time);
    //             transport.update(delta_time, &mut client).map_err(|e| log::error!("transport update error {e}")).unwrap();

    //             if let Some(e) = client.disconnect_reason() {
    //                 log::error!("{}", e);
    //             } else {
    //                 // Receive message from server
    //                 while let Some(message) = client.receive_message(DefaultChannel::ReliableOrdered) {
    //                     // Handle received message
    //                     let msg_str = String::from_utf8(message.to_vec()).expect("received message should be parsable");
    //                     println!("Received from server: {msg_str}");
    //                 }

    //                 // Send message
    //                 if packet_seq % 100 == 0 {
    //                     let message = format!("CLIENT_PACKET_{}", packet_seq);
    //                     println!("Sending '{message}'");
    //                     client.send_message(DefaultChannel::ReliableOrdered, message.as_bytes().to_vec());
    //                 }
    //                 packet_seq += 1;
    //             }

    //             // Send packets to server
    //             match transport.send_packets(&mut client).await {
    //                 Ok(()) => {},
    //                 Err(e) => {println!("sending error {e}")}
    //             };
    //         }
    //     }
    // };
}

// fn create_renet_client(username: String, server_addr: SocketAddr) -> (RenetClient, NetcodeClientTransport) {
//     let connection_config = ConnectionConfig::default();
//     let client = RenetClient::new(connection_config);

//     let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
//     let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
//     let client_id = current_time.as_millis() as u64;
//     let authentication = ClientAuthentication::Unsecure {
//         server_addr,
//         client_id,
//         user_data: Some(Username(username).to_netcode_user_data()),
//         protocol_id: PROTOCOL_ID,
//     };

//     let transport = NetcodeClientTransport::new(current_time, authentication, socket).unwrap();

//     (client, transport)
// }

const MESSAGE_SIZE: usize = 1500;

// async fn read_loop(
//     data_channel: Arc<DataChannel>,
//     to_client_sender: mpsc::UnboundedSender<Box<[u8]>>,
// ) -> Result<(), RTCError> {
//     let mut buffer = vec![0u8; MESSAGE_SIZE];
//     loop {
//         let message_length = match data_channel.read(&mut buffer).await {
//             Ok(length) => length,
//             Err(err) => {
//                 println!("Datachannel closed; Exit the read_loop: {}", err);
//                 return Ok(());
//             }
//         };

//         match to_client_sender.send(buffer[..message_length].into()) {
//             Ok(_) => {}
//             Err(e) => {
//                 return Err(RTCError::new(e.to_string()));
//             }
//         }
//     }
// }

use std::str;
async fn read_loop(data_channel: Arc<DataChannel>, to_client_sender: mpsc::UnboundedSender<Box<[u8]>>) -> Result<(), RTCError> {
    let mut buffer = vec![0u8; MESSAGE_SIZE];
    loop {
        println!("#### read_loop ####");
        let message_length = match data_channel.read(&mut buffer).await {
            Ok(length) => length,
            Err(err) => {
                println!("Datachannel closed; Exit the read_loop: {}", err);
                return Ok(());
            }
        };

        if let Ok(message) = String::from_utf8(buffer[..message_length].to_vec()) {
            println!("Received from server, {} bytes: {}", message_length, message);
        } else {
            println!("Received from server, {} bytes, but cannot parse it", message_length);
        }
    }
    println!("#### end read_loop ####");
}

// write_loop shows how to write to the datachannel directly
// async fn write_loop(
//     data_channel: Arc<DataChannel>,
//     mut to_server_receiver: mpsc::UnboundedReceiver<Box<[u8]>>,
// ) -> Result<(), RTCError> {
//     loop {
//         if let Some(write_message) = to_server_receiver.recv().await {
//             match data_channel.write(&Bytes::from(write_message)).await {
//                 Ok(_) => {}
//                 Err(e) => {
//                     return Err(RTCError::new(e.to_string()));
//                 }
//             }
//         } else {
//             return Ok(());
//         }
//     }
// }

async fn write_loop(data_channel: Arc<DataChannel>, mut to_server_receiver: mpsc::UnboundedReceiver<Box<[u8]>>) -> Result<(), RTCError> {
    let mut packet_seq = 0;
    loop {
        println!("#### write_loop ####");
        if packet_seq < 10 {
            match data_channel.write(&Bytes::from("CLIENT_PACKET")).await {
                Ok(_) => {
                    packet_seq += 1;
                }
                Err(e) => {
                    return Err(RTCError::new(e.to_string()));
                }
            }
        } else {
            return Ok(());
        }
    }
    println!("#### write_loop ####");
}

struct SessionAnswer {
    pub sdp: String,
    pub type_str: String,
}

struct SessionCandidate {
    pub candidate: String,
    pub sdp_m_line_index: u16,
    pub sdp_mid: String,
}

struct JsSessionResponse {
    pub(crate) answer: SessionAnswer,
    pub(crate) candidate: SessionCandidate,
}

use tinyjson::JsonValue;
fn get_session_response(input: &str) -> JsSessionResponse {
    let json_obj: JsonValue = input.parse().unwrap();

    let sdp_opt: Option<&String> = json_obj["answer"]["sdp"].get();
    let sdp: String = sdp_opt.unwrap().clone();

    let type_str_opt: Option<&String> = json_obj["answer"]["type"].get();
    let type_str: String = type_str_opt.unwrap().clone();

    let candidate_opt: Option<&String> = json_obj["candidate"]["candidate"].get();
    let candidate: String = candidate_opt.unwrap().clone();

    let sdp_m_line_index_opt: Option<&f64> = json_obj["candidate"]["sdpMLineIndex"].get();
    let sdp_m_line_index: u16 = *(sdp_m_line_index_opt.unwrap()) as u16;

    let sdp_mid_opt: Option<&String> = json_obj["candidate"]["sdpMid"].get();
    let sdp_mid: String = sdp_mid_opt.unwrap().clone();

    JsSessionResponse {
        answer: SessionAnswer { sdp, type_str },
        candidate: SessionCandidate {
            candidate,
            sdp_m_line_index,
            sdp_mid,
        },
    }
}
