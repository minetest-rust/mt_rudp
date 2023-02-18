use super::*;
use std::{borrow::Cow, collections::HashMap, io, sync::Arc};
use tokio::sync::{watch, Mutex, RwLock};

#[derive(Debug)]
pub(crate) struct Ack {
    pub(crate) tx: watch::Sender<bool>,
    pub(crate) rx: watch::Receiver<bool>,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Chan {
    pub(crate) acks: HashMap<u16, Ack>,
    pub(crate) seqnum: u16,
}

#[derive(Debug)]
pub(crate) struct RudpShare<P: UdpPeer> {
    pub(crate) id: u16,
    pub(crate) remote_id: RwLock<u16>,
    pub(crate) chans: [Mutex<Chan>; NUM_CHANS],
    pub(crate) udp_tx: P::Sender,
    pub(crate) close: watch::Sender<bool>,
}

pub async fn new<P: UdpPeer>(
    id: u16,
    remote_id: u16,
    udp_tx: P::Sender,
    udp_rx: P::Receiver,
) -> io::Result<(RudpSender<P>, RudpReceiver<P>)> {
    let (close_tx, close_rx) = watch::channel(false);

    let share = Arc::new(RudpShare {
        id,
        remote_id: RwLock::new(remote_id),
        udp_tx,
        close: close_tx,
        chans: std::array::from_fn(|_| {
            Mutex::new(Chan {
                acks: HashMap::new(),
                seqnum: INIT_SEQNUM,
            })
        }),
    });

    Ok((
        RudpSender {
            share: Arc::clone(&share),
        },
        RudpReceiver::new(udp_rx, share, close_rx),
    ))
}

macro_rules! impl_share {
    ($T:ident) => {
        impl<P: UdpPeer> $T<P> {
            pub async fn peer_id(&self) -> u16 {
                self.share.id
            }

            pub async fn is_server(&self) -> bool {
                self.share.id == PeerID::Srv as u16
            }

            pub async fn close(self) {
                self.share.close.send(true).ok(); // FIXME: handle err?

                self.share
                    .send(
                        PktType::Ctl,
                        Pkt {
                            unrel: true,
                            chan: 0,
                            data: Cow::Borrowed(&[CtlType::Disco as u8]),
                        },
                    )
                    .await
                    .ok();
            }
        }
    };
}

impl_share!(RudpReceiver);
impl_share!(RudpSender);
