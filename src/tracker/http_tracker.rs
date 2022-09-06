use std::convert::TryInto;
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use log::{debug, info};
use reqwest;

use crate::bencoding;
use crate::common::{bytes_to_uri, PEER_ID_LENGTH};
use crate::tracker::{PeerInfo, PeerInfoList};
use write_to::ReadFrom;

use super::{EventKind, TrackerClient, TrackerResponse};

pub struct HttpTracker {
    pub address: String,
}

impl TrackerClient for HttpTracker {
    // probably needs tracker-specific errors instead of just parse errors
    fn announce(
        &mut self,
        upload: usize,
        download: usize,
        left: usize,
        listen_port: u16,
        info_hash: [u8; 20],
        peer_id: [u8; 20],
        kind: EventKind,
    ) -> Result<TrackerResponse, Box<dyn Error>> {
        debug_assert_eq!(peer_id.len(), PEER_ID_LENGTH);
        let encoded = format!(
            "info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&event={}",
            bytes_to_uri(&info_hash),
            String::from_utf8(peer_id.to_vec()).unwrap(),
            listen_port,
            download,
            upload,
            left,
            kind.to_str(),
        );

        let endpoint = format!("{}?{}", self.address, encoded);
        info!("tracker announce endpoint: {}", endpoint);

        let body = reqwest::blocking::get(&endpoint)?.bytes()?;
        debug!("response bytes: '{:?}'", body);
        let response = bencoding::parse(&body)?;
        let response_dict = response.as_dict()?;
        if let Some(failure) = response_dict.get("failure reason") {
            return Err(format!("Failed to get tracker response: {}", failure.as_utf8()?).into());
        }
        let interval = response_dict["interval"].as_int()?;
        debug!("interval: {}", interval);
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
                    peer_list.push(PeerInfo {
                        addr,
                        id: Some(id.try_into().unwrap()), // TODO: Fix unwrap
                    });
                }
            }
            bencoding::DataKind::Data(compact_peer_list) => {
                // BEP23
                let mut compact_peer_list: &[u8] = compact_peer_list;
                while compact_peer_list.len() != 0 {
                    let (ip, _) =
                        u32::read_from(&mut compact_peer_list, std::mem::size_of::<u32>())?;
                    let ip = Ipv4Addr::from(ip);
                    let (port, _) =
                        u16::read_from(&mut compact_peer_list, std::mem::size_of::<u16>())?;
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
                let (ip, _) = u128::read_from(&mut compact_peer_list, std::mem::size_of::<u128>())?;
                let ip = Ipv6Addr::from(ip);
                let (port, _) = u16::read_from(&mut compact_peer_list, std::mem::size_of::<u16>())?;
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
