use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use base64::{prelude::BASE64_STANDARD, Engine};
// use rand::Rng;

use wasm_bindgen::prelude::*;
cfg_if::cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        // wasm
        use renet::{
            transport_webrtc::{
                ClientAuthentication, NetcodeClientTransport, RtcHandler,
                NETCODE_USER_DATA_BYTES,
            },
            ConnectionConfig, DefaultChannel, RenetClient
        };
        use wasm_bindgen_futures::JsFuture;
        use web_sys::{
            MessageEvent, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelState, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
            RtcSessionDescriptionInit,
        };
    } else {
        // native
        use renet::{
            transport_webrtc::{
                ClientAuthentication, NetcodeClientTransport, NetcodeServerTransport, RtcHandler, ServerAuthentication, ServerConfig, Str0mClient,
                NETCODE_USER_DATA_BYTES,
            },
            ConnectionConfig, DefaultChannel, RenetClient, RenetServer, ServerEvent,
        };
        use str0m::change::{SdpAnswer, SdpOffer};
        use str0m::{Candidate, Rtc};
    }
}

// wasm
// #[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

macro_rules! console_log {
    // Note that this is using the `log` function imported above during
    // `bare_bones`
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

#[wasm_bindgen]
pub fn main() {
    println!("Usage: server [SERVER_PORT] or client [SERVER_PORT] [USER_NAME]");
    let args: Vec<String> = std::env::args().collect();

    let exec_type = &args[1];
    match exec_type.as_str() {
        "server" => {
            let server_addr: SocketAddr = format!("127.0.0.1:{}", args[2]).parse().unwrap();
            server(server_addr);
        }
        "client" => {
            let server_addr: SocketAddr = format!("127.0.0.1:{}", args[2]).parse().unwrap();
            let username = Username(args[3].clone());
            client(server_addr, username);
        }
        _ => {
            println!("Invalid argument, first one must be \"client\" or \"server\".");
        }
    }
}

const PROTOCOL_ID: u64 = 7;

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
    let mut received_messages = vec![];
    let mut last_updated = Instant::now();

    let (tx, rx) = mpsc::sync_channel(1);

    // handle terminal input
    thread::spawn(move || {
        loop {
            // wait for answer SDP to be pasted
            let mut offer_intput = String::new();
            if let Err(e) = std::io::stdin().read_line(&mut offer_intput) {
                panic!("Failed reading SDP from terminal: {}", e);
            };
            offer_intput = offer_intput.trim().to_owned();
            let offer = String::from_utf8(decode(offer_intput).expect("Stdin input should be decoded"))
                .expect("Decoded offer should be converted into String");

            create_server_rtc(offer, addr, tx.clone());
        }
    });

    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        transport.spawn_new_client(&rx);

        server.update(duration);
        transport.update(duration, &mut server).unwrap();

        received_messages.clear();

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

                let text = format!("{}: {}", username, text);
                received_messages.push(text);
            }
        }

        for text in received_messages.iter() {
            server.broadcast_message(DefaultChannel::Unreliable, text.as_bytes().to_vec());
        }

        transport.send_packets(&mut server);
        thread::sleep(Duration::from_millis(50));
    }
}

fn create_server_rtc(offer: String, addr: SocketAddr, tx: SyncSender<(u64, Rtc)>) {
    static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let client_id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);

    let offer = match SdpOffer::from_sdp_string(&offer) {
        Ok(sdp) => sdp,
        Err(e) => {
            log::error!("Failed to parse SDP offer: {}", e);
            return;
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

    println!("\nPaste this SDP to the client terminal:");
    println!("{}\n", BASE64_STANDARD.encode(answer.to_sdp_string()));
    println!("\nPaste this client id to the client terminal:");
    println!("{}\n", client_id);

    // The Rtc instance is shipped off to the main run loop.
    tx.send((client_id, rtc)).expect("to send Rtc instance");
}

fn client(server_addr: SocketAddr, username: Username) {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let local_addr = Ipv4Addr::new(127, 0, 0, 1);
    // let udp_port = rand::thread_rng().gen_range(50000..=65535);
    let udp_port = 51234;
    let udp_socket_addr = SocketAddr::from((local_addr.octets(), udp_port));
    let socket = UdpSocket::bind(udp_socket_addr).unwrap();

    // webrtc
    //native
    #[cfg(not(target_arch = "wasm32"))]
    let (rtc_handler, client_id) = create_client_rtc(udp_socket_addr);

    // wasm
    #[cfg(target_arch = "wasm32")]
    let (tx, rx) = mpsc::sync_channel(1);
    #[cfg(target_arch = "wasm32")]
    let (rtc_handler, client_id) = create_client_rtc(udp_socket_addr, tx);

    // renet
    let authentication = ClientAuthentication::Unsecure {
        server_addr,
        client_id,
        user_data: Some(username.to_netcode_user_data()),
        protocol_id: PROTOCOL_ID,
    };
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let mut client = RenetClient::new(ConnectionConfig::default());
    let mut transport = NetcodeClientTransport::new(current_time, authentication, socket, rtc_handler).unwrap();

    let stdin_channel: Receiver<String> = spawn_stdin_channel();
    let mut last_updated = Instant::now();

    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        client.update(duration);
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = transport.update(duration, &mut client) {
            log::warn!("Failed updating transport layer: {e}");
            break;
        };

        #[cfg(target_arch = "wasm32")]
        if let Err(e) = transport.update(duration, &mut client, &rx) {
            log::warn!("Failed updating transport layer: {e}");
            break;
        };

        if transport.is_connected() {
            match stdin_channel.try_recv() {
                Ok(text) => client.send_message(DefaultChannel::Unreliable, text.as_bytes().to_vec()),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => panic!("Channel disconnected"),
            }

            while let Some(text) = client.receive_message(DefaultChannel::Unreliable) {
                let text = String::from_utf8(text.into()).unwrap();
                log::info!("Received user data: {}", text);
            }
        }

        if transport.is_data_channel_open() {
            match transport.send_packets(&mut client) {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("Renet failed sending: {}", e)
                }
            };
        }

        thread::sleep(Duration::from_millis(50));
    }

    transport.close_rtc();
    log::info!("Str0m is closed");
}

// native
#[cfg(not(target_arch = "wasm32"))]
fn create_client_rtc(local_addr: SocketAddr) -> (RtcHandler, u64) {
    let mut rtc = Rtc::new();
    let local_candidate = match Candidate::host(local_addr) {
        Ok(c) => c,
        Err(e) => panic!("Str0m failed creating local candidate: {}", e),
    };
    rtc.add_local_candidate(local_candidate);

    let mut api = rtc.sdp_api();
    let _ = api.add_channel("data".to_string());
    let (offer, pending) = api.apply().unwrap();

    println!("\nPaste this SDP to the server terminal:");
    println!("{}\n", BASE64_STANDARD.encode(offer.to_sdp_string()));

    // wait for answer SDP to be pasted
    let mut answer_intput = String::new();
    if let Err(e) = std::io::stdin().read_line(&mut answer_intput) {
        panic!("Failed reading SDP from terminal: {}", e);
    };
    answer_intput = answer_intput.trim().to_owned();
    let answer_string = String::from_utf8(decode(answer_intput).expect("Stdin input should be decoded"))
        .expect("Decoded answer should be converted into String");

    // wait for client id to be pasted
    let mut client_id_intput = String::new();
    if let Err(e) = std::io::stdin().read_line(&mut client_id_intput) {
        panic!("Failed reading SDP from terminal: {}", e);
    };
    let client_id = match client_id_intput.trim().to_owned().parse::<u64>() {
        Ok(id) => id,
        Err(e) => panic!("Failed parsing id from stdin to u64: {}", e),
    };

    let answer = match SdpAnswer::from_sdp_string(&answer_string) {
        Ok(a) => a,
        Err(e) => panic!("Str0m failed parsing answer SDP: {}", e),
    };
    if let Err(e) = rtc.sdp_api().accept_answer(pending, answer) {
        panic!("Str0m failed accepting answer SDP: {}", e);
    };

    (RtcHandler::new(Str0mClient::new(client_id, rtc)), client_id)
}

// wasm
#[cfg(target_arch = "wasm32")]
fn create_client_rtc(local_addr: SocketAddr, tx: SyncSender<Vec<u8>>) -> (RtcHandler, u64) {
    let peer_connection = match RtcPeerConnection::new() {
        Ok(pc) => pc,
        Err(e) => panic!("Failed creating peer connection: {:?}", e),
    };
    let data_channel = peer_connection.create_data_channel("data");

    let onmessage_callback = Closure::wrap(Box::new(move |evt: MessageEvent| match evt.data().as_string() {
        Some(data) => {
            log::info!("on_message callback: received length: {}, data: {}", data.len(), data);
            tx.send(data.into_bytes()).expect("to send Rtc instance");
        }
        None => {}
    }) as Box<dyn FnMut(MessageEvent)>);
    data_channel.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
    onmessage_callback.forget();

    // offer
    let peer_connection_2 = peer_connection.clone();
    let create_offer_func: Box<dyn FnMut(JsValue)> = Box::new(move |e: JsValue| {
        let offer: RtcSessionDescriptionInit = e.into();
        match offer.as_string() {
            Some(o) => {
                console_log!("\nPaste this SDP to the server terminal:");
                console_log!("{}\n", BASE64_STANDARD.encode(o));
            }
            None => panic!("Offer SDP should be parsed to a String"),
        }

        // peer_connection_2.set_local_description(&offer).then(&peer_desc_callback);
    });
    let create_offer_callback = Closure::wrap(create_offer_func);
    peer_connection.create_offer().then(&create_offer_callback);

    (RtcHandler::new(peer_connection, data_channel), 0)
}

fn spawn_stdin_channel() -> Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || loop {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer).unwrap();
        tx.send(buffer.trim_end().to_string()).unwrap();
    });
    rx
}

fn decode(encoded: String) -> Result<Vec<u8>, base64::DecodeError> {
    let decoded = BASE64_STANDARD.decode(encoded)?;
    Ok(decoded)
}

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
