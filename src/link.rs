//! In-process socket between two Discordium roles.
//!
//! A deliver arrives as a push: the sender's app id, then the payload. Later
//! steps use this same socket. Dropping a datagram is just not taking it.

use std::collections::{HashMap, VecDeque};

#[derive(Default)]
pub struct FakeSocket {
    queues: HashMap<[u8; 16], VecDeque<Datagram>>,
}

struct Datagram {
    sender: [u8; 16],
    payload: Vec<u8>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FakeSocket {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn deliver(&mut self, from: [u8; 16], to: [u8; 16], payload: &[u8]) {
        self.queues.entry(to).or_default().push_back(Datagram {
            sender: from,
            payload: payload.to_vec(),
        });
    }

    pub fn take(&mut self, to: [u8; 16]) -> Option<([u8; 16], Vec<u8>)> {
        let datagram = self.queues.get_mut(&to)?.pop_front()?;
        Some((datagram.sender, datagram.payload))
    }
}
