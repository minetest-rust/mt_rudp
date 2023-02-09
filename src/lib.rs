#![feature(cursor_remaining)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]
mod client;
mod error;
mod new;
mod recv;
mod send;

pub use prelude::*;

use async_trait::async_trait;
use num_enum::TryFromPrimitive;
use std::{cell::OnceCell, collections::HashMap, io, ops, sync::Arc, time::Instant};
use tokio::{
    sync::{mpsc, watch, Mutex, RwLock},
    task::JoinSet,
};

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
    pub unrel: bool,
    pub chan: u8,
    pub data: T,
}

pub type InPkt = Result<Pkt<Vec<u8>>, error::Error>;

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
struct RudpShare<S: UdpSender> {
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

#[derive(Debug)]
struct Split {
    timestamp: Option<Instant>,
    chunks: Vec<OnceCell<Vec<u8>>>,
    got: usize,
}

struct RecvChan {
    packets: Vec<Option<Vec<u8>>>, // char ** 😛
    splits: HashMap<u16, Split>,
    seqnum: u16,
    num: u8,
}

struct RecvWorker<R: UdpReceiver, S: UdpSender> {
    share: Arc<RudpShare<S>>,
    close: watch::Receiver<bool>,
    chans: Arc<Vec<Mutex<RecvChan>>>,
    pkt_tx: mpsc::UnboundedSender<InPkt>,
    udp_rx: R,
}

mod prelude {
    pub const PROTO_ID: u32 = 0x4f457403;
    pub const UDP_PKT_SIZE: usize = 512;
    pub const NUM_CHANS: usize = 3;
    pub const REL_BUFFER: usize = 0x8000;
    pub const INIT_SEQNUM: u16 = 65500;
    pub const TIMEOUT: u64 = 30;
    pub const PING_TIMEOUT: u64 = 5;

    pub use super::{
        client::{connect, Sender as Client},
        error::Error,
        new::new,
        CtlType, InPkt, PeerID, Pkt, PktType, RudpReceiver, RudpSender, UdpReceiver, UdpSender,
    };

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
}
