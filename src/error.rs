use crate::{CtlType, InPkt, PktType};
use num_enum::TryFromPrimitiveError;
use std::{fmt, io, sync::mpsc};

#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
    InvalidProtoId(u32),
    InvalidPeerID,
    InvalidChannel(u8),
    InvalidType(u8),
    InvalidCtlType(u8),
    RemoteDisco,
    LocalDisco,
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

impl From<TryFromPrimitiveError<CtlType>> for Error {
    fn from(err: TryFromPrimitiveError<CtlType>) -> Self {
        Self::InvalidType(err.number)
    }
}

impl From<mpsc::SendError<InPkt>> for Error {
    fn from(_err: mpsc::SendError<InPkt>) -> Self {
        Self::LocalDisco // technically not a disconnect but a local drop
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
            InvalidCtlType(tp) => write!(f, "Invalid Control Type: {tp}"),
            RemoteDisco => write!(f, "Remote Disconnected"),
            LocalDisco => write!(f, "Local Disconnected"),
        }
    }
}
