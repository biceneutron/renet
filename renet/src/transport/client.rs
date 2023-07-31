use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
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
    api::{setting_engine::SettingEngine, APIBuilder},
    data::data_channel::DataChannel,
    data_channel::{
        data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, data_channel_state::RTCDataChannelState,
        RTCDataChannel,
    },
    dtls_transport::dtls_role::DTLSRole,
    error::Error as RTCError,
    ice::mdns::MulticastDnsMode,
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription,
        RTCPeerConnection,
    },
    sdp::description::session::ATTR_KEY_CANDIDATE,
};

// for develop
use bytes::Bytes;
use std::fmt;
use tokio::time::sleep;

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
        tx: SyncSender<Vec<u8>>,
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

        // webrtc
        let mut setting_engine = SettingEngine::default();
        setting_engine.set_srtp_protection_profiles(vec![]);
        // setting_engine.detach_data_channels();
        setting_engine.set_ice_multicast_dns_mode(MulticastDnsMode::Disabled);
        setting_engine
            .set_answering_dtls_role(DTLSRole::Client)
            .expect("error in set_answering_dtls_role!");

        // Create the API object with the MediaEngine
        let api = APIBuilder::new().with_setting_engine(setting_engine).build();

        // Prepare the configuration
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                // urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let peer_connection = Arc::new(api.new_peer_connection(config).await?);
        let data_channel = peer_connection
            .create_data_channel(
                "data",
                Some(RTCDataChannelInit {
                    // ordered: Some(false),
                    // max_retransmits: Some(1),
                    // max_packet_life_time: Some(500),
                    ..Default::default()
                }),
            )
            .await
            .expect("cannot create data channel");

        peer_connection.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            log::info!("Peer Connection State has changed: {s}");
            if s == RTCPeerConnectionState::Failed {
                log::debug!("Peer Connection has gone to failed exiting");
                // let _ = done_tx.try_send(());
            }

            Box::pin(async {})
        }));
        data_channel.on_error(Box::new(move |error| {
            log::error!("data channel error: {:?}", error);
            Box::pin(async {})
        }));

        // let data_channel_ref_1 = Arc::clone(&data_channel);
        // data_channel.on_open(Box::new(move || {
        //     println!(
        //         "Data channel '{}'-'{}' open. Random messages will now be sent to any connected DataChannels every 2 seconds",
        //         data_channel_ref_1.label(),
        //         data_channel_ref_1.id()
        //     );

        //     // Box::pin(async move {})

        //     let data_channel_ref_2 = Arc::clone(&data_channel_ref_1);
        //     Box::pin(async move {
        //         let mut result = Result::<usize, RTCError>::Ok(0);
        //         let mut packet_seq = 0;
        //         while result.is_ok() {
        //             let timeout = tokio::time::sleep(Duration::from_secs(2));
        //             tokio::pin!(timeout);

        //             tokio::select! {
        //                 _ = timeout.as_mut() =>{
        //                     let message = format!("CLIENT_PACKET_{}", packet_seq);
        //                     println!("Sending '{message}'");
        //                     // result = data_channel_ref_2.send_text(message).await.map_err(Into::into);
        //                     result = data_channel_ref_2.send(&Bytes::from(message)).await.map_err(Into::into);
        //                     packet_seq += 1;
        //                 }
        //             };
        //         }
        //     })
        // }));

        // let d_label = data_channel.label().to_owned();
        data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let msg_str = String::from_utf8(msg.data.to_vec()).unwrap_or("cannot be parsed".to_string());
            println!("Received from server, length: {}, data: {}", msg.data.len(), msg_str);
            tx.send(msg.data.to_vec()).expect("to send Rtc instance");
            Box::pin(async {})
        }));

        peer_connection.on_ice_candidate(Box::new(move |candidate_opt| {
            if let Some(candidate) = &candidate_opt {
                log::debug!("received ice candidate from: {}", candidate.address);
            } else {
                log::debug!("all local candidates received");
            }

            Box::pin(async {})
        }));

        Ok(Self {
            buffer: [0u8; NETCODE_MAX_PACKET_BYTES],
            socket,
            netcode_client,
            peer_connection,
            data_channel,
        })
    }

    pub async fn create_offer(&self) -> io::Result<RTCSessionDescription> {
        let offer = self.peer_connection.create_offer(None).await.expect("cannot create offer");
        self.peer_connection
            .set_local_description(offer)
            .await
            .expect("cannot set local description");
        Ok(self.peer_connection.local_description().await.unwrap())

        // Ok(String::new())
    }

    pub async fn set_answer(&self, answer: String) -> io::Result<()> {
        let sdp = RTCSessionDescription::answer(answer).unwrap();

        let parsed_sdp = match sdp.unmarshal() {
            Ok(parsed) => {
                // for mdsp in &parsed.media_descriptions {
                //     for a in &mdsp.attributes {
                //         println!("a.key {}", a.key);
                //     }
                // }
                parsed
            }
            Err(e) => {
                panic!("error unmarshaling answer sdp {}", e);
            }
        };

        let mut candidate = Option::None;
        if let Some(c) = parsed_sdp.attribute(ATTR_KEY_CANDIDATE) {
            candidate = Some(c.to_owned());
        } else {
            for m_sdp in &parsed_sdp.media_descriptions {
                if let Some(Some(c)) = m_sdp.attribute(ATTR_KEY_CANDIDATE) {
                    candidate = Some(c.to_owned());
                    break;
                }
            }
            if candidate.is_none() {
                panic!("error no candidate in answer sdp");
            }
        }

        self.peer_connection
            .set_remote_description(sdp)
            .await
            .expect("cannot set remote description");

        // add ice candidate to connection
        if let Err(error) = self
            .peer_connection
            // .add_ice_candidate(session_response.candidate.candidate)
            .add_ice_candidate(RTCIceCandidateInit {
                candidate: candidate.unwrap(),
                ..Default::default()
            })
            .await
        {
            panic!("Error during add_ice_candidate: {:?}", error);
        }
        Ok(())
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
        // log::error!("here 0");
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            return Err(NetcodeError::Disconnected(reason).into());
        }
        // log::error!("here 1");

        // let data_channel_ref_1 = Arc::clone(&self.data_channel);
        let packets = connection.get_packets_to_send();
        // log::error!("here 1");
        for packet in packets {
            // log::error!("here 2");
            let (addr, payload) = self.netcode_client.generate_payload_packet(&packet)?;

            // #TODO instead of `self.socket.send_to`, do `data_channel.send(&Bytes::from(message)).await.map_err(Into::into)`
            // result = data_channel_ref_2.send(&Bytes::from(message)).await.map_err(Into::into);
            // self.socket.send_to(payload, addr)?;
            // log::error!("here 2");
            println!("send_packets: data channel sending {} bytes, to {:?}", payload.len(), addr);
            self.data_channel.send(&Bytes::copy_from_slice(payload)).await?;
            // .expect("data channel send should be ok")
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
            // #TODO instead of `self.socket.recv_from`, read from mpsc channel. The `on_message` callback of the
            // data channel receives WebRTC messages and writes the messages into the mpsc channel.
            // try_recv here won't lock up the thread.
            let mut packet = match rx.try_recv() {
                Ok(data) => {
                    println!("##### received packet from mpsc channel");
                    data
                }
                Err(TryRecvError::Empty) => break,
                _ => panic!("Receiver<Rtc> disconnected"),
            };
            // let packet = match self.socket.recv_from(&mut self.buffer) {
            //     Ok((len, addr)) => {

            //         if addr != self.netcode_client.server_addr() {
            //             log::debug!("Discarded packet from unknown server {:?}", addr);
            //             continue;
            //         }

            //         &mut self.buffer[..len]
            //     }
            //     Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
            //     Err(ref e) if e.kind() == io::ErrorKind::Interrupted => break,
            //     Err(e) => return Err(NetcodeTransportError::IO(e)),
            // };

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
