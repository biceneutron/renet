use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::mpsc::{Receiver, TryRecvError},
    time::Duration,
};

use renetcode::{
    ConnectToken, DisconnectReason, NetcodeClient, NetcodeError, NETCODE_KEY_BYTES, NETCODE_MAX_PACKET_BYTES, NETCODE_USER_DATA_BYTES,
};

use crate::remote_connection::RenetClient;

use super::NetcodeTransportError;

// webrtc
use std::sync::Arc;
use webrtc::{
    data_channel::{data_channel_state::RTCDataChannelState, RTCDataChannel},
    peer_connection::RTCPeerConnection,
};

// for develop
use bytes::Bytes;

/// Configuration to establish an secure ou unsecure connection with the server.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ClientAuthentication {
    /// Establishes a safe connection with the server using the [crate::transport::ConnectToken].
    ///
    /// See also [crate::transport::ServerAuthentication::Secure]
    Secure { connect_token: ConnectToken },
    /// Establishes an unsafe connection with the server, useful for testing and prototyping.
    ///
    /// See also [crate::transport::ServerAuthentication::Unsecure]
    Unsecure {
        protocol_id: u64,
        client_id: u64,
        server_addr: SocketAddr,
        user_data: Option<[u8; NETCODE_USER_DATA_BYTES]>,
    },
}

// #TODO deal with this trait
// #[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::system::Resource))]
pub struct NetcodeClientTransport {
    socket: UdpSocket,
    netcode_client: NetcodeClient,
    buffer: [u8; NETCODE_MAX_PACKET_BYTES],

    peer_connection: Arc<RTCPeerConnection>,
    data_channel: Arc<RTCDataChannel>,
}

impl NetcodeClientTransport {
    pub async fn new(
        current_time: Duration,
        authentication: ClientAuthentication,
        socket: UdpSocket,
        peer_connection: Arc<RTCPeerConnection>,
        data_channel: Arc<RTCDataChannel>,
    ) -> Result<Self, NetcodeError> {
        socket.set_nonblocking(true)?;
        let connect_token: ConnectToken = match authentication {
            ClientAuthentication::Unsecure {
                server_addr,
                protocol_id,
                client_id,
                user_data,
            } => ConnectToken::generate(
                current_time,
                protocol_id,
                300,
                client_id,
                15,
                vec![server_addr],
                user_data.as_ref(),
                &[0; NETCODE_KEY_BYTES],
            )?,
            ClientAuthentication::Secure { connect_token } => connect_token,
        };

        let netcode_client = NetcodeClient::new(current_time, connect_token);

        Ok(Self {
            buffer: [0u8; NETCODE_MAX_PACKET_BYTES],
            socket,
            netcode_client,
            peer_connection,
            data_channel,
        })
    }

    pub fn addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn client_id(&self) -> u64 {
        self.netcode_client.client_id()
    }

    pub fn is_connecting(&self) -> bool {
        self.netcode_client.is_connecting()
    }

    pub fn is_connected(&self) -> bool {
        self.netcode_client.is_connected()
    }

    pub fn is_disconnected(&self) -> bool {
        self.netcode_client.is_disconnected()
    }

    /// Returns the duration since the client last received a packet.
    /// Usefull to detect timeouts.
    pub fn time_since_last_received_packet(&self) -> Duration {
        self.netcode_client.time_since_last_received_packet()
    }

    /// Disconnect the client from the transport layer.
    /// This sends the disconnect packet instantly, use this when closing/exiting games,
    /// should use [RenetClient::disconnect][crate::RenetClient::disconnect] otherwise.
    pub async fn disconnect(&mut self) {
        if self.netcode_client.is_disconnected() {
            return;
        }

        match self.netcode_client.disconnect() {
            Ok((addr, packet)) => {
                println!(
                    "data channel actively sending disconnect packet {} bytes, to {:?}",
                    packet.len(),
                    addr
                );
                if let Err(e) = self.data_channel.send(&Bytes::copy_from_slice(packet)).await {
                    log::error!("Failed to send disconnect packet: {e}");
                }
            }
            Err(e) => log::error!("Failed to generate disconnect packet: {e}"),
        }
    }

    /// If the client is disconnected, returns the reason.
    pub fn disconnect_reason(&self) -> Option<DisconnectReason> {
        self.netcode_client.disconnect_reason()
    }

    /// Send packets to the server.
    /// Should be called every tick
    pub async fn send_packets(&mut self, connection: &mut RenetClient) -> Result<(), NetcodeTransportError> {
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            return Err(NetcodeError::Disconnected(reason).into());
        }

        let packets = connection.get_packets_to_send();
        for packet in packets {
            let (addr, payload) = self.netcode_client.generate_payload_packet(&packet)?;

            println!("send_packets: data channel sending {} bytes, to {:?}", payload.len(), addr);
            self.data_channel.send(&Bytes::copy_from_slice(payload)).await?;
        }

        Ok(())
    }

    /// Advances the transport by the duration, and receive packets from the network.
    pub async fn update(
        &mut self,
        duration: Duration,
        client: &mut RenetClient,
        rx: &Receiver<Vec<u8>>,
    ) -> Result<(), NetcodeTransportError> {
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            // Mark the client as disconnected if an error occured in the transport layer
            if !client.is_disconnected() {
                client.disconnect_due_to_transport();
            }

            return Err(NetcodeError::Disconnected(reason).into());
        }

        if let Some(error) = client.disconnect_reason() {
            let (addr, disconnect_packet) = self.netcode_client.disconnect()?;
            println!(
                "data channel sending disconnect packet {} bytes, to {:?}",
                disconnect_packet.len(),
                addr
            );
            // self.socket.send_to(disconnect_packet, addr)?;
            self.data_channel.send(&Bytes::copy_from_slice(disconnect_packet)).await?;
            return Err(error.into());
        }

        loop {
            let mut packet = match rx.try_recv() {
                Ok(data) => {
                    println!("##### received packet from mpsc channel");
                    data
                }
                Err(TryRecvError::Empty) => break,
                _ => panic!("Receiver<Rtc> disconnected"),
            };

            if let Some(payload) = self.netcode_client.process_packet(&mut packet) {
                client.process_packet(payload);
            }
        }

        if self.is_data_channel_open() {
            if let Some((packet, addr)) = self.netcode_client.update(duration) {
                println!("update: data channel sending {} bytes, to {:?}", packet.len(), addr);
                // self.socket.send_to(packet, addr)?;
                self.data_channel.send(&Bytes::copy_from_slice(packet)).await?;
            }
        }

        Ok(())
    }

    pub fn is_data_channel_open(&self) -> bool {
        self.data_channel.ready_state() == RTCDataChannelState::Open
    }

    pub async fn close_rtc(&self) -> Result<(), NetcodeTransportError> {
        self.data_channel.close().await?;
        self.peer_connection.close().await?;

        Ok(())
    }
}
