use std::{error::Error, fmt};

mod client;
mod server;

pub use client::*;
pub use server::*;

pub use renetcode::{
    generate_random_bytes, ConnectToken, DisconnectReason as NetcodeDisconnectReason, NetcodeError, TokenGenerationError,
    NETCODE_KEY_BYTES, NETCODE_USER_DATA_BYTES,
};

use std::net::{SocketAddr, UdpSocket};
use std::ops::Deref;
use std::sync::{Arc, Weak};
use std::time::Instant;
use str0m::RtcError;
pub use webrtc::Error as WebRTCError;

// str0m
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelData, ChannelId};
use str0m::media::MediaKind;
use str0m::media::{Direction, KeyframeRequest, MediaData, Mid, Rid};
use str0m::Event;
use str0m::{IceConnectionState, Input, Output, Rtc};

use serde::{Deserialize, Serialize};

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Event))]
pub enum NetcodeTransportError {
    Netcode(NetcodeError),
    Renet(crate::DisconnectReason),
    IO(std::io::Error),
    Rtc(RtcError),
}

impl Error for NetcodeTransportError {}

impl fmt::Display for NetcodeTransportError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            NetcodeTransportError::Netcode(ref err) => err.fmt(fmt),
            NetcodeTransportError::Renet(ref err) => err.fmt(fmt),
            NetcodeTransportError::IO(ref err) => err.fmt(fmt),
            NetcodeTransportError::Rtc(ref err) => err.fmt(fmt),
        }
    }
}

impl From<renetcode::NetcodeError> for NetcodeTransportError {
    fn from(inner: renetcode::NetcodeError) -> Self {
        NetcodeTransportError::Netcode(inner)
    }
}

impl From<renetcode::TokenGenerationError> for NetcodeTransportError {
    fn from(inner: renetcode::TokenGenerationError) -> Self {
        NetcodeTransportError::Netcode(renetcode::NetcodeError::TokenGenerationError(inner))
    }
}

impl From<crate::DisconnectReason> for NetcodeTransportError {
    fn from(inner: crate::DisconnectReason) -> Self {
        NetcodeTransportError::Renet(inner)
    }
}

impl From<std::io::Error> for NetcodeTransportError {
    fn from(inner: std::io::Error) -> Self {
        NetcodeTransportError::IO(inner)
    }
}

impl From<RtcError> for NetcodeTransportError {
    fn from(inner: RtcError) -> Self {
        NetcodeTransportError::Rtc(inner)
    }
}

// str0m stuffs
#[derive(Debug)]
pub struct Str0mClient {
    id: Str0mClientId,
    rtc: Rtc,
    pending: Option<SdpPendingOffer>,
    cid: Option<ChannelId>,
    tracks_in: Vec<Arc<TrackIn>>,
    tracks_out: Vec<TrackOut>,
    chosen_rid: Option<Rid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Str0mClientId(u64);

impl Deref for Str0mClientId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
struct TrackIn {
    origin: Str0mClientId,
    mid: Mid,
    kind: MediaKind,
}

#[derive(Debug)]
struct TrackOut {
    track_in: Weak<TrackIn>,
    state: TrackOutState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
}

impl TrackOut {
    fn mid(&self) -> Option<Mid> {
        match self.state {
            TrackOutState::ToOpen => None,
            TrackOutState::Negotiating(m) | TrackOutState::Open(m) => Some(m),
        }
    }
}

impl Str0mClient {
    pub fn new(client_id: u64, rtc: Rtc) -> Str0mClient {
        // static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
        // let next_id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        Str0mClient {
            // id: Str0mClientId(next_id),
            id: Str0mClientId(client_id),
            rtc,
            pending: None,
            cid: None,
            tracks_in: vec![],
            tracks_out: vec![],
            chosen_rid: None,
        }
    }

    fn accepts(&self, input: &Input) -> bool {
        self.rtc.accepts(input)
    }

    fn handle_input(&mut self, input: Input) {
        if !self.rtc.is_alive() {
            return;
        }

        if let Err(e) = self.rtc.handle_input(input) {
            log::warn!("Client ({}) disconnected: {:?}", *self.id, e);
            self.rtc.disconnect();
        }
    }

    fn poll_output(&mut self, socket: &UdpSocket) -> (Propagated, Option<Vec<u8>>) {
        println!("poll_output 1");
        if !self.rtc.is_alive() {
            return (Propagated::Noop, None);
        }

        println!("poll_output 2");

        // Incoming tracks from other clients cause new entries in track_out that
        // need SDP negotiation with the remote peer.
        if self.negotiate_if_needed() {
            return (Propagated::Noop, None);
        }

        println!("poll_output 3");

        match self.rtc.poll_output() {
            Ok(output) => self.handle_output(output, socket),
            Err(e) => {
                log::warn!("Client ({}) poll_output failed: {:?}", *self.id, e);
                self.rtc.disconnect();
                (Propagated::Noop, None)
            }
        }
    }

    fn handle_output(&mut self, output: Output, socket: &UdpSocket) -> (Propagated, Option<Vec<u8>>) {
        match output {
            Output::Transmit(transmit) => {
                println!(
                    "handle_output Output::Transmit, {}, {}",
                    transmit.contents.to_vec().len(),
                    transmit.destination
                );
                // println!("handle_output Output::Transmit, {}", transmit.destination);
                // let data = "SERVER MESSAGE".as_bytes();
                socket
                    .send_to(&transmit.contents, transmit.destination)
                    // .send_to(data, transmit.destination)
                    .expect("sending UDP data");
                (Propagated::Noop, None)
            }
            Output::Timeout(t) => {
                println!("handle_output Output::Timeout");
                (Propagated::Timeout(t), None)
            }
            Output::Event(e) => match e {
                Event::IceConnectionStateChange(v) => {
                    println!("handle_output Output::Event::IceConnectionStateChange {}", v as u8);
                    if v == IceConnectionState::Disconnected {
                        // Ice disconnect could result in trying to establish a new connection,
                        // but this impl just disconnects directly.
                        self.rtc.disconnect();
                    }
                    (Propagated::Noop, None)
                }
                // Event::MediaAdded(e) => {
                //     println!("handle_output Output::Event::MediaAdded");
                //     self.handle_media_added(e.mid, e.kind)
                // }
                // Event::MediaData(data) => {
                //     println!("handle_output Output::Event::MediaData");
                //     Propagated::MediaData(self.id, data)
                // }
                // Event::KeyframeRequest(req) => {
                //     println!("handle_output Output::Event::KeyframeRequest");
                //     self.handle_incoming_keyframe_req(req)
                // }
                Event::ChannelOpen(cid, _) => {
                    println!("handle_output Output::Event::ChannelOpen");
                    self.cid = Some(cid);
                    (Propagated::Noop, None)
                }
                Event::ChannelData(data) => {
                    println!("handle_output Output::Event::ChannelData");
                    println!(
                        "length: {}, user data: {}",
                        data.data.len(),
                        String::from_utf8(data.data.clone()).unwrap_or("Cannot be parsed to string".to_string())
                    );

                    (Propagated::Noop, Some(data.data))
                    // self.handle_channel_data(data)
                }

                // NB: To see statistics, uncomment set_stats_interval() above.
                Event::MediaIngressStats(data) => {
                    println!("handle_output Output::Event::MediaIngressStats");
                    log::info!("{:?}", data);
                    (Propagated::Noop, None)
                }
                Event::MediaEgressStats(data) => {
                    println!("handle_output Output::Event::MediaEgressStats");
                    log::info!("{:?}", data);
                    (Propagated::Noop, None)
                }
                Event::PeerStats(data) => {
                    println!("handle_output Output::Event::PeerStats");
                    log::info!("{:?}", data);
                    (Propagated::Noop, None)
                }
                _ => {
                    println!("handle_output Output::Event:: others");
                    (Propagated::Noop, None)
                }
            },
        }
    }

    fn handle_media_added(&mut self, mid: Mid, kind: MediaKind) -> Propagated {
        let track_in = Arc::new(TrackIn {
            origin: self.id,
            mid,
            kind,
        });

        // The Client instance owns the strong reference to the incoming
        // track, all other clients have a weak reference.
        let weak = Arc::downgrade(&track_in);
        self.tracks_in.push(track_in);

        Propagated::TrackOpen(self.id, weak)
    }

    fn handle_incoming_keyframe_req(&self, mut req: KeyframeRequest) -> Propagated {
        // Need to figure out the track_in mid that needs to handle the keyframe request.
        let Some(track_out) = self.tracks_out.iter().find(|t| t.mid() == Some(req.mid)) else {
                return Propagated::Noop;
            };
        let Some(track_in) = track_out.track_in.upgrade() else {
                return Propagated::Noop;
            };

        // This is the rid picked from incoming mediadata, and to which we need to
        // send the keyframe request.
        req.rid = self.chosen_rid;

        Propagated::KeyframeRequest(self.id, req, track_in.origin, track_in.mid)
    }

    fn negotiate_if_needed(&mut self) -> bool {
        if self.cid.is_none() || self.pending.is_some() {
            // Don't negotiate if there is no data channel, or if we have pending changes already.
            return false;
        }

        let mut change = self.rtc.sdp_api();

        for track in &mut self.tracks_out {
            if let TrackOutState::ToOpen = track.state {
                if let Some(track_in) = track.track_in.upgrade() {
                    let stream_id = track_in.origin.to_string();
                    let mid = change.add_media(track_in.kind, Direction::SendOnly, Some(stream_id), None);
                    track.state = TrackOutState::Negotiating(mid);
                }
            }
        }

        if !change.has_changes() {
            return false;
        }

        let Some((offer, pending)) = change.apply() else {
            return false;
        };

        let Some(mut channel) = self
                .cid
                .and_then(|id| self.rtc.channel(id)) else {
                    return false;
                };

        let json = serde_json::to_string(&offer).unwrap();
        channel.write(false, json.as_bytes()).expect("to write answer");

        self.pending = Some(pending);

        true
    }

    fn handle_channel_data(&mut self, d: ChannelData) -> Propagated {
        // if let Ok(offer) = serde_json::from_slice::<'_, SdpOffer>(&d.data) {
        //     self.handle_offer(offer);
        // } else if let Ok(answer) = serde_json::from_slice::<'_, SdpAnswer>(&d.data) {
        //     self.handle_answer(answer);
        // }

        let mut channel = self.cid.and_then(|id| self.rtc.channel(id)).expect("channel to be open");

        // String::from_utf8(d.data.clone()).unwrap_or(String::new()).split();

        // let json = serde_json::to_string(&answer).unwrap();
        channel.write(false, &d.data).expect("to write answer");

        Propagated::Noop
    }

    fn handle_offer(&mut self, offer: SdpOffer) {
        let answer = self.rtc.sdp_api().accept_offer(offer).expect("offer to be accepted");

        // Keep local track state in sync, cancelling any pending negotiation
        // so we can redo it after this offer is handled.
        for track in &mut self.tracks_out {
            if let TrackOutState::Negotiating(_) = track.state {
                track.state = TrackOutState::ToOpen;
            }
        }

        let mut channel = self.cid.and_then(|id| self.rtc.channel(id)).expect("channel to be open");

        let json = serde_json::to_string(&answer).unwrap();
        channel.write(false, json.as_bytes()).expect("to write answer");
    }

    fn handle_answer(&mut self, answer: SdpAnswer) {
        if let Some(pending) = self.pending.take() {
            self.rtc.sdp_api().accept_answer(pending, answer).expect("answer to be accepted");

            for track in &mut self.tracks_out {
                if let TrackOutState::Negotiating(m) = track.state {
                    track.state = TrackOutState::Open(m);
                }
            }
        }
    }

    fn handle_track_open(&mut self, track_in: Weak<TrackIn>) {
        let track_out = TrackOut {
            track_in,
            state: TrackOutState::ToOpen,
        };
        self.tracks_out.push(track_out);
    }

    fn handle_media_data(&mut self, origin: Str0mClientId, data: &MediaData) {
        // Figure out which outgoing track maps to the incoming media data.
        let Some(mid) = self.tracks_out
            .iter()
            .find(|o| o.track_in.upgrade().filter(|i|
                i.origin == origin &&
                i.mid == data.mid).is_some())
            .and_then(|o| o.mid()) else {
                return;
            };

        let Some(mut media) = self.rtc.media(mid) else {
            return;
        };

        if data.rid.is_some() && data.rid != Some("h".into()) {
            // This is where we plug in a selection strategy for simulcast. For
            // now either let rid=None through (which would be no simulcast layers)
            // or "h" if we have simulcast (see commented out code in chat.html).
            return;
        }

        // Remember this value for keyframe requests.
        if self.chosen_rid != data.rid {
            self.chosen_rid = data.rid;
        }

        // Match outgoing pt to incoming codec.
        let Some(pt) = media.match_params(data.params) else {
            return;
        };

        if let Err(e) = media.writer(pt).write(data.network_time, data.time, &data.data) {
            log::warn!("Client ({}) failed: {:?}", *self.id, e);
            self.rtc.disconnect();
        }
    }

    fn handle_keyframe_request(&mut self, req: KeyframeRequest, mid_in: Mid) {
        let has_incoming_track = self.tracks_in.iter().any(|i| i.mid == mid_in);

        // This will be the case for all other client but the one where the track originates.
        if !has_incoming_track {
            return;
        }

        let Some(mut media) = self.rtc.media(mid_in) else {
            return;
        };

        if let Err(e) = media.request_keyframe(req.rid, req.kind) {
            // This can fail if the rid doesn't match any media.
            log::info!("request_keyframe failed: {:?}", e);
        }
    }

    pub fn show_send_addr(&self) {
        self.rtc.show_send_addr();
    }

    pub fn get_send_addr(&self) -> Option<(SocketAddr, SocketAddr)> {
        self.rtc.get_send_addr()
    }
}

/// Events propagated between client.
#[allow(clippy::large_enum_variant)]

enum Propagated {
    /// When we have nothing to propagate.
    Noop,

    /// Poll client has reached timeout.
    Timeout(Instant),

    /// A new incoming track opened.
    TrackOpen(Str0mClientId, Weak<TrackIn>),

    /// Data to be propagated from one client to another.
    MediaData(Str0mClientId, MediaData),

    /// A keyframe request from one client to the source.
    KeyframeRequest(Str0mClientId, KeyframeRequest, Str0mClientId, Mid),
}

impl Propagated {
    /// Get client id, if the propagated event has a client id.
    fn client_id(&self) -> Option<Str0mClientId> {
        match self {
            Propagated::TrackOpen(c, _) | Propagated::MediaData(c, _) | Propagated::KeyframeRequest(c, _, _, _) => Some(*c),
            _ => None,
        }
    }

    /// If the propagated data is a timeout, returns the instant.
    fn as_timeout(&self) -> Option<Instant> {
        if let Self::Timeout(v) = self {
            Some(*v)
        } else {
            None
        }
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct ICEResponse {
    pub sdp: String,
    pub client_id: u64,
}

impl ICEResponse {
    pub fn new(sdp: String, client_id: u64) -> ICEResponse {
        ICEResponse { sdp, client_id }
    }
}
