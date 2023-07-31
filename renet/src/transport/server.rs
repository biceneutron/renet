use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, UdpSocket},
    time::Duration,
};

use renetcode::{NetcodeServer, ServerResult, NETCODE_KEY_BYTES, NETCODE_MAX_PACKET_BYTES, NETCODE_USER_DATA_BYTES};

use crate::server::RenetServer;

use super::{NetcodeTransportError, Propagated, Str0mClient, Str0mClientId};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;
use str0m::{net::Receive, Input, Rtc};

/// Configuration to establish a secure or unsecure connection with the server.
#[derive(Debug)]
pub enum ServerAuthentication {
    /// Establishes a safe connection using a private key for encryption. The private key cannot be
    /// shared with the client. Connections are stablished using [crate::transport::ConnectToken].
    ///
    /// See also [ClientAuthentication::Secure][crate::transport::ClientAuthentication::Secure]
    Secure { private_key: [u8; NETCODE_KEY_BYTES] },
    /// Establishes unsafe connections with clients, useful for testing and prototyping.
    ///
    /// See also [ClientAuthentication::Unsecure][crate::transport::ClientAuthentication::Unsecure]
    Unsecure,
}

/// Configuration options for the server transport.
#[derive(Debug)]
pub struct ServerConfig {
    /// Maximum numbers of clients that can be connected at a time
    pub max_clients: usize,
    /// Unique identifier to this game/application
    /// One could use a hash function with the game current version to generate this value.
    /// So old version would be unable to connect to newer versions.
    pub protocol_id: u64,
    /// Publicly available address that clients will try to connect to. This is
    /// the address used to generate the ConnectToken when using the secure authentication.
    pub public_addr: SocketAddr,
    /// Authentication configuration for the server
    pub authentication: ServerAuthentication,
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::system::Resource))]
pub struct NetcodeServerTransport {
    socket: UdpSocket,
    netcode_server: NetcodeServer,
    buffer: [u8; NETCODE_MAX_PACKET_BYTES],

    str0m_clients: Vec<Str0mClient>,
    datachannel_mapping: HashMap<Str0mClientId, SocketAddr>,
}

impl NetcodeServerTransport {
    pub fn new(current_time: Duration, server_config: ServerConfig, socket: UdpSocket) -> Result<Self, std::io::Error> {
        socket.set_nonblocking(true)?;

        // For unsecure connections we use an fixed private key.
        let private_key = match server_config.authentication {
            ServerAuthentication::Unsecure => [0; NETCODE_KEY_BYTES],
            ServerAuthentication::Secure { private_key } => private_key,
        };

        let netcode_server = NetcodeServer::new(
            current_time,
            server_config.max_clients,
            server_config.protocol_id,
            server_config.public_addr,
            private_key,
        );

        Ok(Self {
            socket,
            netcode_server,
            buffer: [0; NETCODE_MAX_PACKET_BYTES],
            str0m_clients: vec![],
            datachannel_mapping: HashMap::new(),
        })
    }

    /// Returns the server public address
    pub fn addr(&self) -> SocketAddr {
        self.netcode_server.address()
    }

    /// Returns the maximum number of clients that can be connected.
    pub fn max_clients(&self) -> usize {
        self.netcode_server.max_clients()
    }

    /// Returns current number of clients connected.
    pub fn connected_clients(&self) -> usize {
        self.netcode_server.connected_clients()
    }

    /// Returns the user data for client if connected.
    pub fn user_data(&self, client_id: u64) -> Option<[u8; NETCODE_USER_DATA_BYTES]> {
        self.netcode_server.user_data(client_id)
    }

    /// Returns the client address if connected.
    pub fn client_addr(&self, client_id: u64) -> Option<SocketAddr> {
        self.netcode_server.client_addr(client_id)
    }

    /// Disconnects all connected clients.
    /// This sends the disconnect packet instantly, use this when closing/exiting games,
    /// should use [RenetServer::disconnect_all][crate::RenetServer::disconnect_all] otherwise.
    pub fn disconnect_all(&mut self, server: &mut RenetServer) {
        for client_id in self.netcode_server.clients_id() {
            let server_result = self.netcode_server.disconnect(client_id);

            if let Some(str0m_client) = find_str0m_client_by_id(&mut self.str0m_clients, client_id) {
                handle_server_result(server_result, str0m_client, server);
            } else {
                panic!("No corresponding str0m client");
            }
        }
    }

    /// Returns the duration since the connected client last received a packet.
    /// Usefull to detect users that are timing out.
    pub fn time_since_last_received_packet(&self, client_id: u64) -> Option<Duration> {
        self.netcode_server.time_since_last_received_packet(client_id)
    }

    /// Advances the transport by the duration, and receive packets from the network.
    pub fn update(&mut self, duration: Duration, server: &mut RenetServer) -> Result<(), NetcodeTransportError> {
        self.netcode_server.update(duration);

        self.str0m_clients.retain(|c| c.rtc.is_alive());

        loop {
            // str0m handing output events
            // Poll all clients, and get propagated events as a result.
            let to_propagate: Vec<_> = self
                .str0m_clients
                .iter_mut()
                .map(|c| {
                    if !c.rtc.is_alive() {
                        return Propagated::Timeout(Instant::now());
                    }

                    if let Some((_, destination)) = c.get_send_addr() {
                        self.datachannel_mapping.entry(c.id).or_insert_with(|| destination);
                    }

                    let (propagated, maybe_data) = c.poll_output(&self.socket);
                    if maybe_data.is_some() && self.datachannel_mapping.contains_key(&c.id) {
                        let source = self.datachannel_mapping.get(&c.id).unwrap();
                        let data = maybe_data.unwrap();

                        // renet
                        println!(
                            "after being processed by str0m, {} bytes go in to renet, from {:?}",
                            data.len(),
                            source
                        );
                        let buf = &mut data.to_vec();
                        let server_result = self.netcode_server.process_packet(*source, buf);
                        handle_server_result(server_result, c, server);
                    }
                    return propagated;
                })
                .collect();
            let timeouts: Vec<_> = to_propagate.iter().filter_map(|p| p.as_timeout()).collect();

            // We keep propagating client events until all clients respond with a timeout.
            if to_propagate.len() > timeouts.len() {
                propagate(&mut self.str0m_clients, to_propagate);
                // Start over to propagate more client data until all are timeouts.
                continue;
            }

            match self.socket.recv_from(&mut self.buffer) {
                Ok((len, source)) => {
                    // str0m
                    let buf = self.buffer.clone();
                    if let Ok(contents) = buf[..len].try_into() {
                        println!("udp socket received {} bytes, from {:?}, handled by str0m", len, source);
                        let input = Input::Receive(
                            Instant::now(),
                            Receive {
                                source,
                                destination: self.socket.local_addr().unwrap(),
                                contents,
                            },
                        );

                        if let Some(client) = self.str0m_clients.iter_mut().find(|c| c.accepts(&input)) {
                            client.handle_input(input);
                        } else {
                            log::debug!("No client accepts UDP input: {:?}", input);
                        }
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
                Err(e) => return Err(e.into()),
            };
        }

        for client_id in self.netcode_server.clients_id() {
            let server_result = self.netcode_server.update_client(client_id);

            if let Some(str0m_client) = find_str0m_client_by_id(&mut self.str0m_clients, client_id) {
                handle_server_result(server_result, str0m_client, server);
            } else {
                panic!("No corresponding str0m client");
            }
        }

        for disconnection_id in server.disconnections_id() {
            let server_result = self.netcode_server.disconnect(disconnection_id);

            if let Some(str0m_client) = find_str0m_client_by_id(&mut self.str0m_clients, disconnection_id) {
                handle_server_result(server_result, str0m_client, server);
            } else {
                panic!("No corresponding str0m client");
            }
        }

        // str0m
        // Drive time forward in all clients.
        let now = Instant::now();
        for client in &mut self.str0m_clients {
            client.handle_input(Input::Timeout(now));
        }

        Ok(())
    }

    /// Send packets to connected clients.
    pub fn send_packets(&mut self, server: &mut RenetServer) {
        'clients: for client_id in server.clients_id() {
            let packets = server.get_packets_to_send(client_id).unwrap();
            for packet in packets {
                match self.netcode_server.generate_payload_packet(client_id, &packet) {
                    Ok((addr, payload)) => {
                        if let Some(str0m_client) = find_str0m_client_by_id(&mut self.str0m_clients, client_id) {
                            let mut channel = str0m_client
                                .cid
                                .and_then(|id| str0m_client.rtc.channel(id))
                                .expect("channel to be open");
                            println!("str0m channel sending {} bytes, to {:?}", payload.len(), addr);
                            if let Err(err) = channel.write(true, payload) {
                                log::error!("Failed to send packet to {addr}: {err}");
                            }
                        } else {
                            log::error!("Failed to send packet to client {client_id} ({addr}): cannot find str0m client");
                            continue 'clients;
                        };
                    }
                    Err(e) => {
                        log::error!("Failed to encrypt payload packet for client {client_id}: {e}");
                        continue 'clients;
                    }
                }
            }
        }
    }

    pub fn spawn_new_client(&mut self, rx: &Receiver<(u64, Rtc)>) {
        // try_recv here won't lock up the thread.
        match rx.try_recv() {
            Ok((client_id, rtc)) => {
                let new_client = Str0mClient::new(client_id, rtc);
                self.str0m_clients.push(new_client);
            }
            Err(TryRecvError::Empty) => {}
            _ => panic!("Receiver<Rtc> disconnected"),
        }
    }

    pub fn get_num_str0mclients(&self) -> usize {
        self.str0m_clients.len()
    }
}

fn find_str0m_client_by_id(clients: &mut Vec<Str0mClient>, client_id: u64) -> Option<&mut Str0mClient> {
    clients.iter_mut().find(|c| c.id.0 == client_id)
}

fn handle_server_result(server_result: ServerResult, client: &mut Str0mClient, reliable_server: &mut RenetServer) {
    // channel.write(false, &d.data).expect("to write answer");
    let mut channel = client.cid.and_then(|id| client.rtc.channel(id)).expect("channel to be open");
    let mut send_packet = |packet: &[u8], addr: SocketAddr| {
        println!("str0m channel sending {} bytes, to {:?}", packet.len(), addr);
        if let Err(err) = channel.write(true, packet) {
            log::error!("Failed to send packet to {addr}: {err}");
        }
    };

    match server_result {
        ServerResult::None => {}
        ServerResult::PacketToSend { payload, addr } => {
            println!("Handling ServerResult::PacketToSend");
            send_packet(payload, addr);
        }
        ServerResult::Payload { client_id, payload } => {
            println!("Handling ServerResult::Payload");
            if let Err(e) = reliable_server.process_packet_from(payload, client_id) {
                log::error!("Error while processing payload for {}: {}", client_id, e);
            }
        }
        ServerResult::ClientConnected {
            client_id,
            user_data: _,
            addr,
            payload,
        } => {
            println!("Handling ServerResult::ClientConnected");
            reliable_server.add_connection(client_id);
            send_packet(payload, addr);
        }
        ServerResult::ClientDisconnected { client_id, addr, payload } => {
            println!("Handling ServerResult::ClientDisconnected");
            reliable_server.remove_connection(client_id);
            if let Some(payload) = payload {
                send_packet(payload, addr);
            }
        }
    }
}

fn propagate(clients: &mut [Str0mClient], to_propagate: Vec<Propagated>) {
    for p in to_propagate {
        let Some(client_id) = p.client_id() else {
            // If the event doesn't have a client id, it can't be propagated,
            // (it's either a noop or a timeout).
            continue;
        };

        for client in &mut *clients {
            if client.id == client_id {
                // Do not propagate to originating client.
                continue;
            }

            match &p {
                Propagated::TrackOpen(_, track_in) => client.handle_track_open(track_in.clone()),
                Propagated::MediaData(_, data) => client.handle_media_data(client_id, data),
                Propagated::KeyframeRequest(_, req, origin, mid_in) => {
                    // Only one origin client handles the keyframe request.
                    if *origin == client.id {
                        client.handle_keyframe_request(*req, *mid_in)
                    }
                }
                Propagated::Noop | Propagated::Timeout(_) => {}
            }
        }
    }
}
