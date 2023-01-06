use crate::prelude::*;
use num_enum::TryFromPrimitiveError;
use std::{fmt, io};
use tokio::sync::mpsc::error::SendError;

#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
    InvalidProtoId(u32),
    InvalidChannel(u8),
    InvalidType(u8),
    InvalidCtlType(u8),
    PeerIDAlreadySet,
    InvalidChunkIndex(usize, usize),
    InvalidChunkCount(usize, usize),
    RemoteDisco(bool),
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

impl From<SendError<InPkt>> for Error {
    fn from(_err: SendError<InPkt>) -> Self {
        Self::LocalDisco
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Error::*;
        write!(f, "rudp: ")?;

        match self {
            IoError(err) => write!(f, "IO error: {}", err),
            InvalidProtoId(id) => write!(f, "invalid protocol ID: {id}"),
            InvalidChannel(ch) => write!(f, "invalid channel: {ch}"),
            InvalidType(tp) => write!(f, "invalid type: {tp}"),
            InvalidCtlType(tp) => write!(f, "invalid control type: {tp}"),
            PeerIDAlreadySet => write!(f, "peer ID already set"),
            InvalidChunkIndex(i, n) => write!(f, "chunk index {i} bigger than chunk count {n}"),
            InvalidChunkCount(o, n) => write!(f, "chunk count changed from {o} to {n}"),
            RemoteDisco(to) => write!(
                f,
                "remote disconnected{}",
                if *to { " (timeout)" } else { "" }
            ),
            LocalDisco => write!(f, "local disconnected"),
        }
    }
}
