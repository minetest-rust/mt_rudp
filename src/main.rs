#![feature(yeet_expr)]
#![feature(cursor_remaining)]
#![feature(hash_drain_filter)]
mod client;
pub mod error;
mod recv_worker;

use byteorder::{BigEndian, WriteBytesExt};
pub use client::{connect, Sender as Client};
use num_enum::TryFromPrimitive;
use std::{
    io::{self, Write},
    ops,
    sync::{mpsc, Arc},
    thread,
};

pub const PROTO_ID: u32 = 0x4f457403;
pub const UDP_PKT_SIZE: usize = 512;
pub const NUM_CHANS: usize = 3;
pub const REL_BUFFER: usize = 0x8000;
pub const INIT_SEQNUM: u16 = 65500;
pub const TIMEOUT: u64 = 30;

pub trait UdpSender: Send + Sync + 'static {
    fn send(&self, data: Vec<u8>) -> io::Result<()>;
}

pub trait UdpReceiver: Send + Sync + 'static {
    fn recv(&self) -> io::Result<Vec<u8>>;
}

#[derive(Debug, Copy, Clone)]
pub enum PeerID {
    Nil = 0,
    Srv,
    CltMin,
}

#[derive(Debug, Copy, Clone, PartialEq, TryFromPrimitive)]
#[repr(u8)]
pub enum PktType {
    Ctl = 0,
    Orig,
    Split,
    Rel,
}

#[derive(Debug, Copy, Clone, PartialEq, TryFromPrimitive)]
#[repr(u8)]
pub enum CtlType {
    Ack = 0,
    SetPeerID,
    Ping,
    Disco,
}

#[derive(Debug)]
pub struct Pkt<T> {
    unrel: bool,
    chan: u8,
    data: T,
}

pub type Error = error::Error;
pub type InPkt = Result<Pkt<Vec<u8>>, Error>;

#[derive(Debug)]
pub struct AckChan;

#[derive(Debug)]
pub struct RudpShare<S: UdpSender> {
    pub id: u16,
    pub remote_id: u16,
    pub chans: Vec<AckChan>,
    udp_tx: S,
}

#[derive(Debug)]
pub struct RudpReceiver<S: UdpSender> {
    share: Arc<RudpShare<S>>,
    pkt_rx: mpsc::Receiver<InPkt>,
}

#[derive(Debug)]
pub struct RudpSender<S: UdpSender> {
    share: Arc<RudpShare<S>>,
}

impl<S: UdpSender> RudpShare<S> {
    pub fn send(&self, tp: PktType, pkt: Pkt<&[u8]>) -> io::Result<()> {
        let mut buf = Vec::with_capacity(4 + 2 + 1 + 1 + pkt.data.len());
        buf.write_u32::<BigEndian>(PROTO_ID)?;
        buf.write_u16::<BigEndian>(self.remote_id)?;
        buf.write_u8(pkt.chan as u8)?;
        buf.write_u8(tp as u8)?;
        buf.write(pkt.data)?;

        self.udp_tx.send(buf)?;

        Ok(())
    }
}

impl<S: UdpSender> RudpSender<S> {
    pub fn send(&self, pkt: Pkt<&[u8]>) -> io::Result<()> {
        self.share.send(PktType::Orig, pkt) // TODO
    }
}

impl<S: UdpSender> ops::Deref for RudpReceiver<S> {
    type Target = mpsc::Receiver<InPkt>;

    fn deref(&self) -> &Self::Target {
        &self.pkt_rx
    }
}

pub fn new<S: UdpSender, R: UdpReceiver>(
    id: u16,
    remote_id: u16,
    udp_tx: S,
    udp_rx: R,
) -> (RudpSender<S>, RudpReceiver<S>) {
    let (pkt_tx, pkt_rx) = mpsc::channel();

    let share = Arc::new(RudpShare {
        id,
        remote_id,
        udp_tx,
        chans: (0..NUM_CHANS).map(|_| AckChan).collect(),
    });
    let recv_share = Arc::clone(&share);

    thread::spawn(|| {
        recv_worker::RecvWorker::new(udp_rx, recv_share, pkt_tx).run();
    });

    (
        RudpSender {
            share: Arc::clone(&share),
        },
        RudpReceiver { share, pkt_rx },
    )
}

// connect

fn main() -> io::Result<()> {
    //println!("{}", x.deep_size_of());
    let (tx, rx) = connect("127.0.0.1:30000")?;

    let mut mtpkt = vec![];
    mtpkt.write_u16::<BigEndian>(2)?; // high level type
    mtpkt.write_u8(29)?; // serialize ver
    mtpkt.write_u16::<BigEndian>(0)?; // compression modes
    mtpkt.write_u16::<BigEndian>(40)?; // MinProtoVer
    mtpkt.write_u16::<BigEndian>(40)?; // MaxProtoVer
    mtpkt.write_u16::<BigEndian>(3)?; // player name length
    mtpkt.write(b"foo")?; // player name

    tx.send(Pkt {
        unrel: true,
        chan: 1,
        data: &mtpkt,
    })?;

    while let Ok(result) = rx.recv() {
        match result {
            Ok(pkt) => {
                io::stdout().write(pkt.data.as_slice())?;
            }
            Err(err) => eprintln!("Error: {}", err),
        }
    }
    println!("disco");

    Ok(())
}
