use super::*;
use byteorder::{BigEndian, WriteBytesExt};
use std::{
    collections::HashMap,
    io::{self, Write},
    sync::Arc,
};
use tokio::sync::{watch, Mutex, RwLock};

pub type Ack = Option<watch::Receiver<bool>>;

#[derive(Debug)]
pub(crate) struct AckWait {
    pub(crate) tx: watch::Sender<bool>,
    pub(crate) rx: watch::Receiver<bool>,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Chan {
    pub(crate) acks: HashMap<u16, AckWait>,
    pub(crate) seqnum: u16,
}

#[derive(Debug)]
pub struct Sender<S: UdpSender> {
    pub(crate) id: u16,
    pub(crate) remote_id: RwLock<u16>,
    pub(crate) chans: [Mutex<Chan>; NUM_CHANS],
    udp: S,
    close: watch::Sender<bool>,
}

impl<S: UdpSender> Sender<S> {
    pub fn new(udp: S, close: watch::Sender<bool>, id: u16, remote_id: u16) -> Arc<Self> {
        Arc::new(Self {
            id,
            remote_id: RwLock::new(remote_id),
            udp,
            close,
            chans: std::array::from_fn(|_| {
                Mutex::new(Chan {
                    acks: HashMap::new(),
                    seqnum: INIT_SEQNUM,
                })
            }),
        })
    }

    pub async fn send_rudp(&self, pkt: Pkt<'_>) -> io::Result<Ack> {
        self.send_rudp_type(PktType::Orig, pkt).await // TODO: splits
    }

    pub async fn send_rudp_type(&self, tp: PktType, pkt: Pkt<'_>) -> io::Result<Ack> {
        let mut buf = Vec::with_capacity(4 + 2 + 1 + 1 + 2 + 1 + pkt.data.len());
        buf.write_u32::<BigEndian>(PROTO_ID)?;
        buf.write_u16::<BigEndian>(*self.remote_id.read().await)?;
        buf.write_u8(pkt.chan)?;

        let mut chan = self.chans[pkt.chan as usize].lock().await;
        let seqnum = chan.seqnum;

        if !pkt.unrel {
            buf.write_u8(PktType::Rel as u8)?;
            buf.write_u16::<BigEndian>(seqnum)?;
        }

        buf.write_u8(tp as u8)?;
        buf.write_all(pkt.data.as_ref())?;

        self.send_udp(&buf).await?;

        if pkt.unrel {
            Ok(None)
        } else {
            // TODO: reliable window
            let (tx, rx) = watch::channel(false);
            chan.acks.insert(
                seqnum,
                AckWait {
                    tx,
                    rx: rx.clone(),
                    data: buf,
                },
            );
            chan.seqnum = chan.seqnum.overflowing_add(1).0;

            Ok(Some(rx))
        }
    }

    pub async fn send_udp(&self, data: &[u8]) -> io::Result<()> {
        if data.len() > UDP_PKT_SIZE {
            panic!("splitting packets is not implemented yet");
        }

        self.udp.send(data).await
    }

    pub async fn peer_id(&self) -> u16 {
        self.id
    }

    pub async fn is_server(&self) -> bool {
        self.id == PeerID::Srv as u16
    }

    pub fn close(&self) {
        self.close.send(true).ok();
    }
}
