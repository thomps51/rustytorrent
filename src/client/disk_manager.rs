use log::info;
use mio::net::UdpSocket;
use std::collections::HashMap;
use std::fs;
use std::io::{SeekFrom, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
};

use bit_vec::BitVec;
use mio::Token;
use std::io::{self, Read, Seek};

use crate::common::{is_valid_piece, new_udp_socket, File};

use crate::{
    common::{MetaInfo, Sha1Hash},
    messages::{Block, Request},
};

pub type AllocatedFiles = HashMap<PathBuf, fs::File>;

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum ConnectionIdentifier {
    TcpToken(Token, usize),
    UtpId(SocketAddr, u16),
}

// Going to separate Read and Write disk IO threads for now, may combine later.

// Messages sent to the Read thread
#[derive(Debug)]
pub enum DiskRequest {
    // Thread needs to read disk for these requests, and send completed reads to the main thread to process
    Request {
        info_hash: Sha1Hash,
        conn_id: ConnectionIdentifier,
        request: Request,
    },
    // Add files from a new torrent
    AddTorrent {
        meta_info: MetaInfo,
        destination: PathBuf,
    },
    WritePiece {
        info_hash: Sha1Hash,
        index: usize,
        data: Vec<u8>,
    },
    Stop,
}

// Messages sent by the Read thread
#[derive(Debug)]
pub enum DiskResponse {
    RequestCompleted {
        info_hash: Sha1Hash,
        conn_id: ConnectionIdentifier,
        block: Block,
    },
    AddTorrentCompleted {
        meta_info: MetaInfo,
        piece_have: BitVec,
    },
    WritePieceCompleted {
        info_hash: Sha1Hash,
        piece_index: usize,
        final_piece: bool,
    },
    WritePieceFailedHash {
        info_hash: Sha1Hash,
        piece_index: usize,
    },
    Error {
        message: DiskRequest,
        error: std::io::Error,
    },
}

struct TorrentData {
    files: AllocatedFiles,
    files_info: Vec<File>,
    num_pieces: usize,
    piece_length: usize,
    last_piece_length: usize,
    have: BitVec,
    have_count: usize,
    info_hash: Sha1Hash,
}

impl TorrentData {
    fn get_piece_length(&self, index: usize) -> usize {
        if index == self.num_pieces - 1 {
            self.last_piece_length
        } else {
            self.piece_length
        }
    }

    fn get_bytes(&self, index: usize, offset: usize, num_bytes: usize) -> io::Result<Vec<u8>> {
        let mut result = Vec::new();
        result.resize(num_bytes, 0u8);
        let mut current_index = 0;
        let begin_byte = index * self.piece_length + offset;
        let mut file_begin = 0;
        for file_info in &self.files_info {
            let file_end = file_begin + file_info.length;
            if begin_byte >= file_end {
                file_begin = file_end;
                continue;
            }
            let mut file = self.files.get(&file_info.path).unwrap();
            let piece_start_byte_in_file = if begin_byte < file_begin {
                0
            } else {
                begin_byte - file_begin
            };
            file.seek(SeekFrom::Start(piece_start_byte_in_file as u64))?;
            let read = file.read(&mut result[current_index..])?;
            current_index += read;
            if current_index == num_bytes {
                return Ok(result);
            }
            file_begin = file_end;
        }
        result.resize(current_index, 0);
        Ok(result)
    }

    fn write(&mut self, index: usize, data: Vec<u8>) -> io::Result<Option<DiskResponse>> {
        if self.have[index] {
            return Ok(None);
        }
        let piece_begin_byte = index * self.piece_length;
        let mut piece_current_byte = 0;
        let mut file_begin = 0;
        for file_info in self.files_info.iter() {
            let file_end = file_begin + file_info.length;
            if piece_begin_byte >= file_end {
                file_begin = file_end;
                continue;
            }
            let file = self.files.get_mut(file_info.path.as_path()).unwrap();
            let piece_start_byte_in_file = if piece_begin_byte < file_begin {
                0
            } else {
                piece_begin_byte - file_begin
            };
            file.seek(SeekFrom::Start(piece_start_byte_in_file as u64))?;
            let num_bytes_to_write = std::cmp::min(
                data.len() - piece_current_byte,
                file_end - piece_start_byte_in_file,
            );
            let start = piece_current_byte;
            let end = start + num_bytes_to_write;
            file.write_all(&data[start..end])?;
            if end == data.len() {
                file.flush()?;
                self.have.set(index, true);
                self.have_count += 1;
                return Ok(Some(DiskResponse::WritePieceCompleted {
                    info_hash: self.info_hash,
                    piece_index: index,
                    final_piece: self.have_count == self.num_pieces,
                }));
            }
            piece_current_byte = end;
            file_begin = file_end;
        }
        panic!("I don't think it should get here");
    }

    fn verify_files(&mut self, pieces: &Vec<Sha1Hash>) -> io::Result<BitVec> {
        let mut have = BitVec::from_elem(pieces.len(), false);
        let mut have_count = 0;
        for i in 0..self.num_pieces {
            let piece = self.get_bytes(i, 0, self.get_piece_length(i))?;
            if !is_valid_piece(&piece, i, pieces) {
                continue;
            }
            have_count += 1;
            have.set(i, true);
        }
        self.have_count = have_count;
        info!("Found {} pieces already locally", have_count);
        Ok(have)
    }
}

pub struct DiskManager {
    recv: Receiver<DiskRequest>,
    sender: Sender<DiskResponse>,
    send_socket: UdpSocket,
    torrents: HashMap<Sha1Hash, TorrentData>,
}

impl DiskManager {
    pub fn new(
        recv: Receiver<DiskRequest>,
        sender: Sender<DiskResponse>,
        send_socket_addr: SocketAddr,
    ) -> Self {
        let send_socket = new_udp_socket();
        send_socket.connect(send_socket_addr).unwrap();
        Self {
            recv,
            sender,
            send_socket,
            torrents: HashMap::new(),
        }
    }

    fn handle_message(&mut self, message: &DiskRequest) -> io::Result<()> {
        // TODO, hand off to thread pool
        let response = match message {
            DiskRequest::AddTorrent {
                meta_info,
                destination,
            } => self.add_torrent(meta_info, destination)?,
            DiskRequest::Request {
                info_hash,
                conn_id,
                request,
            } => self.get_block(*info_hash, *conn_id, request)?,
            DiskRequest::WritePiece {
                info_hash,
                index,
                data,
            } => self.write_piece(*info_hash, *index, data.clone())?,
            DiskRequest::Stop => panic!("Handled elsewhere"),
        };
        if let Some(response) = response {
            // log::debug!("Sending disk response: {:?}", response);
            self.sender.send(response).unwrap();
            self.send_socket.send(&[1]).unwrap();
        }
        Ok(())
    }

    pub fn run(&mut self) {
        loop {
            let message = self.recv.recv().unwrap();
            if let DiskRequest::Stop = message {
                break;
            }
            if let Err(error) = self.handle_message(&message) {
                log::debug!("Sending disk error response: {:?}", error);
                self.sender
                    .send(DiskResponse::Error { message, error })
                    .unwrap();
                self.send_socket.send(&[1]).unwrap();
            }
        }
    }

    fn add_torrent(
        &mut self,
        meta_info: &MetaInfo,
        destination: &Path,
    ) -> io::Result<Option<DiskResponse>> {
        let num_pieces = meta_info.pieces.len();
        let mut files = AllocatedFiles::new();
        info!("Adding torrent with num_pieces: {}", num_pieces);
        let mut check_files = false;
        for file in &meta_info.files {
            let path = destination.join(&file.path);
            if path.exists() && std::fs::metadata(&path)?.len() == file.length as u64 {
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)?;
                check_files = true;
                info!("Found existing file at {:?}, will check", path);
                files.insert(path.to_path_buf(), f);
            } else {
                info!("Did not find existing file");
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(&path)?;
                f.set_len(file.length as u64)?;
                files.insert(path.to_path_buf(), f);
            }
        }
        let mut last_piece_length = meta_info.total_size % meta_info.piece_length;
        if last_piece_length == 0 {
            last_piece_length = meta_info.piece_length;
        }
        // files info needs to have the full path, including destination
        let mut files_info = meta_info.files.clone();
        for file in files_info.iter_mut() {
            file.path = destination.join(&file.path);
        }
        let mut torrent_data = TorrentData {
            files,
            files_info,
            num_pieces,
            last_piece_length,
            piece_length: meta_info.piece_length,
            have: BitVec::from_elem(num_pieces, false),
            info_hash: meta_info.info_hash_raw,
            have_count: 0,
        };
        if check_files {
            let have = torrent_data.verify_files(&meta_info.pieces)?;
            torrent_data.have = have;
        }
        let result = DiskResponse::AddTorrentCompleted {
            meta_info: meta_info.clone(),
            piece_have: torrent_data.have.clone(),
        };
        self.torrents.insert(meta_info.info_hash_raw, torrent_data);
        Ok(Some(result))
    }

    fn get_block(
        &self,
        info_hash: Sha1Hash,
        conn_id: ConnectionIdentifier,
        request: &Request,
    ) -> io::Result<Option<DiskResponse>> {
        let torrent_data = if let Some(torrent_data) = self.torrents.get(&info_hash) {
            torrent_data
        } else {
            return Ok(None);
        };
        let block_data = torrent_data.get_bytes(
            request.piece_index(),
            request.offset(),
            request.requested_piece_length(),
        )?;
        let block = Block::new(request, block_data);
        let result = DiskResponse::RequestCompleted {
            info_hash,
            conn_id,
            block,
        };
        Ok(Some(result))
    }

    fn write_piece(
        &mut self,
        info_hash: Sha1Hash,
        index: usize,
        data: Vec<u8>,
    ) -> io::Result<Option<DiskResponse>> {
        let torrent_data = if let Some(torrent_data) = self.torrents.get_mut(&info_hash) {
            torrent_data
        } else {
            return Ok(None);
        };
        torrent_data.write(index, data)
    }
}
