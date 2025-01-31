use crate::discovery::{PeerAddr, Protocol};
use snafu::ResultExt;
use ssb_crypto::handshake::HandshakeKeys;
use ssb_crypto::{NetworkKey, PublicKey, SecretKey};
use ssb_handshake::HandshakeError;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::{io, thread};

use crate::box_stream::{BoxReader, BoxStreamError, BoxWriter};
use crate::peer_manager::{PeerEvent, PeerManagerEvent};

type WriterLoopHandle = thread::JoinHandle<Result<(), PeerConnectionError>>;
type ReaderLoopHandle = thread::JoinHandle<Result<(), PeerConnectionError>>;

pub struct PeerConnection {
    pub peer: PeerAddr,
    pub peer_writer_tx: mpsc::Sender<String>,
    _reader_loop_handle: ReaderLoopHandle,
    _writer_loop_handle: WriterLoopHandle,
}

type PeerMsg = String;

#[derive(Snafu, Debug)]
pub enum PeerConnectionError {
    #[snafu(display("Failed to read message from BoxReader: {}", source))]
    BoxReaderError { source: BoxStreamError },
    #[snafu(display("Failed to send message to BoxWriter: {}", source))]
    BoxWriterError { source: io::Error },
    #[snafu(display("Failed to receive peer message from channel: {}", source))]
    MsgReceiveFailed { source: mpsc::RecvError },
    #[snafu(display("Failed to perform handshake: {}", source))]
    HandshakeFailed { source: HandshakeError },
    #[snafu(display("Failed to clone TcpStream for BoxWriter: {}", source))]
    TcpStreamCloneFailed { source: io::Error },
    #[snafu(display("Timeout when attempting to connect to peer: {}", source))]
    CannotConnectToPeer { source: io::Error },
}

fn spawn_reader_loop<R>(
    tx: mpsc::Sender<PeerManagerEvent>,
    peer: PeerAddr,
    mut box_reader: BoxReader<R>,
) -> ReaderLoopHandle
where
    R: Read + Send + 'static,
{
    thread::spawn(move || -> Result<(), PeerConnectionError> {
        loop {
            let maybe_bytes = box_reader.recv().context(BoxReaderError)?;

            let peer_msg = match maybe_bytes {
                Some(raw_bytes) => String::from_utf8(raw_bytes.clone())
                    .unwrap_or(format!("Raw bytes: {:?}", raw_bytes)),
                None => "Goodbye!".to_string(),
            };

            tx.send(PeerManagerEvent {
                peer,
                event: PeerEvent::MessageReceived(peer_msg),
            });
            // should have error handling, but this only 
            // happens if the main event_bus dies ?
        }
    })
}

fn spawn_writer_loop<W>(mut box_writer: BoxWriter<W>) -> (mpsc::Sender<String>, WriterLoopHandle)
where
    W: Write + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<String>();
    let handle: WriterLoopHandle = thread::spawn(move || loop {
        let peer_msg = rx
            .recv()
            .map(String::into_bytes)
            .context(MsgReceiveFailed)?;
        box_writer.send(peer_msg).context(BoxWriterError)?;
    });

    (tx, handle)
}

impl PeerConnection {
    pub fn from_handshake<F>(
        event_bus: mpsc::Sender<PeerManagerEvent>,
        mut tcp_stream: TcpStream,
        perform_handshake: F,
    ) -> Result<PeerConnection, PeerConnectionError>
    where
        F: Fn(&mut TcpStream) -> Result<(PeerAddr, HandshakeKeys), HandshakeError> + Send + 'static,
    {
        let (peer, hs_keys) = perform_handshake(&mut tcp_stream).context(HandshakeFailed)?;

        let write_stream = tcp_stream.try_clone().context(TcpStreamCloneFailed)?;
        let mut box_writer =
            BoxWriter::new(write_stream, hs_keys.write_key, hs_keys.write_noncegen);
        let (peer_writer_tx, _writer_loop_handle) = spawn_writer_loop(box_writer);

        let mut box_reader = BoxReader::new(tcp_stream, hs_keys.read_key, hs_keys.read_noncegen);
        let _reader_loop_handle = spawn_reader_loop(event_bus.clone(), peer.clone(), box_reader);

        let peer_connection = PeerConnection {
            peer,
            peer_writer_tx,
            _reader_loop_handle,
            _writer_loop_handle,
        };

        Ok(peer_connection)
    }
}

#[derive(Clone)]
pub struct Handshaker {
    event_bus: mpsc::Sender<PeerManagerEvent>,
    public_key: PublicKey,
    secret_key: SecretKey,
    network_key: NetworkKey,
}

impl Handshaker {
    pub fn new(
        event_bus: mpsc::Sender<PeerManagerEvent>,
        public_key: PublicKey,
        secret_key: SecretKey,
        network_key: NetworkKey,
    ) -> Handshaker {
        Handshaker {
            event_bus,
            public_key,
            secret_key,
            network_key,
        }
    }

    pub fn client_handshake(&self, peer: PeerAddr) -> Result<PeerConnection, PeerConnectionError> {
        let tcp_stream =
            TcpStream::connect_timeout(&peer.socket_addr, std::time::Duration::from_millis(500))
                .context(CannotConnectToPeer)?;

        let config = self.clone();

        PeerConnection::from_handshake(self.event_bus.clone(), tcp_stream, move |stream| {
            let keys = ssb_handshake::client(
                stream,
                config.network_key.clone(),
                config.public_key,
                config.secret_key.clone(),
                peer.public_key,
            )?;
            Ok((peer.clone(), keys))
        })
    }

    pub fn server_handshake(
        &self,
        stream: TcpStream,
    ) -> Result<PeerConnection, PeerConnectionError> {
        let config = self.clone();

        PeerConnection::from_handshake(self.event_bus.clone(), stream, move |stream| {
            let client_addr = stream.peer_addr()?;

            let (client_pk, keys) = ssb_handshake::server_with_client_pk(
                stream,
                config.network_key.clone(),
                config.public_key,
                config.secret_key.clone(),
            )?;

            let peer = PeerAddr {
                public_key: client_pk,
                socket_addr: client_addr,
                protocol: Protocol::Net,
            };

            Ok((peer, keys))
        })
    }
}
