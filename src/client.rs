use crate::*;
use std::{io, net, sync::Arc};

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
        buffer.resize(UDP_PKT_SIZE, 0);

        let len = self.sock.recv(&mut buffer)?;
        buffer.truncate(len);

        Ok(buffer)
    }
}

pub fn connect(addr: &str) -> io::Result<(RudpSender<Sender>, RudpReceiver<Sender>)> {
    let sock = Arc::new(net::UdpSocket::bind("0.0.0.0:0")?);
    sock.connect(addr)?;

    Ok(new(
        PeerID::Srv as u16,
        PeerID::Nil as u16,
        Sender {
            sock: Arc::clone(&sock),
        },
        Receiver { sock },
    ))
}
