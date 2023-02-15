#![feature(cursor_remaining)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]
mod client;
mod common;
mod error;
mod recv;
mod send;
mod share;

pub use client::*;
pub use common::*;
pub use error::*;
use recv::*;
pub use send::*;
pub use share::*;
pub use ticker_mod::*;

mod ticker_mod {
    #[macro_export]
    macro_rules! ticker {
		($duration:expr, $close:expr, $body:block) => {
			let mut interval = tokio::time::interval($duration);

			while tokio::select!{
				_ = interval.tick() => true,
				_ = $close.changed() => false,
			} $body
		};
	}
}
