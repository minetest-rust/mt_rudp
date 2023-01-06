#![feature(cursor_remaining)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]
mod client;
pub mod error;
mod new;
mod recv;
mod send;

use async_trait::async_trait;
use byteorder::{BigEndian, WriteBytesExt};
pub use client::{connect, Sender as Client};
pub use new::new;
use num_enum::TryFromPrimitive;
use pretty_hex::PrettyHex;
use std::{
    collections::HashMap,
    io::{self, Write},
    ops,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch, Mutex, RwLock},
    task::JoinSet,
};

pub const PROTO_ID: u32 = 0x4f457403;
pub const UDP_PKT_SIZE: usize = 512;
pub const NUM_CHANS: usize = 3;
pub const REL_BUFFER: usize = 0x8000;
pub const INIT_SEQNUM: u16 = 65500;
pub const TIMEOUT: u64 = 30;

mod ticker_mod {
    #[macro_export]
    macro_rules! ticker {
		($duration:expr, $close:expr, $body:block) => {
			let mut interval = tokio::time::interval($duration);

			while tokio::select!{
				_ = interval.tick() => true,
				_ = $close.changed() => false,
			} $body
		};
	}

    //pub(crate) use ticker;
}

#[async_trait]
pub trait UdpSender: Send + Sync + 'static {
    async fn send(&self, data: &[u8]) -> io::Result<()>;
}

#[async_trait]
pub trait UdpReceiver: Send + Sync + 'static {
    async fn recv(&self) -> io::Result<Vec<u8>>;
}

#[derive(Debug, Copy, Clone, PartialEq)]
#[repr(u16)]
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
struct Ack {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
    data: Vec<u8>,
}

#[derive(Debug)]
struct Chan {
    acks: HashMap<u16, Ack>,
    seqnum: u16,
}

#[derive(Debug)]
pub struct RudpShare<S: UdpSender> {
    id: u16,
    remote_id: RwLock<u16>,
    chans: Vec<Mutex<Chan>>,
    udp_tx: S,
    close_tx: watch::Sender<bool>,
    tasks: Mutex<JoinSet<()>>,
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

macro_rules! impl_share {
    ($T:ident) => {
        impl<S: UdpSender> $T<S> {
            pub async fn peer_id(&self) -> u16 {
                self.share.id
            }

            pub async fn is_server(&self) -> bool {
                self.share.id == PeerID::Srv as u16
            }

            pub async fn close(self) {
                self.share.close_tx.send(true).ok();

                let mut tasks = self.share.tasks.lock().await;
                while let Some(res) = tasks.join_next().await {
                    res.ok(); // TODO: handle error (?)
                }
            }
        }
    };
}

impl_share!(RudpReceiver);
impl_share!(RudpSender);

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

async fn example(tx: &RudpSender<Client>, rx: &mut RudpReceiver<Client>) -> io::Result<()> {
    // send hello packet
    let mut mtpkt = vec![];
    mtpkt.write_u16::<BigEndian>(2)?; // high level type
    mtpkt.write_u8(29)?; // serialize ver
    mtpkt.write_u16::<BigEndian>(0)?; // compression modes
    mtpkt.write_u16::<BigEndian>(40)?; // MinProtoVer
    mtpkt.write_u16::<BigEndian>(40)?; // MaxProtoVer
    mtpkt.write_u16::<BigEndian>(6)?; // player name length
    mtpkt.write(b"foobar")?; // player name

    tx.send(Pkt {
        unrel: true,
        chan: 1,
        data: &mtpkt,
    })
    .await?;

    // handle incoming packets
    while let Some(result) = rx.recv().await {
        match result {
            Ok(pkt) => {
                println!("{}", pkt.data.hex_dump());
            }
            Err(err) => eprintln!("Error: {}", err),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let (tx, mut rx) = connect("127.0.0.1:30000").await?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => println!("canceled"),
        res = example(&tx, &mut rx) => {
            res?;
            println!("disconnected");
        }
    }

    // close either the receiver or the sender
    // this shuts down associated tasks
    rx.close().await;

    Ok(())
}
