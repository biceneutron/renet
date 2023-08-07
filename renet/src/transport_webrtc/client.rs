use std::{
    io,
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

use renetcode::{
    ConnectToken, DisconnectReason, NetcodeClient, NetcodeError, NETCODE_KEY_BYTES, NETCODE_MAX_PACKET_BYTES, NETCODE_USER_DATA_BYTES,
};

use str0m::{net::Receive, Input};

use crate::remote_connection::RenetClient;

use super::{NetcodeTransportError, Str0mClient, Str0mOutput};

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

    str0m_client: Str0mClient,
}

impl NetcodeClientTransport {
    pub fn new(
        current_time: Duration,
        authentication: ClientAuthentication,
        socket: UdpSocket,
        str0m_client: Str0mClient,
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
            str0m_client,
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
    pub fn disconnect(&mut self) {
        if self.netcode_client.is_disconnected() {
            return;
        }

        match self.netcode_client.disconnect() {
            Ok((addr, packet)) => {
                log::debug!(
                    "Data channel actively sending {} bytes of disconnection packet to {:?}",
                    packet.len(),
                    addr
                );
                if let Err(e) = channel_send(&mut self.str0m_client, packet) {
                    log::error!("Failed to send disconnect packet: {e}");
                };
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
    pub fn send_packets(&mut self, connection: &mut RenetClient) -> Result<(), NetcodeTransportError> {
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            return Err(NetcodeError::Disconnected(reason).into());
        }

        let packets = connection.get_packets_to_send();
        for packet in packets {
            let (addr, payload) = self.netcode_client.generate_payload_packet(&packet)?;

            log::debug!("Data channel sending {} bytes to {:?}", payload.len(), addr);
            channel_send(&mut self.str0m_client, payload)?;
        }

        Ok(())
    }

    /// Advances the transport by the duration, and receive packets from the network.
    pub fn update(&mut self, duration: Duration, client: &mut RenetClient) -> Result<(), NetcodeTransportError> {
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            // Mark the client as disconnected if an error occured in the transport layer
            if !client.is_disconnected() {
                client.disconnect_due_to_transport();
            }

            return Err(NetcodeError::Disconnected(reason).into());
        }

        if let Some(error) = client.disconnect_reason() {
            let (addr, disconnect_packet) = self.netcode_client.disconnect()?;
            log::debug!(
                "Data channel sending {} bytes of disconnection packet to {:?}",
                disconnect_packet.len(),
                addr
            );

            channel_send(&mut self.str0m_client, &disconnect_packet)?;
            return Err(error.into());
        }

        loop {
            let output = if self.str0m_client.rtc.is_alive() {
                self.str0m_client.poll_output(&self.socket)
            } else {
                break;
            };

            // Poll the str0m rtc output until we get timeout
            // If `maybe_data` is an Option::Some, the data channel must have been opened.
            match output {
                Str0mOutput::Timeout(_) => {}
                Str0mOutput::Data(mut data) => {
                    if let Some(payload) = self.netcode_client.process_packet(&mut data) {
                        client.process_packet(payload);
                    };
                    continue;
                }
                Str0mOutput::Noop => continue,
            }

            match self.socket.recv_from(&mut self.buffer) {
                Ok((len, addr)) => {
                    log::debug!("UDP socket received {} bytes from {:?}", len, addr);

                    // str0m
                    if let Ok(contents) = self.buffer[..len].try_into() {
                        let input = Input::Receive(
                            Instant::now(),
                            Receive {
                                source: addr,
                                destination: self.socket.local_addr().unwrap(),
                                contents,
                            },
                        );

                        if self.str0m_client.accepts(&input) {
                            self.str0m_client.handle_input(input);
                        } else {
                            log::warn!("Str0m client doesn't accept the incoming packet");
                        }
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(NetcodeTransportError::IO(e)),
            };
        }

        if let Some((packet, addr)) = self.netcode_client.update(duration) {
            if is_data_channel_open(&self.str0m_client) {
                log::debug!("Data channel sending {} bytes to {:?}", packet.len(), addr);
                channel_send(&mut self.str0m_client, packet)?;
            }
        }

        let now = Instant::now();
        self.str0m_client.handle_input(Input::Timeout(now));

        Ok(())
    }

    pub fn is_data_channel_open(&self) -> bool {
        self.str0m_client.cid.is_some()
    }

    pub fn close_rtc(&mut self) {
        self.str0m_client.rtc.disconnect();
    }
}

fn is_data_channel_open(client: &Str0mClient) -> bool {
    client.cid.is_some()
}

// should be private
fn channel_send(client: &mut Str0mClient, data: &[u8]) -> Result<usize, NetcodeTransportError> {
    let mut channel = client.cid.and_then(|id| client.rtc.channel(id)).expect("channel to be open");
    channel.write(true, data).map_err(Into::into)
}
