use renet::{
    transport::{ICEResponse, NetcodeServerTransport, ServerAuthentication, ServerConfig, NETCODE_USER_DATA_BYTES},
    ConnectionConfig, DefaultChannel, RenetServer, ServerEvent,
};
use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use rouille::Server;
use rouille::{Request, Response};
use std::io::Read;
use str0m::change::SdpOffer;
use str0m::{Candidate, Rtc};

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

fn main() {
    // env_logger::init();
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("debug"));
    println!("Usage: server [SERVER_PORT] or client [SERVER_PORT] [USER_NAME]");
    let args: Vec<String> = std::env::args().collect();

    let exec_type = &args[1];
    match exec_type.as_str() {
        "client" => {
            // let server_addr: SocketAddr = format!("127.0.0.1:{}", args[2]).parse().unwrap();
            // let username = Username(args[3].clone());
            // client(server_addr, username);
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

fn server(public_addr: SocketAddr) {
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
    // let mut received_messages = vec![];
    let mut last_updated = Instant::now();

    let (tx, rx) = mpsc::sync_channel(1);

    // HTTP server
    thread::spawn(move || {
        let server = Server::new(
            "127.0.0.1:8080",
            move |request| web_request(request, addr, tx.clone()),
            // certificate,
            // private_key,
        )
        .expect("starting the web server");

        let port = server.server_addr().port();
        log::info!("Connect a browser to https://{:?}:{:?}", addr.ip(), port);

        server.run();
    });

    loop {
        // println!("#### num of str0m clients: {}", transport.get_num_str0mclients());

        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        transport.spawn_new_client(&rx);

        server.update(duration);
        transport.update(duration, &mut server).unwrap(); // receive msg

        // received_messages.clear();

        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    let user_data = transport.user_data(client_id).unwrap();
                    let username = Username::from_user_data(&user_data);
                    usernames.insert(client_id, username.0);
                    println!("Client {} connected.", client_id)
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    println!("Client {} disconnected: {}", client_id, reason);
                    usernames.remove_entry(&client_id);
                }
            }
        }

        // custom logic
        for client_id in server.clients_id() {
            while let Some(message) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
                let text = String::from_utf8(message.into()).unwrap();
                let username = usernames.get(&client_id).unwrap();
                println!("Client {} ({}) sent text: {}", username, client_id, text);
                // let text = format!("{}: {}", username, text);
                // received_messages.push(text);

                println!("Echoing back {}", text);
                server.send_message(client_id, DefaultChannel::ReliableOrdered, text.as_bytes().to_vec());
            }
        }

        // for text in received_messages.iter() {
        //     server.broadcast_message(DefaultChannel::ReliableOrdered, text.as_bytes().to_vec());
        // }

        transport.send_packets(&mut server);
        thread::sleep(Duration::from_millis(50));
    }
}

fn web_request(request: &Request, addr: SocketAddr, tx: SyncSender<(u64, Rtc)>) -> Response {
    println!("Got web_request");

    static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let client_id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    println!("using client id {}", client_id);

    if request.method() == "GET" {
        return Response::empty_204();
    }

    // Expected POST SDP Offers.
    let mut data = request.data().expect("body to be available");

    let mut req_data = String::new();
    if let Some(e) = data.read_to_string(&mut req_data).err() {
        panic!("error parsing HTTP request {}", e);
    }

    // println!("offer sdp {}", req_data);

    let offer = match SdpOffer::from_sdp_string(&req_data) {
        Ok(sdp) => sdp,
        Err(e) => return Response::empty_400(),
    };

    // let offer: SdpOffer = serde_json::from_reader(&mut data).expect("serialized offer");
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

    println!("showing the source in the first place...");
    rtc.show_send_addr();

    // The Rtc instance is shipped off to the main run loop.
    tx.send((client_id, rtc)).expect("to send Rtc instance");

    // let body = serde_json::to_vec(&answer).expect("answer to serialize");
    let response = match serde_json::to_string(&ICEResponse::new(answer.to_sdp_string(), client_id)) {
        Ok(res) => res,
        Err(e) => return Response::text(format!("Server failed creating answer SDP: {}", e)).with_status_code(500),
    };

    Response::text(response)
}
