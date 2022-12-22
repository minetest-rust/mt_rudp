#![feature(yeet_expr)]
#![feature(cursor_remaining)]
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use num_enum::{TryFromPrimitive, TryFromPrimitiveError};
use std::{
    cell::Cell,
    fmt,
    io::{self, Write},
    net,
    sync::{mpsc, Arc},
    thread,
};

pub const PROTO_ID: u32 = 0x4f457403;
pub const UDP_PKT_SIZE: usize = 512;
pub const NUM_CHANNELS: usize = 3;
pub const REL_BUFFER: usize = 0x8000;
pub const INIT_SEQNUM: u16 = 65500;

#[derive(Debug, Copy, Clone, PartialEq)]
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

#[derive(Debug)]
pub struct Pkt<T> {
    unrel: bool,
    chan: u8,
    data: T,
}

#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
    InvalidProtoId(u32),
    InvalidPeerID,
    InvalidChannel(u8),
    InvalidType(u8),
    LocalHangup,
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::IoError(err)
    }
}

impl From<TryFromPrimitiveError<PktType>> for Error {
    fn from(err: TryFromPrimitiveError<PktType>) -> Self {
        Self::InvalidType(err.number)
    }
}

impl From<mpsc::SendError<PktResult>> for Error {
    fn from(err: mpsc::SendError<PktResult>) -> Self {
        Self::LocalHangup
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Error::*;
        write!(f, "RUDP Error: ")?;

        match self {
            IoError(err) => write!(f, "IO Error: {}", err),
            InvalidProtoId(id) => write!(f, "Invalid Protocol ID: {id}"),
            InvalidPeerID => write!(f, "Invalid Peer ID"),
            InvalidChannel(ch) => write!(f, "Invalid Channel: {ch}"),
            InvalidType(tp) => write!(f, "Invalid Type: {tp}"),
            LocalHangup => write!(f, "Local packet receiver hung up"),
        }
    }
}

#[derive(Debug)]
struct Channel {}

pub type PktResult = Result<Pkt<Vec<u8>>, Error>;
type PktSender = mpsc::Sender<PktResult>;

#[derive(Debug)]
struct ConnInner {
    sock: net::UdpSocket,
    id: u16,
    remote_id: u16,
    chans: Vec<Channel>,
}

struct RecvChannel {
    packets: Vec<Cell<Option<Vec<u8>>>>, // used to be called char **
    seqnum: u16,
    num: u8,
}

struct ConnReceiver {
    tx: PktSender,
    inner: Arc<ConnInner>,
    chans: Vec<RecvChannel>,
    inbox: [u8; UDP_PKT_SIZE],
}

impl ConnReceiver {
    pub fn run(inner: Arc<ConnInner>, tx: PktSender) {
        Self {
            tx,
            inner,
            chans: (0..NUM_CHANNELS as u8)
                .map(|num| RecvChannel {
                    num,
                    packets: (0..REL_BUFFER).map(|_| Cell::new(None)).collect(),
                    seqnum: INIT_SEQNUM,
                })
                .collect(),
            inbox: [0; UDP_PKT_SIZE],
        }
        .do_loop();
    }

    fn handle_err(&self, res: Result<(), Error>) -> bool {
        if let Err(err) = res {
            if !self.tx.send(Err(err)).is_ok() {
                return false;
            }
        }

        true
    }

    fn do_loop(&mut self) {
        while self.handle_err(self.recv_pkt()) {}
    }

    fn recv_pkt(&mut self) -> Result<(), Error> {
        use Error::*;

        // todo: reset timeout
        let len = self.inner.sock.recv(&mut self.inbox)?;
        let mut cursor = io::Cursor::new(&self.inbox[..len]);

        let proto_id = cursor.read_u32::<BigEndian>()?;
        if proto_id != PROTO_ID {
            do yeet InvalidProtoId(proto_id);
        }

        let peer_id = cursor.read_u16::<BigEndian>()?;

        let n_channel = cursor.read_u8()?;
        let mut channel = self
            .chans
            .get(n_channel as usize)
            .ok_or(InvalidChannel(n_channel))?;

        self.process_pkt(cursor, channel)
    }

    fn process_pkt(&self, mut cursor: io::Cursor<&[u8]>, chan: &RecvChannel) -> Result<(), Error> {
        use PktType::*;

        match cursor.read_u8()?.try_into()? {
            Ctl => {
                dbg!(cursor.remaining_slice());
            }
            Orig => {
                self.tx.send(Ok(Pkt {
                    chan: chan.num,
                    unrel: true,
                    data: cursor.remaining_slice().into(),
                }))?;
            }
            Split => {
                dbg!(cursor.remaining_slice());
            }
            Rel => {
                let seqnum = cursor.read_u16::<BigEndian>()?;
                chan.packets[seqnum as usize].set(Some(cursor.remaining_slice().into()));

                while let Some(pkt) = chan.packets[chan.seqnum as usize].take() {
                    self.handle_err(self.process_pkt(io::Cursor::new(&pkt), chan));
                    chan.seqnum.overflowing_add(1);
                }
            }
        }

        Ok(())
    }
}

impl ConnInner {
    pub fn send(&self, pkt: Pkt<&[u8]>) -> io::Result<()> {
        let mut buf = Vec::with_capacity(4 + 2 + 1 + 1 + pkt.data.len());
        buf.write_u32::<BigEndian>(PROTO_ID)?;
        buf.write_u16::<BigEndian>(self.remote_id)?;
        buf.write_u8(pkt.chan as u8)?;
        buf.write_u8(PktType::Orig as u8)?;
        buf.write(pkt.data)?;

        self.sock.send(&buf)?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct Conn {
    inner: Arc<ConnInner>,
    rx: mpsc::Receiver<PktResult>,
}

impl Conn {
    pub fn connect(addr: &str) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();

        let inner = Arc::new(ConnInner {
            sock: net::UdpSocket::bind("0.0.0.0:0")?,
            id: PeerID::Srv as u16,
            remote_id: PeerID::Nil as u16,
            chans: (0..NUM_CHANNELS).map(|_| Channel {}).collect(),
        });

        inner.sock.connect(addr)?;

        let recv_inner = Arc::clone(&inner);
        thread::spawn(move || {
            ConnReceiver::run(recv_inner, tx);
        });

        Ok(Conn { inner, rx })
    }

    pub fn send(&self, pkt: Pkt<&[u8]>) -> io::Result<()> {
        self.inner.send(pkt)
    }

    pub fn recv(&self) -> Result<PktResult, mpsc::RecvError> {
        self.rx.recv()
    }
}

fn main() {
    //println!("{}", x.deep_size_of());
    let conn = Conn::connect("127.0.0.1:30000").expect("the spanish inquisition");

    let mut mtpkt = vec![];
    mtpkt.write_u16::<BigEndian>(2).unwrap(); // high level type
    mtpkt.write_u8(29).unwrap(); // serialize ver
    mtpkt.write_u16::<BigEndian>(0).unwrap(); // compression modes
    mtpkt.write_u16::<BigEndian>(40).unwrap(); // MinProtoVer
    mtpkt.write_u16::<BigEndian>(40).unwrap(); // MaxProtoVer
    mtpkt.write_u16::<BigEndian>(3).unwrap(); // player name length
    mtpkt.write(b"foo").unwrap(); // player name

    conn.send(Pkt {
        unrel: true,
        chan: 1,
        data: &mtpkt,
    })
    .unwrap();

    while let Ok(result) = conn.recv() {
        match result {
            Ok(pkt) => {
                io::stdout().write(pkt.data.as_slice()).unwrap();
            }
            Err(err) => eprintln!("Error: {}", err),
        }
    }
}
