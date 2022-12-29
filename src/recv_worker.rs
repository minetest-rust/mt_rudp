use crate::{error::Error, *};
use async_recursion::async_recursion;
use byteorder::{BigEndian, ReadBytesExt};
use std::{
    cell::{Cell, OnceCell},
    collections::HashMap,
    io,
    sync::{Arc, Weak},
    time,
};
use tokio::sync::{mpsc, Mutex};

fn to_seqnum(seqnum: u16) -> usize {
    (seqnum as usize) & (REL_BUFFER - 1)
}

type Result<T> = std::result::Result<T, Error>;

struct Split {
    timestamp: Option<time::Instant>,
    chunks: Vec<OnceCell<Vec<u8>>>,
    got: usize,
}

struct Chan {
    packets: Vec<Cell<Option<Vec<u8>>>>, // char ** 😛
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
    pub fn new(udp_rx: R, share: Arc<RudpShare<S>>, pkt_tx: mpsc::UnboundedSender<InPkt>) -> Self {
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
                        .drain_filter(
                            |_k, v| !matches!(v.timestamp, Some(t) if t.elapsed() < timeout),
                        )
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

    async fn recv_pkt(&self) -> Result<()> {
        use Error::*;

        // todo: reset timeout
        let mut cursor = io::Cursor::new(self.udp_rx.recv().await?);

        let proto_id = cursor.read_u32::<BigEndian>()?;
        if proto_id != PROTO_ID {
            return Err(InvalidProtoId(proto_id));
        }

        let _peer_id = cursor.read_u16::<BigEndian>()?;

        let n_chan = cursor.read_u8()?;
        let mut chan = self
            .chans
            .get(n_chan as usize)
            .ok_or(InvalidChannel(n_chan))?
            .lock()
            .await;

        self.process_pkt(cursor, true, &mut chan).await
    }

    #[async_recursion]
    async fn process_pkt(
        &self,
        mut cursor: io::Cursor<Vec<u8>>,
        unrel: bool,
        chan: &mut Chan,
    ) -> Result<()> {
        use Error::*;

        match cursor.read_u8()?.try_into()? {
            PktType::Ctl => match cursor.read_u8()?.try_into()? {
                CtlType::Ack => { /* TODO */ }
                CtlType::SetPeerID => {
                    let mut id = self.share.remote_id.write().await;

                    if *id != PeerID::Nil as u16 {
                        return Err(PeerIDAlreadySet);
                    }

                    *id = cursor.read_u16::<BigEndian>()?;
                }
                CtlType::Ping => {}
                CtlType::Disco => return Err(RemoteDisco),
            },
            PktType::Orig => {
                println!("Orig");

                self.pkt_tx.send(Ok(Pkt {
                    chan: chan.num,
                    unrel,
                    data: cursor.remaining_slice().into(),
                }))?;
            }
            PktType::Split => {
                println!("Split");

                let seqnum = cursor.read_u16::<BigEndian>()?;
                let chunk_index = cursor.read_u16::<BigEndian>()? as usize;
                let chunk_count = cursor.read_u16::<BigEndian>()? as usize;

                let mut split = chan.splits.entry(seqnum).or_insert_with(|| Split {
                    got: 0,
                    chunks: (0..chunk_count).map(|_| OnceCell::new()).collect(),
                    timestamp: None,
                });

                if split.chunks.len() != chunk_count {
                    return Err(InvalidChunkCount(split.chunks.len(), chunk_count));
                }

                if split
                    .chunks
                    .get(chunk_index)
                    .ok_or(InvalidChunkIndex(chunk_index, chunk_count))?
                    .set(cursor.remaining_slice().into())
                    .is_ok()
                {
                    split.got += 1;
                }

                split.timestamp = if unrel {
                    Some(time::Instant::now())
                } else {
                    None
                };

                if split.got == chunk_count {
                    self.pkt_tx.send(Ok(Pkt {
                        chan: chan.num,
                        unrel,
                        data: split
                            .chunks
                            .iter()
                            .flat_map(|chunk| chunk.get().unwrap().iter())
                            .copied()
                            .collect(),
                    }))?;

                    chan.splits.remove(&seqnum);
                }
            }
            PktType::Rel => {
                println!("Rel");

                let seqnum = cursor.read_u16::<BigEndian>()?;
                chan.packets[to_seqnum(seqnum)].set(Some(cursor.remaining_slice().into()));

                fn next_pkt(chan: &mut Chan) -> Option<Vec<u8>> {
                    chan.packets[to_seqnum(chan.seqnum)].take()
                }

                while let Some(pkt) = next_pkt(chan) {
                    self.handle(self.process_pkt(io::Cursor::new(pkt), false, chan).await)?;
                    chan.seqnum = chan.seqnum.overflowing_add(1).0;
                }
            }
        }

        Ok(())
    }

    fn handle(&self, res: Result<()>) -> Result<()> {
        use Error::*;

        match res {
            Ok(v) => Ok(v),
            Err(RemoteDisco) => Err(RemoteDisco),
            Err(LocalDisco) => Err(LocalDisco),
            Err(e) => Ok(self.pkt_tx.send(Err(e))?),
        }
    }
}
