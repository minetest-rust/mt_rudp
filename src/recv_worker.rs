use crate::{error::Error, CtlType, InPkt, Pkt, PktType, RudpShare, UdpReceiver, UdpSender};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use num_enum::{TryFromPrimitive, TryFromPrimitiveError};
use std::{
    cell::Cell,
    io, result,
    sync::{mpsc, Arc},
};

fn to_seqnum(seqnum: u16) -> usize {
    (seqnum as usize) & (crate::REL_BUFFER - 1)
}

struct RelChan {
    packets: Vec<Cell<Option<Vec<u8>>>>, // in the good old days this used to be called char **
    seqnum: u16,
    num: u8,
}

type PktTx = mpsc::Sender<InPkt>;
type Result = result::Result<(), Error>;

pub struct RecvWorker<R: UdpReceiver, S: UdpSender> {
    share: Arc<RudpShare<S>>,
    pkt_tx: PktTx,
    udp_rx: R,
}

impl<R: UdpReceiver, S: UdpSender> RecvWorker<R, S> {
    pub fn new(udp_rx: R, share: Arc<RudpShare<S>>, pkt_tx: PktTx) -> Self {
        Self {
            udp_rx,
            share,
            pkt_tx,
        }
    }

    pub fn run(&self) {
        let mut recv_chans = (0..crate::NUM_CHANS as u8)
            .map(|num| RelChan {
                num,
                packets: (0..crate::REL_BUFFER).map(|_| Cell::new(None)).collect(),
                seqnum: crate::INIT_SEQNUM,
            })
            .collect();

        loop {
            if let Err(e) = self.handle(self.recv_pkt(&mut recv_chans)) {
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
                        .ok();
                }
                break;
            }
        }
    }

    fn recv_pkt(&self, chans: &mut Vec<RelChan>) -> Result {
        use Error::*;

        // todo: reset timeout
        let mut cursor = io::Cursor::new(self.udp_rx.recv()?);

        let proto_id = cursor.read_u32::<BigEndian>()?;
        if proto_id != crate::PROTO_ID {
            do yeet InvalidProtoId(proto_id);
        }

        let peer_id = cursor.read_u16::<BigEndian>()?;

        let n_chan = cursor.read_u8()?;
        let chan = chans
            .get_mut(n_chan as usize)
            .ok_or(InvalidChannel(n_chan))?;

        self.process_pkt(cursor, chan)
    }

    fn process_pkt(&self, mut cursor: io::Cursor<Vec<u8>>, chan: &mut RelChan) -> Result {
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
