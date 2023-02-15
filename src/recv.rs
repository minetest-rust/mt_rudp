use super::*;
use async_recursion::async_recursion;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::{
    borrow::Cow,
    cell::OnceCell,
    collections::HashMap,
    io,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch, Mutex};

fn to_seqnum(seqnum: u16) -> usize {
    (seqnum as usize) & (REL_BUFFER - 1)
}

type Result<T> = std::result::Result<T, Error>;

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

pub(crate) struct RecvWorker<R: UdpReceiver, S: UdpSender> {
    share: Arc<RudpShare<S>>,
    close: watch::Receiver<bool>,
    chans: Arc<Vec<Mutex<RecvChan>>>,
    pkt_tx: mpsc::UnboundedSender<InPkt>,
    udp_rx: R,
}

impl<R: UdpReceiver, S: UdpSender> RecvWorker<R, S> {
    pub fn new(
        udp_rx: R,
        share: Arc<RudpShare<S>>,
        close: watch::Receiver<bool>,
        pkt_tx: mpsc::UnboundedSender<InPkt>,
    ) -> Self {
        Self {
            udp_rx,
            share,
            close,
            pkt_tx,
            chans: Arc::new(
                (0..NUM_CHANS as u8)
                    .map(|num| {
                        Mutex::new(RecvChan {
                            num,
                            packets: (0..REL_BUFFER).map(|_| None).collect(),
                            seqnum: INIT_SEQNUM,
                            splits: HashMap::new(),
                        })
                    })
                    .collect(),
            ),
        }
    }

    pub async fn run(&self) {
        use Error::*;

        let cleanup_chans = Arc::clone(&self.chans);
        let mut cleanup_close = self.close.clone();
        self.share
            .tasks
            .lock()
            .await
            /*.build_task()
            .name("cleanup_splits")*/
            .spawn(async move {
                let timeout = Duration::from_secs(TIMEOUT);

                ticker!(timeout, cleanup_close, {
                    for chan_mtx in cleanup_chans.iter() {
                        let mut chan = chan_mtx.lock().await;
                        chan.splits = chan
                            .splits
                            .drain_filter(
                                |_k, v| !matches!(v.timestamp, Some(t) if t.elapsed() < timeout),
                            )
                            .collect();
                    }
                });
            });

        let mut close = self.close.clone();
        let timeout = tokio::time::sleep(Duration::from_secs(TIMEOUT));
        tokio::pin!(timeout);

        loop {
            if let Err(e) = self.handle(self.recv_pkt(&mut close, timeout.as_mut()).await) {
                // TODO: figure out whether this is a good idea
                if let RemoteDisco(to) = e {
                    self.pkt_tx.send(Err(RemoteDisco(to))).ok();
                }

                #[allow(clippy::single_match)]
                match e {
					// anon5's mt notifies the peer on timeout, C++ MT does not
					LocalDisco /*| RemoteDisco(true)*/ => drop(
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
							.ok(),
					),
					_ => {}
				}

                break;
            }
        }
    }

    async fn recv_pkt(
        &self,
        close: &mut watch::Receiver<bool>,
        timeout: Pin<&mut tokio::time::Sleep>,
    ) -> Result<()> {
        use Error::*;

        let mut cursor = io::Cursor::new(tokio::select! {
            pkt = self.udp_rx.recv() => pkt?,
            _ = tokio::time::sleep_until(timeout.deadline()) => return Err(RemoteDisco(true)),
            _ = close.changed() => return Err(LocalDisco),
        });

        timeout.reset(tokio::time::Instant::now() + Duration::from_secs(TIMEOUT));

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
        chan: &mut RecvChan,
    ) -> Result<()> {
        use Error::*;

        match cursor.read_u8()?.try_into()? {
            PktType::Ctl => match cursor.read_u8()?.try_into()? {
                CtlType::Ack => {
                    println!("Ack");

                    let seqnum = cursor.read_u16::<BigEndian>()?;
                    if let Some(ack) = self.share.chans[chan.num as usize]
                        .lock()
                        .await
                        .acks
                        .remove(&seqnum)
                    {
                        ack.tx.send(true).ok();
                    }
                }
                CtlType::SetPeerID => {
                    println!("SetPeerID");

                    let mut id = self.share.remote_id.write().await;

                    if *id != PeerID::Nil as u16 {
                        return Err(PeerIDAlreadySet);
                    }

                    *id = cursor.read_u16::<BigEndian>()?;
                }
                CtlType::Ping => {
                    println!("Ping");
                }
                CtlType::Disco => {
                    println!("Disco");
                    return Err(RemoteDisco(false));
                }
            },
            PktType::Orig => {
                println!("Orig");

                self.pkt_tx.send(Ok(Pkt {
                    chan: chan.num,
                    unrel,
                    data: Cow::Owned(cursor.remaining_slice().into()),
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

                split.timestamp = if unrel { Some(Instant::now()) } else { None };

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
                chan.packets[to_seqnum(seqnum)].replace(cursor.remaining_slice().into());

                let mut ack_data = Vec::with_capacity(3);
                ack_data.write_u8(CtlType::Ack as u8)?;
                ack_data.write_u16::<BigEndian>(seqnum)?;

                self.share
                    .send(
                        PktType::Ctl,
                        Pkt {
                            unrel: true,
                            chan: chan.num,
                            data: Cow::Borrowed(&ack_data),
                        },
                    )
                    .await?;

                fn next_pkt(chan: &mut RecvChan) -> Option<Vec<u8>> {
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
            Err(RemoteDisco(to)) => Err(RemoteDisco(to)),
            Err(LocalDisco) => Err(LocalDisco),
            Err(e) => Ok(self.pkt_tx.send(Err(e))?),
        }
    }
}
