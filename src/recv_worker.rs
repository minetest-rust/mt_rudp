use crate::{error::Error, *};
use byteorder::{BigEndian, ReadBytesExt};
use std::{
    cell::Cell,
    collections::HashMap,
    io,
    sync::{Arc, Weak},
    time,
};
use tokio::sync::{mpsc, Mutex};

fn to_seqnum(seqnum: u16) -> usize {
    (seqnum as usize) & (REL_BUFFER - 1)
}

type Result = std::result::Result<(), Error>;

struct Split {
    timestamp: time::Instant,
}

struct Chan {
    packets: Vec<Cell<Option<Vec<u8>>>>, // in the good old days this used to be called char **
    splits: HashMap<u16, Split>,
    seqnum: u16,
    num: u8,
}

pub struct RecvWorker<R: UdpReceiver, S: UdpSender> {
    share: Arc<RudpShare<S>>,
    chans: Arc<Vec<Mutex<Chan>>>,
    pkt_tx: mpsc::UnboundedSender<InPkt>,
    udp_rx: R,
}

impl<R: UdpReceiver, S: UdpSender> RecvWorker<R, S> {
    pub async fn new(udp_rx: R, share: Arc<RudpShare<S>>, pkt_tx: mpsc::UnboundedSender<InPkt>) {
        Self {
            udp_rx,
            share,
            pkt_tx,
            chans: Arc::new(
                (0..NUM_CHANS as u8)
                    .map(|num| {
                        Mutex::new(Chan {
                            num,
                            packets: (0..REL_BUFFER).map(|_| Cell::new(None)).collect(),
                            seqnum: INIT_SEQNUM,
                            splits: HashMap::new(),
                        })
                    })
                    .collect(),
            ),
        }
        .run()
        .await
    }

    pub async fn run(&self) {
        let cleanup_chans = Arc::downgrade(&self.chans);
        tokio::spawn(async move {
            let timeout = time::Duration::from_secs(TIMEOUT);
            let mut interval = tokio::time::interval(timeout);

            while let Some(chans) = Weak::upgrade(&cleanup_chans) {
                for chan in chans.iter() {
                    let mut ch = chan.lock().await;
                    ch.splits = ch
                        .splits
                        .drain_filter(|_k, v| v.timestamp.elapsed() < timeout)
                        .collect();
                }

                interval.tick().await;
            }
        });

        loop {
            if let Err(e) = self.handle(self.recv_pkt().await) {
                if let Error::LocalDisco = e {
                    self.share
                        .send(
                            PktType::Ctl,
                            Pkt {
                                unrel: true,
                                chan: 0,
                                data: &[CtlType::Disco as u8],
                            },
                        )
                        .await
                        .ok();
                }
                break;
            }
        }
    }

    async fn recv_pkt(&self) -> Result {
        use Error::*;

        // todo: reset timeout
        let mut cursor = io::Cursor::new(self.udp_rx.recv().await?);

        let proto_id = cursor.read_u32::<BigEndian>()?;
        if proto_id != PROTO_ID {
            do yeet InvalidProtoId(proto_id);
        }

        let peer_id = cursor.read_u16::<BigEndian>()?;

        let n_chan = cursor.read_u8()?;
        let mut chan = self
            .chans
            .get(n_chan as usize)
            .ok_or(InvalidChannel(n_chan))?
            .lock()
            .await;

        self.process_pkt(cursor, &mut chan)
    }

    fn process_pkt(&self, mut cursor: io::Cursor<Vec<u8>>, chan: &mut Chan) -> Result {
        use CtlType::*;
        use Error::*;
        use PktType::*;

        match cursor.read_u8()?.try_into()? {
            Ctl => match cursor.read_u8()?.try_into()? {
                Disco => return Err(RemoteDisco),
                _ => {}
            },
            Orig => {
                println!("Orig");

                self.pkt_tx.send(Ok(Pkt {
                    chan: chan.num,
                    unrel: true,
                    data: cursor.remaining_slice().into(),
                }))?;
            }
            Split => {
                println!("Split");
                dbg!(cursor.remaining_slice());
            }
            Rel => {
                println!("Rel");

                let seqnum = cursor.read_u16::<BigEndian>()?;
                chan.packets[to_seqnum(seqnum)].set(Some(cursor.remaining_slice().into()));

                while let Some(pkt) = chan.packets[to_seqnum(chan.seqnum)].take() {
                    self.handle(self.process_pkt(io::Cursor::new(pkt), chan))?;
                    chan.seqnum = chan.seqnum.overflowing_add(1).0;
                }
            }
        }

        Ok(())
    }

    fn handle(&self, res: Result) -> Result {
        use Error::*;

        match res {
            Ok(v) => Ok(v),
            Err(RemoteDisco) => Err(RemoteDisco),
            Err(LocalDisco) => Err(LocalDisco),
            Err(e) => Ok(self.pkt_tx.send(Err(e))?),
        }
    }
}
