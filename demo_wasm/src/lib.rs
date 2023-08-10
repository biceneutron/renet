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
use std::convert::TryInto;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Document, Element, HtmlCollection, HtmlElement, HtmlFormElement, HtmlInputElement, HtmlTextAreaElement, MessageEvent, RtcDataChannel,
    RtcDataChannelEvent, RtcDataChannelState, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescription,
    RtcSessionDescriptionInit, Window,
};

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

// #[wasm_bindgen]
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
    // loop {
    //     let now = Instant::now();
    //     let duration = now - last_updated;
    //     last_updated = now;

    //     client.update(duration);
    //     #[cfg(not(target_arch = "wasm32"))]
    //     if let Err(e) = transport.update(duration, &mut client) {
    //         log::warn!("Failed updating transport layer: {e}");
    //         break;
    //     };

    //     #[cfg(target_arch = "wasm32")]
    //     if let Err(e) = transport.update(duration, &mut client, &rx) {
    //         log::warn!("Failed updating transport layer: {e}");
    //         break;
    //     };

    //     if transport.is_connected() {
    //         match stdin_channel.try_recv() {
    //             Ok(text) => {
    //                 client.send_message(DefaultChannel::Unreliable, text.as_bytes().to_vec())
    //             }
    //             Err(TryRecvError::Empty) => {}
    //             Err(TryRecvError::Disconnected) => panic!("Channel disconnected"),
    //         }

    //         while let Some(text) = client.receive_message(DefaultChannel::Unreliable) {
    //             let text = String::from_utf8(text.into()).unwrap();
    //             log::info!("Received user data: {}", text);
    //         }
    //     }

    //     if transport.is_data_channel_open() {
    //         match transport.send_packets(&mut client) {
    //             Ok(()) => {}
    //             Err(e) => {
    //                 log::warn!("Renet failed sending: {}", e)
    //             }
    //         };
    //     }

    //     thread::sleep(Duration::from_millis(50));
    // }

    // transport.close_rtc();
    log::info!("Str0m is closed");
}

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
        let offer_init: RtcSessionDescriptionInit = e.into();
        let offer = RtcSessionDescription::new_with_description_init_dict(&offer_init);
        match offer {
            Ok(o) => {
                console_log!("\nPaste this SDP to the server terminal:");
                console_log!("{}\n", BASE64_STANDARD.encode(o.sdp()));

                let window = web_sys::window().unwrap();
                let document = window.document().unwrap();
                let establish_connection = document.query_selector("#establish-connection").unwrap().unwrap();
                let handler = Closure::wrap(Box::new(handle_submit) as Box<dyn Fn(_)>);
                AsRef::<web_sys::EventTarget>::as_ref(&establish_connection)
                    .add_event_listener_with_callback("submit", handler.as_ref().unchecked_ref());
                handler.forget();

                console_log!("DB_0");
                // loop
                // fetch_posts(&window);
            }
            Err(e) => {
                // panic!("Offer SDP should be parsed to a String")
                console_log!("panic! {:?}", e.as_string());
            }
        }
        console_log!("DB_1");

        // peer_connection_2.set_local_description(&offer).then(&peer_desc_callback);
    });
    let create_offer_callback = Closure::wrap(create_offer_func);
    peer_connection.create_offer().then(&create_offer_callback);

    create_offer_callback.forget();

    (RtcHandler::new(peer_connection, data_channel), 0)
}

fn handle_submit(event: web_sys::Event) {
    event.prevent_default();
    console_log!("In handle submit");
    let form = event.target().unwrap().dyn_into::<HtmlFormElement>().unwrap();
    let collection = form.elements();
    let sdp_el = collection.named_item("sdp").unwrap().dyn_into::<HtmlTextAreaElement>().unwrap();
    let client_id_el = collection.named_item("client-id").unwrap().dyn_into::<HtmlInputElement>().unwrap();

    let sdp = sdp_el.value();
    let client_id = client_id_el.value();
    sdp_el.set_value("");
    client_id_el.set_value("");

    console_log!("sdp: {}", sdp);
    console_log!("client_id: {}", client_id);
}

fn spawn_stdin_channel() -> Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || loop {
        console_log!("DB_1");
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
