use crate::{PeerID, UdpReceiver, UdpSender};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use num_enum::{TryFromPrimitive, TryFromPrimitiveError};
use std::{
    cell::Cell,
    fmt,
    io::{self, Write},
    net, ops,
    sync::{mpsc, Arc},
    thread,
};

pub struct Sender {
    sock: Arc<net::UdpSocket>,
}

impl UdpSender for Sender {
    fn send(&self, data: Vec<u8>) -> io::Result<()> {
        self.sock.send(&data)?;
        Ok(())
    }
}

pub struct Receiver {
    sock: Arc<net::UdpSocket>,
}

impl UdpReceiver for Receiver {
    fn recv(&self) -> io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.resize(crate::UDP_PKT_SIZE, 0);

        let len = self.sock.recv(&mut buffer)?;
        buffer.truncate(len);

        Ok(buffer)
    }
}

pub fn connect(addr: &str) -> io::Result<(crate::RudpSender<Sender>, crate::RudpReceiver<Sender>)> {
    let sock = Arc::new(net::UdpSocket::bind("0.0.0.0:0")?);
    sock.connect(addr)?;

    Ok(crate::new(
        PeerID::Srv as u16,
        PeerID::Nil as u16,
        Sender {
            sock: Arc::clone(&sock),
        },
        Receiver { sock },
    ))
}
