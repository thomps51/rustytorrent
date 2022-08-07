use log::{debug, info};
use std::sync::mpsc::{Receiver, Sender};

use mio::{Events, Interest, Poll, Token};

use crate::{common::Sha1Hash, tracker::PeerInfo};

use super::{controller::ControllerInputMessage, NetworkSource};

#[derive(Debug)]
pub enum PollerRequest {
    Register {
        info_hash: Option<Sha1Hash>,
        source: NetworkSource,
        token: Token,
        peer_info: PeerInfo,
        interests: Interest,
    },
    Reregister {
        info_hash: Option<Sha1Hash>,
        source: NetworkSource,
        token: Token,
        peer_info: PeerInfo,
        interests: Interest,
    },
    Deregister {
        source: NetworkSource,
        token: Token,
    },
    Stop,
}

#[derive(Debug)]
pub enum PollerResponse {
    Events {
        events: Events,
    },
    RegisterDone {
        info_hash: Option<Sha1Hash>,
        source: NetworkSource,
        token: Token,
        peer_info: PeerInfo,
        interests: Interest,
    },
    ReregisterDone {
        info_hash: Option<Sha1Hash>,
        source: NetworkSource,
        token: Token,
        peer_info: PeerInfo,
        interests: Interest,
    },
    DeregisterDone {
        source: NetworkSource,
        token: Token,
    },
}

pub struct NetworkPollerManager {
    pub poll: Poll,
    pub recv: Receiver<PollerRequest>,
    pub sender: Sender<ControllerInputMessage>,
}

impl NetworkPollerManager {
    pub fn run(&mut self) {
        let mut events = Events::with_capacity(1024);
        loop {
            self.poll
                .poll(&mut events, Some(std::time::Duration::from_millis(1000)))
                .unwrap();
            // self.poll.poll(&mut events, None).unwrap();
            if !events.is_empty() {
                let mut events_clone = Events::with_capacity(1024);
                std::mem::swap(&mut events, &mut events_clone);
                self.sender
                    .send(ControllerInputMessage::Network(PollerResponse::Events {
                        events: events_clone,
                    }))
                    .unwrap();
            } else {
                debug!("No events while polling");
            }
            loop {
                match self.recv.try_recv() {
                    Ok(msg) => {
                        debug!("NetworkPoller received message: {:?}", msg);
                        let response = match msg {
                            PollerRequest::Register {
                                info_hash,
                                source: mut stream,
                                token,
                                interests,
                                peer_info,
                            } => {
                                self.poll
                                    .registry()
                                    .register(&mut stream, token, interests)
                                    .unwrap();
                                PollerResponse::RegisterDone {
                                    info_hash,
                                    source: stream,
                                    token,
                                    interests,
                                    peer_info,
                                }
                            }
                            PollerRequest::Deregister {
                                source: mut stream,
                                token,
                            } => {
                                self.poll.registry().deregister(&mut stream).unwrap();
                                PollerResponse::DeregisterDone {
                                    source: stream,
                                    token,
                                }
                            }
                            PollerRequest::Reregister {
                                info_hash,
                                source: mut stream,
                                token,
                                interests,
                                peer_info,
                            } => {
                                self.poll
                                    .registry()
                                    .reregister(&mut stream, token, interests)
                                    .unwrap();
                                PollerResponse::ReregisterDone {
                                    info_hash,
                                    source: stream,
                                    token,
                                    interests,
                                    peer_info,
                                }
                            }
                            PollerRequest::Stop => {
                                info!("stopping NetworkPoller");
                                return;
                            }
                        };
                        self.sender
                            .send(ControllerInputMessage::Network(response))
                            .unwrap();
                    }
                    Err(_error) => {
                        break;
                        // wat do
                        // errors if no data
                    }
                }
            }
        }
    }
}
