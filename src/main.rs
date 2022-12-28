#![feature(yeet_expr)]
#![feature(cursor_remaining)]
#![feature(hash_drain_filter)]
mod client;
pub mod error;
mod recv_worker;

use async_trait::async_trait;
use byteorder::{BigEndian, WriteBytesExt};
pub use client::{connect, Sender as Client};
use num_enum::TryFromPrimitive;
use std::future::Future;
use std::{
    io::{self, Write},
    ops,
    sync::Arc,
};
use tokio::sync::mpsc;

pub const PROTO_ID: u32 = 0x4f457403;
pub const UDP_PKT_SIZE: usize = 512;
pub const NUM_CHANS: usize = 3;
pub const REL_BUFFER: usize = 0x8000;
pub const INIT_SEQNUM: u16 = 65500;
pub const TIMEOUT: u64 = 30;

#[async_trait]
pub trait UdpSender: Send + Sync + 'static {
    async fn send(&self, data: Vec<u8>) -> io::Result<()>;
}

#[async_trait]
pub trait UdpReceiver: Send + Sync + 'static {
    async fn recv(&self) -> io::Result<Vec<u8>>;
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
    pkt_rx: mpsc::UnboundedReceiver<InPkt>,
}

#[derive(Debug)]
pub struct RudpSender<S: UdpSender> {
    share: Arc<RudpShare<S>>,
}

impl<S: UdpSender> RudpShare<S> {
    pub async fn send(&self, tp: PktType, pkt: Pkt<&[u8]>) -> io::Result<()> {
        let mut buf = Vec::with_capacity(4 + 2 + 1 + 1 + pkt.data.len());
        buf.write_u32::<BigEndian>(PROTO_ID)?;
        buf.write_u16::<BigEndian>(self.remote_id)?;
        buf.write_u8(pkt.chan as u8)?;
        buf.write_u8(tp as u8)?;
        buf.write(pkt.data)?;

        self.udp_tx.send(buf).await?;

        Ok(())
    }
}

impl<S: UdpSender> RudpSender<S> {
    pub async fn send(&self, pkt: Pkt<&[u8]>) -> io::Result<()> {
        self.share.send(PktType::Orig, pkt).await // TODO
    }
}

impl<S: UdpSender> ops::Deref for RudpReceiver<S> {
    type Target = mpsc::UnboundedReceiver<InPkt>;

    fn deref(&self) -> &Self::Target {
        &self.pkt_rx
    }
}

impl<S: UdpSender> ops::DerefMut for RudpReceiver<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.pkt_rx
    }
}

pub fn new<S: UdpSender, R: UdpReceiver>(
    id: u16,
    remote_id: u16,
    udp_tx: S,
    udp_rx: R,
) -> (RudpSender<S>, RudpReceiver<S>) {
    let (pkt_tx, pkt_rx) = mpsc::unbounded_channel();

    let share = Arc::new(RudpShare {
        id,
        remote_id,
        udp_tx,
        chans: (0..NUM_CHANS).map(|_| AckChan).collect(),
    });
    let recv_share = Arc::clone(&share);

    tokio::spawn(async {
        let worker = recv_worker::RecvWorker::new(udp_rx, recv_share, pkt_tx);
        worker.run().await;
    });

    (
        RudpSender {
            share: Arc::clone(&share),
        },
        RudpReceiver { share, pkt_rx },
    )
}

// connect

#[tokio::main]
async fn main() -> io::Result<()> {
    //println!("{}", x.deep_size_of());
    let (tx, mut rx) = connect("127.0.0.1:30000").await?;

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
    })
    .await?;

    while let Some(result) = rx.recv().await {
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
