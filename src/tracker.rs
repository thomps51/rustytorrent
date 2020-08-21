use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::str::FromStr;

use log::debug;
use reqwest;

use crate::bencoding;
use crate::torrent::Torrent;

pub struct Tracker {
    pub address: String,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub addr: SocketAddr,
    pub id: Option<Vec<u8>>,
}

pub type PeerInfoList = Vec<PeerInfo>;

#[derive(Debug)]
pub struct TrackerResponse {
    pub peer_list: PeerInfoList,
    interval: i64,
}

pub enum EventKind {
    Started,
    Completed,
    Stopped,
    Empty,
}

impl EventKind {
    fn to_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::Empty => "empty",
        }
    }
}

impl Tracker {
    // make this async for updates other than the first?
    // probably needs tracker-specific errors instead of just parse errors
    pub fn announce(
        &self,
        torrent: &Torrent,
        kind: EventKind,
    ) -> Result<TrackerResponse, Box<dyn Error>> {
        debug_assert_eq!(crate::PEER_ID.len(), 20);
        let encoded = format!(
            "info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&event={}&compact=0",
            torrent.metainfo.info_hash_uri,
            crate::PEER_ID,
            crate::LISTEN_PORT.to_string(),
            torrent.downloaded.to_string(),
            torrent.uploaded.to_string(),
            torrent.left.to_string(),
            kind.to_str(),
        );

        let endpoint = format!("{}?{}", self.address, encoded);
        println!("tracker announce endpoint: {}", endpoint);

        let response_dict = if crate::TEST_MODE {
            bencoding::parse_into_dictionary(Path::new("sample_response.txt"))?
        } else {
            let body = reqwest::blocking::get(&endpoint)?.bytes()?;
            println!("response bytes: '{:?}'", body);
            let response = bencoding::parse(&body)?;
            response.as_dict()?.clone()
        };
        if let Some(failure) = response_dict.get("failure reason") {
            return Err(format!("Failed to get tracker response: {}", failure.as_utf8()?).into());
        }
        let interval = response_dict["interval"].as_int()?;
        println!("interval: {}", interval);
        let mut peer_list = PeerInfoList::new();
        match &response_dict["peers"] {
            bencoding::DataKind::List(peer_list_raw) => {
                for peer in peer_list_raw {
                    let dict = peer.as_dict()?;
                    let ip = dict["ip"].as_utf8()?;
                    let id = dict["peer id"].as_bytes()?.to_owned();
                    let port = dict["port"].as_int()?;
                    let ip_address = IpAddr::from_str(ip)?;
                    let addr = SocketAddr::new(ip_address, port as u16);
                    debug!("ip: {}, port: {}, id {:?}", ip, port, id);
                    peer_list.push(PeerInfo { addr, id: Some(id) });
                }
            }
            bencoding::DataKind::Data(compact_peer_list) => {
                // BEP23
                let mut compact_peer_list: &[u8] = compact_peer_list;
                while compact_peer_list.len() != 0 {
                    use crate::messages::read_as_be;
                    let ip: u32 = read_as_be(&mut compact_peer_list).unwrap();
                    let ip = Ipv4Addr::from(ip);
                    let port = read_as_be(&mut compact_peer_list).unwrap();
                    let addr = SocketAddr::from((ip, port));
                    peer_list.push(PeerInfo { addr, id: None });
                }
            }
            _ => panic!("Unexpected type in peers response from tracker"),
        }
        if let Some(raw_v6_addrs) = response_dict.get("peers6") {
            // BEP7
            let mut compact_peer_list = raw_v6_addrs.as_bytes().unwrap();
            while compact_peer_list.len() != 0 {
                use crate::messages::read_as_be;
                let ip: u128 = read_as_be(&mut compact_peer_list).unwrap();
                let ip = Ipv6Addr::from(ip);
                let port = read_as_be(&mut compact_peer_list).unwrap();
                let addr = SocketAddr::from((ip, port));
                peer_list.push(PeerInfo { addr, id: None });
            }
        }
        Ok(TrackerResponse {
            interval,
            peer_list,
        })
    }
}
