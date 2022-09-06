// Messages for UDP Tracker

use write_to::{Length, NormalizedIntegerAccessors, ReadFrom, WriteTo};

type InfoHash = [u8; 20];
type PeerId = [u8; 20];

// macro_rules! derive_message {
//     ($ID: literal, $NAME:ident, $($element: ident: $ty: ty),*) => {
//         #[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
//         pub struct $NAME {
//             $(pub $element: $ty),*
//         }
//     };
// }

#[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
pub struct ConnectRequest {
    pub protocol_id: i64,
    pub action: i32,
    pub transaction_id: i32,
}

#[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
pub struct ConnectResponse {
    pub action: i32,
    pub transaction_id: i32,
    pub connection_id: i64,
}

#[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
pub struct AnnounceRequest {
    pub connection_id: i64,
    pub action: i32,
    pub transaction_id: i32,
    pub info_hash: InfoHash,
    pub peer_id: PeerId,
    pub downloaded: i64,
    pub left: i64,
    pub uploaded: i64,
    pub event: i32,
    pub ip_address: u32,
    pub key: u32,
    pub num_want: i32,
    pub port: u16,
}

#[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
pub struct Endpoint {
    pub ip: i32,
    pub port: u16,
}

#[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
pub struct AnnounceResponse {
    pub action: i32,
    pub transaction_id: i32,
    pub interval: i32,
    pub leechers: i32,
    pub seeders: i32,
    pub peers: Vec<Endpoint>,
}

#[derive(Debug, WriteTo, ReadFrom, Length, NormalizedIntegerAccessors, Clone)]
pub struct ErrorResponse {
    pub action: i32,
    pub transaction_id: i32,
    pub error_string: Vec<u8>,
}
