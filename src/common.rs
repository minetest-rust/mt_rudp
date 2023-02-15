use super::*;
use async_trait::async_trait;
use delegate::delegate;
use num_enum::TryFromPrimitive;
use std::{io, sync::Arc};
use tokio::sync::mpsc;

pub const PROTO_ID: u32 = 0x4f457403;
pub const UDP_PKT_SIZE: usize = 512;
pub const NUM_CHANS: usize = 3;
pub const REL_BUFFER: usize = 0x8000;
pub const INIT_SEQNUM: u16 = 65500;
pub const TIMEOUT: u64 = 30;
pub const PING_TIMEOUT: u64 = 5;

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
pub struct RudpReceiver<S: UdpSender> {
    pub(crate) share: Arc<RudpShare<S>>,
    pub(crate) pkt_rx: mpsc::UnboundedReceiver<InPkt>,
}

#[derive(Debug)]
pub struct RudpSender<S: UdpSender> {
    pub(crate) share: Arc<RudpShare<S>>,
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

impl<S: UdpSender> RudpReceiver<S> {
    delegate! {
        to self.pkt_rx {
            pub async fn recv(&mut self) -> Option<InPkt>;
        }
    }
}
