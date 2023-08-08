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
use std::time::Instant;

// str0m
use str0m::channel::ChannelId;
use str0m::Event;
use str0m::RtcError;
use str0m::{IceConnectionState, Input, Output, Rtc};

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
    cid: Option<ChannelId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Str0mClientId(u64);

impl Deref for Str0mClientId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Str0mClient {
    pub fn new(client_id: u64, rtc: Rtc) -> Str0mClient {
        Str0mClient {
            id: Str0mClientId(client_id),
            rtc,
            cid: None,
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

    fn poll_output(&mut self, socket: &UdpSocket) -> Str0mOutput {
        if !self.rtc.is_alive() {
            return Str0mOutput::Noop;
        }

        match self.rtc.poll_output() {
            Ok(output) => self.handle_output(output, socket),
            Err(e) => {
                log::warn!("Client ({}) poll_output failed: {:?}", *self.id, e);
                self.rtc.disconnect();
                Str0mOutput::Noop
            }
        }
    }

    fn handle_output(&mut self, output: Output, socket: &UdpSocket) -> Str0mOutput {
        match output {
            Output::Transmit(transmit) => {
                socket.send_to(&transmit.contents, transmit.destination).expect("sending UDP data");
                Str0mOutput::Noop
            }
            Output::Timeout(t) => Str0mOutput::Timeout(t),
            Output::Event(e) => match e {
                Event::IceConnectionStateChange(state) => {
                    log::info!("ICE connection state: {:?}", state);
                    if state == IceConnectionState::Disconnected {
                        // Ice disconnect could result in trying to establish a new connection,
                        // but this impl just disconnects directly.
                        self.rtc.disconnect();
                    }
                    Str0mOutput::Noop
                }
                Event::Connected => {
                    log::info!("ICE connection has been built");
                    Str0mOutput::Noop
                }
                Event::ChannelClose(cid) => {
                    log::info!("Data channel {:?} is closed", cid);
                    Str0mOutput::Noop
                }
                Event::ChannelOpen(cid, _) => {
                    log::info!("Data channel {:?} is open", cid);

                    self.cid = Some(cid);
                    Str0mOutput::Noop
                }
                Event::ChannelData(data) => {
                    log::debug!("Data channel received {} bytes", data.data.len());

                    Str0mOutput::Data(data.data)
                }
                _ => {
                    log::warn!("Str0m got other events {:?}", e);
                    Str0mOutput::Noop
                }
            },
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
#[derive(Debug, Clone)]
enum Str0mOutput {
    /// When we have nothing to propagate.
    Noop,

    /// Poll client has reached timeout.
    Timeout(Instant),

    /// client received data from data channel
    Data(Vec<u8>),
}

impl Str0mOutput {
    /// If the propagated data is a timeout, returns the instant.
    fn as_timeout(&self) -> Option<Instant> {
        if let Self::Timeout(v) = self {
            Some(*v)
        } else {
            None
        }
    }
}
