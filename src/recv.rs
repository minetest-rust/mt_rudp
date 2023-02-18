use super::*;
use async_recursion::async_recursion;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    io,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::watch,
    time::{interval, sleep, Interval, Sleep},
};

fn to_seqnum(seqnum: u16) -> usize {
    (seqnum as usize) & (REL_BUFFER - 1)
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
struct Split {
    timestamp: Option<Instant>,
    chunks: Vec<Option<Vec<u8>>>,
    got: usize,
}

struct RecvChan {
    packets: Vec<Option<Vec<u8>>>, // char ** 😛
    splits: HashMap<u16, Split>,
    seqnum: u16,
}

pub struct RudpReceiver<P: UdpPeer> {
    pub(crate) share: Arc<RudpShare<P>>,
    chans: [RecvChan; NUM_CHANS],
    udp: P::Receiver,
    close: watch::Receiver<bool>,
    closed: bool,
    resend: Interval,
    ping: Interval,
    cleanup: Interval,
    timeout: Pin<Box<Sleep>>,
    queue: VecDeque<Result<Pkt<'static>>>,
}

impl<P: UdpPeer> RudpReceiver<P> {
    pub(crate) fn new(
        udp: P::Receiver,
        share: Arc<RudpShare<P>>,
        close: watch::Receiver<bool>,
    ) -> Self {
        Self {
            udp,
            share,
            close,
            closed: false,
            resend: interval(Duration::from_millis(500)),
            ping: interval(Duration::from_secs(PING_TIMEOUT)),
            cleanup: interval(Duration::from_secs(TIMEOUT)),
            timeout: Box::pin(sleep(Duration::from_secs(TIMEOUT))),
            chans: std::array::from_fn(|_| RecvChan {
                packets: (0..REL_BUFFER).map(|_| None).collect(),
                seqnum: INIT_SEQNUM,
                splits: HashMap::new(),
            }),
            queue: VecDeque::new(),
        }
    }

    fn handle_err(&mut self, res: Result<()>) -> Result<()> {
        use Error::*;

        match res {
            Err(RemoteDisco(_)) | Err(LocalDisco) => {
                self.closed = true;
                res
            }
            Ok(_) => res,
            Err(e) => {
                self.queue.push_back(Err(e));
                Ok(())
            }
        }
    }

    pub async fn recv(&mut self) -> Option<Result<Pkt<'static>>> {
        use Error::*;

        if self.closed {
            return None;
        }

        loop {
            if let Some(x) = self.queue.pop_front() {
                return Some(x);
            }

            tokio::select! {
                _ = self.close.changed() => {
                    self.closed = true;
                    return Some(Err(LocalDisco));
                },
                _ = self.cleanup.tick() => {
                    let timeout = Duration::from_secs(TIMEOUT);

                    for chan in self.chans.iter_mut() {
                        chan.splits = chan
                            .splits
                            .drain_filter(
                                |_k, v| !matches!(v.timestamp, Some(t) if t.elapsed() < timeout),
                            )
                            .collect();
                    }
                },
                _ = self.resend.tick() => {
                    for chan in self.share.chans.iter() {
                        for (_, ack) in chan.lock().await.acks.iter() {
                            self.share.send_raw(&ack.data).await.ok(); // TODO: handle error (?)
                        }
                    }
                },
                _ = self.ping.tick() => {
                    self.share
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
                }
                _ = &mut self.timeout => {
                    self.closed = true;
                    return Some(Err(RemoteDisco(true)));
                },
                pkt = self.udp.recv() => {
                    if let Err(e) = self.handle_pkt(pkt).await {
                        return Some(Err(e));
                    }
                }
            }
        }
    }

    async fn handle_pkt(&mut self, pkt: io::Result<Vec<u8>>) -> Result<()> {
        use Error::*;

        let mut cursor = io::Cursor::new(pkt?);

        self.timeout
            .as_mut()
            .reset(tokio::time::Instant::now() + Duration::from_secs(TIMEOUT));

        let proto_id = cursor.read_u32::<BigEndian>()?;
        if proto_id != PROTO_ID {
            return Err(InvalidProtoId(proto_id));
        }

        let _peer_id = cursor.read_u16::<BigEndian>()?;

        let chan = cursor.read_u8()?;
        if chan >= NUM_CHANS as u8 {
            return Err(InvalidChannel(chan));
        }

        let res = self.process_pkt(cursor, true, chan).await;
        self.handle_err(res)?;

        Ok(())
    }

    #[async_recursion]
    async fn process_pkt(
        &mut self,
        mut cursor: io::Cursor<Vec<u8>>,
        unrel: bool,
        chan: u8,
    ) -> Result<()> {
        use Error::*;

        let ch = chan as usize;
        match cursor.read_u8()?.try_into()? {
            PktType::Ctl => match cursor.read_u8()?.try_into()? {
                CtlType::Ack => {
                    // println!("Ack");

                    let seqnum = cursor.read_u16::<BigEndian>()?;
                    if let Some(ack) = self.share.chans[ch].lock().await.acks.remove(&seqnum) {
                        ack.tx.send(true).ok();
                    }
                }
                CtlType::SetPeerID => {
                    // println!("SetPeerID");

                    let mut id = self.share.remote_id.write().await;

                    if *id != PeerID::Nil as u16 {
                        return Err(PeerIDAlreadySet);
                    }

                    *id = cursor.read_u16::<BigEndian>()?;
                }
                CtlType::Ping => {
                    // println!("Ping");
                }
                CtlType::Disco => {
                    // println!("Disco");
                    return Err(RemoteDisco(false));
                }
            },
            PktType::Orig => {
                // println!("Orig");

                self.queue.push_back(Ok(Pkt {
                    chan,
                    unrel,
                    data: Cow::Owned(cursor.remaining_slice().into()),
                }));
            }
            PktType::Split => {
                // println!("Split");

                let seqnum = cursor.read_u16::<BigEndian>()?;
                let chunk_index = cursor.read_u16::<BigEndian>()? as usize;
                let chunk_count = cursor.read_u16::<BigEndian>()? as usize;

                let mut split = self.chans[ch]
                    .splits
                    .entry(seqnum)
                    .or_insert_with(|| Split {
                        got: 0,
                        chunks: (0..chunk_count).map(|_| None).collect(),
                        timestamp: None,
                    });

                if split.chunks.len() != chunk_count {
                    return Err(InvalidChunkCount(split.chunks.len(), chunk_count));
                }

                if split
                    .chunks
                    .get_mut(chunk_index)
                    .ok_or(InvalidChunkIndex(chunk_index, chunk_count))?
                    .replace(cursor.remaining_slice().into())
                    .is_none()
                {
                    split.got += 1;
                }

                split.timestamp = if unrel { Some(Instant::now()) } else { None };

                if split.got == chunk_count {
                    let split = self.chans[ch].splits.remove(&seqnum).unwrap();

                    self.queue.push_back(Ok(Pkt {
                        chan,
                        unrel,
                        data: split
                            .chunks
                            .into_iter()
                            .map(|x| x.unwrap())
                            .reduce(|mut a, mut b| {
                                a.append(&mut b);
                                a
                            })
                            .unwrap_or_default()
                            .into(),
                    }));
                }
            }
            PktType::Rel => {
                // println!("Rel");

                let seqnum = cursor.read_u16::<BigEndian>()?;
                self.chans[ch].packets[to_seqnum(seqnum)].replace(cursor.remaining_slice().into());

                let mut ack_data = Vec::with_capacity(3);
                ack_data.write_u8(CtlType::Ack as u8)?;
                ack_data.write_u16::<BigEndian>(seqnum)?;

                self.share
                    .send(
                        PktType::Ctl,
                        Pkt {
                            chan,
                            unrel: true,
                            data: ack_data.into(),
                        },
                    )
                    .await?;

                let next_pkt = |chan: &mut RecvChan| chan.packets[to_seqnum(chan.seqnum)].take();
                while let Some(pkt) = next_pkt(&mut self.chans[ch]) {
                    let res = self.process_pkt(io::Cursor::new(pkt), false, chan).await;
                    self.handle_err(res)?;
                    self.chans[ch].seqnum = self.chans[ch].seqnum.overflowing_add(1).0;
                }
            }
        }

        Ok(())
    }
}
