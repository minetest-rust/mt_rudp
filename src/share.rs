use super::*;
use drop_bomb::DropBomb;
use std::{borrow::Cow, collections::HashMap, io, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch, Mutex, RwLock},
    task::JoinSet,
};

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
pub(crate) struct RudpShare<S: UdpSender> {
    pub(crate) id: u16,
    pub(crate) remote_id: RwLock<u16>,
    pub(crate) chans: Vec<Mutex<Chan>>,
    pub(crate) udp_tx: S,
    pub(crate) close_tx: watch::Sender<bool>,
    pub(crate) tasks: Mutex<JoinSet<()>>,
    pub(crate) bomb: Mutex<DropBomb>,
}

pub async fn new<S: UdpSender, R: UdpReceiver>(
    id: u16,
    remote_id: u16,
    udp_tx: S,
    udp_rx: R,
) -> io::Result<(RudpSender<S>, RudpReceiver<S>)> {
    let (pkt_tx, pkt_rx) = mpsc::unbounded_channel();
    let (close_tx, close_rx) = watch::channel(false);

    let share = Arc::new(RudpShare {
        id,
        remote_id: RwLock::new(remote_id),
        udp_tx,
        close_tx,
        chans: (0..NUM_CHANS)
            .map(|_| {
                Mutex::new(Chan {
                    acks: HashMap::new(),
                    seqnum: INIT_SEQNUM,
                })
            })
            .collect(),
        tasks: Mutex::new(JoinSet::new()),
        bomb: Mutex::new(DropBomb::new("rudp connection must be explicitly closed")),
    });

    let mut tasks = share.tasks.lock().await;

    let recv_share = Arc::clone(&share);
    let recv_close = close_rx.clone();
    tasks
        /*.build_task()
        .name("recv")*/
        .spawn(async move {
            let worker = RecvWorker::new(udp_rx, recv_share, recv_close, pkt_tx);
            worker.run().await;
        });

    let resend_share = Arc::clone(&share);
    let mut resend_close = close_rx.clone();
    tasks
        /*.build_task()
        .name("resend")*/
        .spawn(async move {
            ticker!(Duration::from_millis(500), resend_close, {
                for chan in resend_share.chans.iter() {
                    for (_, ack) in chan.lock().await.acks.iter() {
                        resend_share.send_raw(&ack.data).await.ok(); // TODO: handle error (?)
                    }
                }
            });
        });

    let ping_share = Arc::clone(&share);
    let mut ping_close = close_rx.clone();
    tasks
        /*.build_task()
        .name("ping")*/
        .spawn(async move {
            ticker!(Duration::from_secs(PING_TIMEOUT), ping_close, {
                ping_share
                    .send(
                        PktType::Ctl,
                        Pkt {
                            chan: 0,
                            unrel: false,
                            data: Cow::Borrowed(&[CtlType::Ping as u8]),
                        },
                    )
                    .await
                    .ok();
            });
        });

    drop(tasks);

    Ok((
        RudpSender {
            share: Arc::clone(&share),
        },
        RudpReceiver { share, pkt_rx },
    ))
}
