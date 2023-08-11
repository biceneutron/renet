cfg_if::cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, SystemTime},
};

use base64::{prelude::BASE64_STANDARD, Engine};
// use rand::Rng;

use wasm_bindgen::prelude::*;

// wasm
use renet::{
    transport_webrtc::{ClientAuthentication, NetcodeClientTransport, RtcHandler, NETCODE_USER_DATA_BYTES},
    ConnectionConfig, DefaultChannel, RenetClient,
};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Event, MessageEvent, ProgressEvent, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelState, RtcIceCandidate, RtcIceCandidateInit,
    RtcPeerConnection, RtcPeerConnectionIceEvent, RtcPeerConnectionState, RtcSdpType, RtcSessionDescription, RtcSessionDescriptionInit,
    XmlHttpRequest,
};

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

#[wasm_bindgen(start)]
pub fn main() {
    console_log!("Started running!");
    let server_addr: SocketAddr = format!("127.0.0.1:{}", "1111").parse().unwrap();
    let username = Username("Nick".to_string());
    client(server_addr, username);
}

const PROTOCOL_ID: u64 = 7;

fn client(server_addr: SocketAddr, username: Username) {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let local_addr = Ipv4Addr::new(127, 0, 0, 1);
    // let udp_port = rand::thread_rng().gen_range(50000..=65535);
    let udp_port = 51234;
    let udp_socket_addr = SocketAddr::from((local_addr.octets(), udp_port));
    // let socket = UdpSocket::bind(udp_socket_addr).unwrap();

    // webrtc
    let (tx, rx) = mpsc::sync_channel(1);
    let (rtc_handler, client_id) = create_client_rtc(udp_socket_addr, tx);

    // renet
    let authentication = ClientAuthentication::Unsecure {
        server_addr,
        client_id,
        user_data: Some(username.to_netcode_user_data()),
        protocol_id: PROTOCOL_ID,
    };
    let current_time = Instant::now().duration_since(Instant { 0: 0 });

    let mut client = RenetClient::new(ConnectionConfig::default());
    console_log!("DB_2");
    let mut transport = NetcodeClientTransport::new(current_time, authentication, rtc_handler).unwrap();
    console_log!("DB_3");

    // let stdin_channel: Receiver<String> = spawn_stdin_channel();
    let mut last_updated = Instant::now();

    console_log!("Main loop starts!");
    let mut message_seq = 0;
    loop {
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        client.update(duration);
        if let Err(e) = transport.update(duration, &mut client, &rx) {
            log::warn!("Failed updating transport layer: {e}");
            break;
        };

        console_log!("transport.is_connected() {}", transport.is_connected());
        if transport.is_connected() {
            console_log!("message seq = {}", message_seq);
            if message_seq % 20 == 0 {
                let message = format!("Client Message {}", message_seq);
                client.send_message(DefaultChannel::Unreliable, message.as_bytes().to_vec());
            }
            message_seq += 1;

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

        // thread::sleep(Duration::from_millis(50));
    }

    // transport.close_rtc();
    log::info!("Str0m is closed");
}

fn create_client_rtc(local_addr: SocketAddr, tx: SyncSender<Vec<u8>>) -> (RtcHandler, u64) {
    let peer_connection = match RtcPeerConnection::new() {
        Ok(pc) => pc,
        Err(e) => panic!("Failed creating peer connection: {:?}", e),
    };
    let data_channel = peer_connection.create_data_channel("data");

    // on_connectionstatechange callback
    let peer_connection_1 = peer_connection.clone();
    let onconnectionstatechange_callback = Closure::wrap(Box::new(move |evt: Event| {
        // Some(data) => {
        //     console_log!("Connection State: {:?}", data);
        // }
        // None => {}
        console_log!("Connection State: {:?}", peer_connection_1.connection_state());
    }) as Box<dyn FnMut(Event)>);
    peer_connection.set_onconnectionstatechange(Some(onconnectionstatechange_callback.as_ref().unchecked_ref()));
    onconnectionstatechange_callback.forget();

    // on_message callback
    let onmessage_callback = Closure::wrap(Box::new(move |evt: MessageEvent| match evt.data().as_string() {
        Some(data) => {
            console_log!("on_message callback: received length: {}, data: {}", data.len(), data);
            tx.send(data.into_bytes()).expect("to send Rtc instance");
        }
        None => {}
    }) as Box<dyn FnMut(MessageEvent)>);
    data_channel.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
    onmessage_callback.forget();

    // offer
    let peer_connection_2 = peer_connection.clone();
    let create_offer_func: Box<dyn FnMut(JsValue)> = Box::new(move |e: JsValue| {
        let offer = e.into();
        let peer_connection_3 = peer_connection_2.clone();
        let signaling_offer_func: Box<dyn FnMut(JsValue)> = Box::new(move |_: JsValue| {
            let request = XmlHttpRequest::new().expect("can't create new XmlHttpRequest");

            request
                .open("POST", &"http://127.0.0.1:8080")
                .unwrap_or_else(|err| log::error!("can't POST to server session url. {:?}", err));

            let peer_connection_4 = peer_connection_3.clone();
            let request_2 = request.clone();
            let request_func: Box<dyn FnMut(ProgressEvent)> = Box::new(move |_: ProgressEvent| {
                if request_2.status().unwrap() == 200 {
                    let response_string = request_2.response_text().unwrap().unwrap();
                    let signaling_response: SignalingResponse = serde_json::from_str(&response_string).unwrap();
                    let answer = signaling_response.sdp.clone();

                    console_log!("Received answer: {}", answer);

                    // #TODO parse the candiadate from answer SDP and set it to peer connection

                    // let setting_candidate_func: Box<dyn FnMut(JsValue)> = Box::new(move |e: JsValue| {
                    //     let mut candidate_init_dict: RtcIceCandidateInit = RtcIceCandidateInit::new(candidate_str);
                    //     candidate_init_dict.sdp_m_line_index(Some(session_response.candidate.sdp_m_line_index));
                    //     candidate_init_dict.sdp_mid(Some(session_response.candidate.sdp_mid.as_str()));
                    //     let candidate: RtcIceCandidate = RtcIceCandidate::new(&candidate_init_dict).unwrap();
                    // });

                    // let setting_candidate_callback = Closure::wrap(setting_candidate_func);
                    let mut sdp_init_dict = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                    sdp_init_dict.sdp(&answer);
                    peer_connection_4.set_remote_description(&sdp_init_dict);
                    // .then(&setting_candidate_callback);
                    // setting_candidate_callback.forget();
                }
            });

            let request_callback = Closure::wrap(request_func);
            request.set_onload(Some(request_callback.as_ref().unchecked_ref()));
            request_callback.forget();

            request
                .send_with_opt_str(Some(peer_connection_3.local_description().unwrap().sdp().as_str()))
                .unwrap_or_else(|err| log::error!("WebSys, can't sent request str. Original Error: {:?}", err));
        });
        // let offer = RtcSessionDescription::new_with_description_init_dict(&offer_init);
        // match offer {
        //     Ok(o) => {
        //         console_log!("\nPaste this SDP to the server terminal:");
        //         console_log!("{}\n", BASE64_STANDARD.encode(o.sdp()));

        //         // let window = web_sys::window().unwrap();
        //         // let document = window.document().unwrap();
        //         // let establish_connection = document.query_selector("#establish-connection").unwrap().unwrap();
        //         // let handler = Closure::wrap(Box::new(handle_submit) as Box<dyn Fn(_)>);
        //         // AsRef::<web_sys::EventTarget>::as_ref(&establish_connection)
        //         //     .add_event_listener_with_callback("submit", handler.as_ref().unchecked_ref());
        //         // handler.forget();

        //         console_log!("DB_0");
        //         // loop
        //         // fetch_posts(&window);
        //     }
        //     Err(e) => {
        //         // panic!("Offer SDP should be parsed to a String")
        //         console_log!("panic! {:?}", e.as_string());
        //     }
        // }
        console_log!("DB_1");
        let signaling_offer_callback = Closure::wrap(signaling_offer_func);

        peer_connection_2.set_local_description(&offer).then(&signaling_offer_callback);
        signaling_offer_callback.forget();
    });
    let create_offer_callback = Closure::wrap(create_offer_func);
    peer_connection.create_offer().then(&create_offer_callback);

    create_offer_callback.forget();

    (RtcHandler::new(peer_connection, data_channel), 0)
}

// fn spawn_stdin_channel() -> Receiver<String> {
//     let (tx, rx) = mpsc::channel::<String>();
//     thread::spawn(move || loop {
//         console_log!("DB_1");
//         let mut buffer = String::new();
//         std::io::stdin().read_line(&mut buffer).unwrap();
//         tx.send(buffer.trim_end().to_string()).unwrap();
//     });
//     rx
// }

fn decode(encoded: String) -> Result<Vec<u8>, base64::DecodeError> {
    let decoded = BASE64_STANDARD.decode(encoded)?;
    Ok(decoded)
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct SignalingResponse {
    pub sdp: String,
    pub client_id: u64,
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

// time

#[wasm_bindgen(inline_js = r#"
                export function performance_now() {
                return performance.now();
                }"#)]
extern "C" {
    fn performance_now() -> f64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    pub fn now() -> Self {
        Self((performance_now() * 1000.0) as u64)
    }
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        Duration::from_micros(self.0 - earlier.0)
    }
    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        match duration.as_micros().try_into() {
            Ok(duration) => self.0.checked_add(duration).map(|i| Self(i)),
            Err(_) => None,
        }
    }
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        match duration.as_micros().try_into() {
            Ok(duration) => self.0.checked_sub(duration).map(|i| Self(i)),
            Err(_) => None,
        }
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, other: Duration) -> Instant {
        self.checked_add(other).unwrap()
    }
}
impl Sub<Duration> for Instant {
    type Output = Instant;
    fn sub(self, other: Duration) -> Instant {
        self.checked_sub(other).unwrap()
    }
}
impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, other: Instant) -> Duration {
        self.duration_since(other)
    }
}
impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, other: Duration) {
        *self = *self + other;
    }
}
impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, other: Duration) {
        *self = *self - other;
    }
}
    }
}
