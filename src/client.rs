use super::*;
use async_trait::async_trait;
use std::{io, sync::Arc};
use tokio::net;

#[derive(Debug)]
pub struct ToSrv(Arc<net::UdpSocket>);

#[derive(Debug)]
pub struct FromSrv(Arc<net::UdpSocket>);

#[async_trait]
impl UdpSender for ToSrv {
    async fn send(&self, data: &[u8]) -> io::Result<()> {
        self.0.send(data).await?;
        Ok(())
    }
}

#[async_trait]
impl UdpReceiver for FromSrv {
    async fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.resize(UDP_PKT_SIZE, 0);

        let len = self.0.recv(&mut buffer).await?;
        buffer.truncate(len);

        Ok(buffer)
    }
}

pub struct RemoteSrv;
impl UdpPeer for RemoteSrv {
    type Sender = ToSrv;
    type Receiver = FromSrv;
}

pub async fn connect(addr: &str) -> io::Result<(RudpSender<RemoteSrv>, RudpReceiver<RemoteSrv>)> {
    let sock = Arc::new(net::UdpSocket::bind("0.0.0.0:0").await?);
    sock.connect(addr).await?;

    new(
        PeerID::Srv as u16,
        PeerID::Nil as u16,
        ToSrv(Arc::clone(&sock)),
        FromSrv(sock),
    )
    .await
}
