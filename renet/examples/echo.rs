use renet::{
    transport::{
        ClientAuthentication, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication, ServerConfig, SignalingResponse,
        Str0mClient, NETCODE_USER_DATA_BYTES,
    },
    ConnectionConfig, DefaultChannel, RenetClient, RenetServer, ServerEvent,
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use rand::Rng;
use reqwest::{Client as HttpClient, Response as HttpResponse};
use rouille::Server;
use rouille::{Request, Response};
use std::io::Read;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::{Candidate, Rtc};

const SERVER_URL: &str = "http://127.0.0.1:8080";
const SERVER_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1111);

const SMALL_MESSAGE: &str = "CLIENT_PACKET";
const SLICE_MESSAGE: &str =
    "CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET_CLIENT_PACKET";

// Helper struct to pass an username in the user data
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

#[tokio::main]
async fn main() {
    println!("Usage: server [SERVER_PORT] or client [SERVER_PORT] [USER_NAME]");
    let args: Vec<String> = std::env::args().collect();

    let exec_type = &args[1];
    match exec_type.as_str() {
        "client" => {
            let username = Username(args[2].clone());
            client(username).await;
        }
        "server" => {
            let server_addr: SocketAddr = format!("127.0.0.1:{}", args[2]).parse().unwrap();
            server(server_addr);
        }
        _ => {
            println!("Invalid argument, first one must be \"client\" or \"server\".");
        }
    }
}

const PROTOCOL_ID: u64 = 7;

async fn client(username: Username) {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let local_addr = Ipv4Addr::new(127, 0, 0, 1);
    let udp_port = rand::thread_rng().gen_range(50000..=65535);
    let udp_socket_addr = SocketAddr::from((local_addr.octets(), udp_port));
    let socket = UdpSocket::bind(udp_socket_addr).unwrap();

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
    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket, Str0mClient::new(client_id, rtc)).unwrap();

    let mut last_updated = Instant::now();
    let mut packet_seq = 0;

    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        // Receive new messages and update client
        client.update(duration);
        if let Err(e) = transport.update(duration, &mut client) {
            log::warn!("Failed updating transport layer: {e}");
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
            if packet_seq == 220 {
                transport.disconnect();
            }
            packet_seq += 1;
        }

        // Send packets to server
        if transport.is_data_channel_open() {
            match transport.send_packets(&mut client) {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("Renet failed sending: {e}")
                }
            };
        }

        thread::sleep(Duration::from_millis(50));
    }

    transport.close_rtc();
    log::info!("Str0m is closed");
}

fn server(public_addr: SocketAddr) {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let connection_config = ConnectionConfig::default();
    let mut server: RenetServer = RenetServer::new(connection_config);

    let socket: UdpSocket = UdpSocket::bind(public_addr).unwrap();
    let addr = socket.local_addr().expect("a local socket adddress");
    let server_config = ServerConfig {
        max_clients: 64,
        protocol_id: PROTOCOL_ID,
        public_addr,
        authentication: ServerAuthentication::Unsecure,
    };
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let mut transport = NetcodeServerTransport::new(current_time, server_config, socket).unwrap();

    let mut usernames: HashMap<u64, String> = HashMap::new();
    let mut last_updated = Instant::now();

    let (tx, rx) = mpsc::sync_channel(1);

    // HTTP server
    thread::spawn(move || {
        let server = Server::new("127.0.0.1:8080", move |request| web_request(request, addr, tx.clone())).expect("starting the web server");

        let port = server.server_addr().port();
        log::info!("Connect a browser to https://{:?}:{:?}", addr.ip(), port);

        server.run();
    });

    let mut packet_seq = 0;
    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        transport.spawn_new_client(&rx);

        server.update(duration);
        transport.update(duration, &mut server).unwrap();

        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    let user_data = transport.user_data(client_id).unwrap();
                    let username = Username::from_user_data(&user_data);
                    usernames.insert(client_id, username.0);
                    log::info!("Client {} connected.", client_id)
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    log::info!("Client {} disconnected: {}", client_id, reason);
                    usernames.remove_entry(&client_id);
                }
            }
        }

        // custom logic
        for client_id in server.clients_id() {
            while let Some(message) = server.receive_message(client_id, DefaultChannel::Unreliable) {
                let text = String::from_utf8(message.into()).unwrap();
                let username = usernames.get(&client_id).unwrap();
                log::info!("Client {} ({}) sent text: {}", username, client_id, text);

                log::info!("Echoing back {}", text);
                server.send_message(
                    client_id,
                    DefaultChannel::Unreliable,
                    format!("{}_{}", username, text).as_bytes().to_vec(),
                );
            }

            // if packet_seq <= 200 && packet_seq % 10 == 0 {
            //     let text = format!("{}_{}", SLICE_MESSAGE, packet_seq);
            //     println!("Sending {}", text);
            //     server.send_message(client_id, DefaultChannel::Unreliable, text.as_bytes().to_vec());
            // }
            // packet_seq += 1;
        }

        // for text in received_messages.iter() {
        //     server.broadcast_message(DefaultChannel::ReliableOrdered, text.as_bytes().to_vec());
        // }

        transport.send_packets(&mut server);
        thread::sleep(Duration::from_millis(50));
    }
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
                log::warn!("Failed sending HTTP request: {:?}", e);
                thread::sleep(Duration::from_millis(1000));
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

fn web_request(request: &Request, addr: SocketAddr, tx: SyncSender<(u64, Rtc)>) -> Response {
    log::info!("HTTP server got one request");

    static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let client_id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);

    if request.method() == "GET" {
        return Response::empty_204();
    }

    // Expected POST SDP Offers.
    let mut data = request.data().expect("body to be available");

    let mut req_data = String::new();
    if let Some(e) = data.read_to_string(&mut req_data).err() {
        panic!("error parsing HTTP request {}", e);
    }

    let offer = match SdpOffer::from_sdp_string(&req_data) {
        Ok(sdp) => sdp,
        Err(e) => {
            log::error!("Failed to parse SDP offer: {}", e);
            return Response::empty_400();
        }
    };

    let mut rtc = Rtc::builder()
        // Uncomment this to see statistics
        // .set_stats_interval(Some(Duration::from_secs(1)))
        // .set_ice_lite(true)
        .build();

    // Add the shared UDP socket as a host candidate
    let candidate = Candidate::host(addr).expect("a host candidate");
    rtc.add_local_candidate(candidate);

    // Create an SDP Answer.
    let answer = rtc.sdp_api().accept_offer(offer).expect("offer to be accepted");

    // The Rtc instance is shipped off to the main run loop.
    tx.send((client_id, rtc)).expect("to send Rtc instance");

    let response = match serde_json::to_string(&SignalingResponse::new(answer.to_sdp_string(), client_id)) {
        Ok(res) => res,
        Err(e) => return Response::text(format!("Server failed creating answer SDP: {}", e)).with_status_code(500),
    };

    Response::text(response)
}
