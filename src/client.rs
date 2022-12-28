use crate::*;
use std::{io, sync::Arc};
use tokio::net;

pub struct Sender {
    sock: Arc<net::UdpSocket>,
}

#[async_trait]
impl UdpSender for Sender {
    async fn send(&self, data: Vec<u8>) -> io::Result<()> {
        self.sock.send(&data).await?;
        Ok(())
    }
}

pub struct Receiver {
    sock: Arc<net::UdpSocket>,
}

#[async_trait]
impl UdpReceiver for Receiver {
    async fn recv(&self) -> io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.resize(UDP_PKT_SIZE, 0);

        let len = self.sock.recv(&mut buffer).await?;
        buffer.truncate(len);

        Ok(buffer)
    }
}

pub async fn connect(addr: &str) -> io::Result<(RudpSender<Sender>, RudpReceiver<Sender>)> {
    let sock = Arc::new(net::UdpSocket::bind("0.0.0.0:0").await?);
    sock.connect(addr).await?;

    Ok(new(
        PeerID::Srv as u16,
        PeerID::Nil as u16,
        Sender {
            sock: Arc::clone(&sock),
        },
        Receiver { sock },
    ))
}
