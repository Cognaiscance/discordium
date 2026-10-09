//! HELLO from a device, HELLO_ACK from the record server.
//!
//! The payload is a version byte and a type byte. The sender is the push's
//! app id, not anything inside the payload. The server acks an approved
//! `discordium` on one of this user's own devices. A repeat ack does not
//! attach that device again. Any other sender is ignored, and an app that is
//! missing from the directory is not remembered as a refusal.

use crate::directory::{approved_own_discordium, record_discordium, Directory, Peer};

pub const VERSION: u8 = 1;
pub const HELLO: u8 = 0x01;
pub const HELLO_ACK: u8 = 0x02;

pub const HELLO_BYTES: [u8; 2] = [VERSION, HELLO];
pub const HELLO_ACK_BYTES: [u8; 2] = [VERSION, HELLO_ACK];

pub fn is_hello(payload: &[u8]) -> bool {
    payload == HELLO_BYTES
}

#[derive(Debug)]
pub struct DeviceHello {
    server: Option<Peer>,
    acked: bool,
}

impl DeviceHello {
    pub fn new() -> Self {
        Self {
            server: None,
            acked: false,
        }
    }

    pub fn note_directory(&mut self, dir: &Directory) {
        if !self.acked {
            self.server = record_discordium(dir);
        }
    }

    pub fn is_acked(&self) -> bool {
        self.acked
    }

    /// Where the next HELLO goes. None once acked, or until the server is visible.
    pub fn hello_to_send(&self) -> Option<Peer> {
        if self.acked {
            None
        } else {
            self.server
        }
    }

    pub fn on_push(&mut self, sender: [u8; 16], payload: &[u8]) {
        let Some(server) = self.server else {
            return;
        };
        if sender == server.app && payload == HELLO_ACK_BYTES {
            self.acked = true;
        }
    }
}

#[derive(Debug, Default)]
pub struct ServerHello {
    attached: Vec<[u8; 16]>,
}

impl ServerHello {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attached(&self) -> &[[u8; 16]] {
        &self.attached
    }

    /// Ack a HELLO from an approved own Discordium. The ack is sent again on
    /// a retry, and the device stays attached once.
    pub fn on_hello(
        &mut self,
        dir: &Directory,
        sender: [u8; 16],
        payload: &[u8],
    ) -> Option<[u8; 2]> {
        if !is_hello(payload) || !approved_own_discordium(dir, &sender) {
            return None;
        }
        if !self.attached.contains(&sender) {
            self.attached.push(sender);
        }
        Some(HELLO_ACK_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{AppRecord, ContactRecord, DeviceRecord, Directory, Grade};
    use crate::link::FakeSocket;

    fn ident(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn app(id: [u8; 16], alias: &str, approved: bool) -> AppRecord {
        AppRecord {
            id,
            alias: alias.to_string(),
            approved,
        }
    }

    fn dev(
        id: [u8; 16],
        alias: &str,
        grade: Grade,
        rank: u8,
        apps: Vec<AppRecord>,
    ) -> DeviceRecord {
        DeviceRecord {
            id,
            alias: alias.to_string(),
            grade,
            sg_rank: rank,
            apps,
        }
    }

    fn directory(
        local: [u8; 16],
        own: Vec<DeviceRecord>,
        contacts: Vec<ContactRecord>,
    ) -> Directory {
        Directory {
            local_app_id: ident(1),
            local_app_alias: "discordium".into(),
            local_app_approved: true,
            token: ident(2),
            local_device: local,
            owner_alias: "owner".into(),
            owner_id: ident(3),
            own_devices: own,
            contacts,
        }
    }

    /// Laptop device plus the record server. A contact server has a lower id
    /// and the same rank, so a lookup that walks contacts would pick it.
    fn two_nodes() -> (Directory, [u8; 16], [u8; 16], [u8; 16]) {
        let laptop = ident(0x10);
        let laptop_app = ident(0x11);
        let home = ident(0x20);
        let home_app = ident(0x21);
        let dir = directory(
            laptop,
            vec![
                dev(
                    laptop,
                    "laptop",
                    Grade::Device,
                    0,
                    vec![app(laptop_app, "discordium", true)],
                ),
                dev(
                    home,
                    "home",
                    Grade::Server,
                    1,
                    vec![app(home_app, "discordium", true)],
                ),
            ],
            vec![ContactRecord {
                alias: "other".into(),
                id: ident(0x30),
                devices: vec![dev(
                    ident(0x01),
                    "other-sg",
                    Grade::Server,
                    1,
                    vec![app(ident(0x31), "discordium", true)],
                )],
            }],
        );
        (dir, laptop_app, home, home_app)
    }

    #[test]
    fn approved_own_device_is_acked() {
        let (dir, laptop_app, home, home_app) = two_nodes();
        let mut link = FakeSocket::new();
        let mut device = DeviceHello::new();
        let mut server = ServerHello::new();
        device.note_directory(&dir);

        let peer = device.hello_to_send().unwrap();
        assert_eq!(peer.device, home);
        assert_eq!(peer.app, home_app);
        link.deliver(laptop_app, peer.app, &HELLO_BYTES);
        let (sender, payload) = link.take(home_app).unwrap();
        let ack = server.on_hello(&dir, sender, &payload).unwrap();
        assert_eq!(ack, HELLO_ACK_BYTES);
        link.deliver(home_app, laptop_app, &ack);
        let (sender, payload) = link.take(laptop_app).unwrap();
        device.on_push(sender, &payload);

        assert!(device.is_acked());
        assert!(device.hello_to_send().is_none());
        assert_eq!(server.attached(), &[laptop_app]);
    }

    #[test]
    fn retry_does_not_attach_twice() {
        let (dir, laptop_app, _home, home_app) = two_nodes();
        let mut link = FakeSocket::new();
        let mut device = DeviceHello::new();
        let mut server = ServerHello::new();
        device.note_directory(&dir);
        let peer = device.hello_to_send().unwrap();

        link.deliver(laptop_app, peer.app, &HELLO_BYTES);
        let (sender, payload) = link.take(home_app).unwrap();
        let _lost_ack = server.on_hello(&dir, sender, &payload).unwrap();
        assert!(link.take(laptop_app).is_none());
        assert!(!device.is_acked());
        assert_eq!(server.attached(), &[laptop_app]);

        link.deliver(laptop_app, peer.app, &HELLO_BYTES);
        let (sender, payload) = link.take(home_app).unwrap();
        let ack = server.on_hello(&dir, sender, &payload).unwrap();
        assert_eq!(server.attached(), &[laptop_app]);
        link.deliver(home_app, laptop_app, &ack);
        let (sender, payload) = link.take(laptop_app).unwrap();
        device.on_push(sender, &payload);
        assert!(device.is_acked());
        assert_eq!(server.attached(), &[laptop_app]);
    }

    #[test]
    fn other_senders_are_ignored() {
        let (mut dir, laptop_app, _home, home_app) = two_nodes();
        dir.own_devices[0]
            .apps
            .push(app(ident(0x12), "discordium", false));
        dir.own_devices[0]
            .apps
            .push(app(ident(0x13), "notes", true));
        let mut link = FakeSocket::new();
        let mut server = ServerHello::new();

        for sender in [ident(0x31), ident(0x12), ident(0x13), ident(0x99)] {
            link.deliver(sender, home_app, &HELLO_BYTES);
            let (from, payload) = link.take(home_app).unwrap();
            assert!(server.on_hello(&dir, from, &payload).is_none());
        }
        for bad in [
            vec![2, HELLO],
            vec![VERSION, HELLO_ACK],
            vec![VERSION, HELLO, 0],
            vec![VERSION],
            vec![],
        ] {
            assert!(server.on_hello(&dir, laptop_app, &bad).is_none());
        }
        assert!(server.attached().is_empty());

        link.deliver(laptop_app, home_app, &HELLO_BYTES);
        let (from, payload) = link.take(home_app).unwrap();
        assert!(server.on_hello(&dir, from, &payload).is_some());
        assert_eq!(server.attached(), &[laptop_app]);

        dir.own_devices.push(dev(
            ident(0x40),
            "phone",
            Grade::Device,
            0,
            vec![app(ident(0x99), "discordium", true)],
        ));
        link.deliver(ident(0x99), home_app, &HELLO_BYTES);
        let (from, payload) = link.take(home_app).unwrap();
        assert!(server.on_hello(&dir, from, &payload).is_some());
        assert_eq!(server.attached(), &[laptop_app, ident(0x99)]);

        let mut device = DeviceHello::new();
        device.note_directory(&dir);
        device.on_push(ident(0x31), &HELLO_ACK_BYTES);
        assert!(!device.is_acked());
        device.on_push(home_app, &HELLO_ACK_BYTES);
        assert!(device.is_acked());
    }

    #[test]
    fn device_waits_until_the_server_is_visible() {
        let laptop = ident(0x10);
        let home = ident(0x20);
        let mut dir = directory(
            laptop,
            vec![
                dev(
                    laptop,
                    "laptop",
                    Grade::Device,
                    0,
                    vec![app(ident(0x11), "discordium", true)],
                ),
                dev(home, "home", Grade::Server, 1, vec![]),
            ],
            vec![],
        );
        let mut device = DeviceHello::new();
        device.note_directory(&dir);
        assert!(device.hello_to_send().is_none());

        dir.own_devices[1]
            .apps
            .push(app(ident(0x21), "discordium", false));
        device.note_directory(&dir);
        assert!(device.hello_to_send().is_none());

        dir.own_devices[1].apps[0].approved = true;
        device.note_directory(&dir);
        assert_eq!(device.hello_to_send().unwrap().app, ident(0x21));
    }
}
