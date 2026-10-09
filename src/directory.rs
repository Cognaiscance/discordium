//! The local node's get-data tree, and the role this process takes from it.
//!
//! The reply layout matches pNet's app get-data encoder: a device's certificate
//! fields sit between its host list and its app list.

use std::fmt;

pub const APP_ALIAS: &str = "discordium";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    Device,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Device,
    Record,
    Standby,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Device => "device",
            Role::Record => "record",
            Role::Standby => "standby",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRecord {
    pub id: [u8; 16],
    pub alias: String,
    /// Own-device entries carry the node's flag. Contact entries in this tree
    /// are already limited to approved apps, so this is true for them.
    pub approved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRecord {
    pub id: [u8; 16],
    pub alias: String,
    pub grade: Grade,
    pub sg_rank: u8,
    pub apps: Vec<AppRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactRecord {
    pub alias: String,
    pub id: [u8; 16],
    pub devices: Vec<DeviceRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory {
    pub local_app_id: [u8; 16],
    pub local_app_alias: String,
    pub local_app_approved: bool,
    pub token: [u8; 16],
    pub local_device: [u8; 16],
    pub owner_alias: String,
    pub owner_id: [u8; 16],
    pub own_devices: Vec<DeviceRecord>,
    pub contacts: Vec<ContactRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Truncated,
    BadUtf8,
    BadGrade,
    Trailing,
    Status,
    Rejected { code: Option<u8> },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Truncated => f.write_str("truncated get-data reply"),
            ParseError::BadUtf8 => f.write_str("get-data string is not utf-8"),
            ParseError::BadGrade => f.write_str("device grade is not device or server"),
            ParseError::Trailing => f.write_str("get-data reply has trailing bytes"),
            ParseError::Status => f.write_str("get-data status is not success"),
            ParseError::Rejected { code: Some(code) } => {
                write!(f, "node rejected get-data ({code:#04x})")
            }
            ParseError::Rejected { code: None } => f.write_str("node rejected get-data"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleError {
    LocalDeviceMissing,
}

impl fmt::Display for RoleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoleError::LocalDeviceMissing => {
                f.write_str("local device is not in the get-data tree")
            }
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Result<u8, ParseError> {
        let byte = *self.data.get(self.pos).ok_or(ParseError::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ParseError> {
        let end = self.pos.checked_add(N).ok_or(ParseError::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(ParseError::Truncated)?;
        let array = slice.try_into().map_err(|_| ParseError::Truncated)?;
        self.pos = end;
        Ok(array)
    }

    fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        let end = self.pos.checked_add(n).ok_or(ParseError::Truncated)?;
        if end > self.data.len() {
            return Err(ParseError::Truncated);
        }
        self.pos = end;
        Ok(())
    }

    fn str(&mut self) -> Result<String, ParseError> {
        let len = self.u8()? as usize;
        let end = self.pos.checked_add(len).ok_or(ParseError::Truncated)?;
        let bytes = self.data.get(self.pos..end).ok_or(ParseError::Truncated)?;
        let text = std::str::from_utf8(bytes).map_err(|_| ParseError::BadUtf8)?;
        self.pos = end;
        Ok(text.to_string())
    }
}

/// Parse a get-data reply, including its leading status byte.
pub fn parse_get_data(reply: &[u8]) -> Result<Directory, ParseError> {
    let mut cur = Cursor {
        data: reply,
        pos: 0,
    };
    let status = cur.u8()?;
    if status == 0x01 {
        return Err(ParseError::Rejected {
            code: reply.get(cur.pos).copied(),
        });
    }
    if status != 0x00 {
        return Err(ParseError::Status);
    }

    let local_app_id = cur.array()?;
    let local_app_alias = cur.str()?;
    cur.skip(4 + 2)?;
    let local_app_approved = cur.u8()? != 0;
    let token = cur.array()?;
    let local_device = cur.array()?;
    let owner_alias = cur.str()?;
    let owner_id = cur.array()?;

    let device_count = cur.u8()? as usize;
    let mut own_devices = Vec::with_capacity(device_count);
    for _ in 0..device_count {
        let mut device = read_device(&mut cur)?;
        device.apps = read_apps(&mut cur, true)?;
        own_devices.push(device);
    }

    let contact_count = cur.u8()? as usize;
    let mut contacts = Vec::with_capacity(contact_count);
    for _ in 0..contact_count {
        let alias = cur.str()?;
        let id = cur.array()?;
        let device_count = cur.u8()? as usize;
        let mut devices = Vec::with_capacity(device_count);
        for _ in 0..device_count {
            let mut device = read_device(&mut cur)?;
            device.apps = read_apps(&mut cur, false)?;
            devices.push(device);
        }
        contacts.push(ContactRecord { alias, id, devices });
    }

    if cur.pos != reply.len() {
        return Err(ParseError::Trailing);
    }

    Ok(Directory {
        local_app_id,
        local_app_alias,
        local_app_approved,
        token,
        local_device,
        owner_alias,
        owner_id,
        own_devices,
        contacts,
    })
}

fn read_device(cur: &mut Cursor<'_>) -> Result<DeviceRecord, ParseError> {
    let id = cur.array()?;
    let alias = cur.str()?;
    let grade = match cur.u8()? {
        0 => Grade::Device,
        1 => Grade::Server,
        _ => return Err(ParseError::BadGrade),
    };
    let sg_rank = cur.u8()?;
    let host_count = cur.u8()?;
    for _ in 0..host_count {
        let _host = cur.str()?;
    }
    // signing key, X25519 key, certificate signature, issued-at.
    cur.skip(32 + 32 + 64 + 8)?;
    let _cert_alias = cur.str()?;
    Ok(DeviceRecord {
        id,
        alias,
        grade,
        sg_rank,
        apps: Vec::new(),
    })
}

fn read_apps(cur: &mut Cursor<'_>, with_host: bool) -> Result<Vec<AppRecord>, ParseError> {
    let count = cur.u8()? as usize;
    let mut apps = Vec::with_capacity(count);
    for _ in 0..count {
        let id = cur.array()?;
        let alias = cur.str()?;
        let approved = if with_host {
            cur.skip(4 + 2)?;
            cur.u8()? != 0
        } else {
            true
        };
        apps.push(AppRecord {
            id,
            alias,
            approved,
        });
    }
    Ok(apps)
}

/// The owner's server that holds the record.
///
/// Numbered ranks come first, lowest number first. Wire rank 0 means no rank
/// and sorts last, the same way pNet treats a missing `sg_rank`. An equal
/// rank uses the lower device id so two processes do not both open a store.
/// Contact devices are not candidates.
pub fn record_holder(dir: &Directory) -> Option<&DeviceRecord> {
    dir.own_devices
        .iter()
        .filter(|device| device.grade == Grade::Server)
        .min_by(|a, b| rank_key(a).cmp(&rank_key(b)))
}

fn rank_key(device: &DeviceRecord) -> (bool, u8, [u8; 16]) {
    (device.sg_rank == 0, device.sg_rank, device.id)
}

/// An app on one device, addressed the way a send names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub device: [u8; 16],
    pub app: [u8; 16],
}

/// Approved `discordium` on the record-holding server.
///
/// The first approved app of that alias on the record holder is the one a
/// device hellos. Contact devices are not candidates.
pub fn record_discordium(dir: &Directory) -> Option<Peer> {
    let holder = record_holder(dir)?;
    let app = holder
        .apps
        .iter()
        .find(|app| app.alias == APP_ALIAS && app.approved)?;
    Some(Peer {
        device: holder.id,
        app: app.id,
    })
}

/// True when `app` is an approved `discordium` on one of this user's devices.
///
/// A contact app does not count. An app that is not in the tree yet is false,
/// which is not a refusal: a later directory can make the same id true.
pub fn approved_own_discordium(dir: &Directory, app: &[u8; 16]) -> bool {
    dir.own_devices.iter().any(|device| {
        device.apps.iter().any(|candidate| {
            candidate.id == *app && candidate.approved && candidate.alias == APP_ALIAS
        })
    })
}

/// Own device that lists `app`. The server uses this to address a reply.
pub fn device_of_app(dir: &Directory, app: &[u8; 16]) -> Option<[u8; 16]> {
    dir.own_devices
        .iter()
        .find(|device| device.apps.iter().any(|candidate| candidate.id == *app))
        .map(|device| device.id)
}

pub fn choose_role(dir: &Directory) -> Result<Role, RoleError> {
    let local = dir
        .own_devices
        .iter()
        .find(|device| device.id == dir.local_device)
        .ok_or(RoleError::LocalDeviceMissing)?;
    if local.grade == Grade::Device {
        return Ok(Role::Device);
    }
    let holder = record_holder(dir);
    if holder.is_some_and(|holder| holder.id == local.id) {
        Ok(Role::Record)
    } else {
        Ok(Role::Standby)
    }
}

pub fn role_summary(dir: &Directory, role: Role) -> String {
    match role {
        Role::Device => "This device serves the interface.".to_string(),
        Role::Record => "This server holds the record.".to_string(),
        Role::Standby => match record_holder(dir) {
            Some(holder) => format!(
                "This server is standing by and will not open a store. The record is held by {}.",
                holder.alias
            ),
            None => "This server is standing by and will not open a store.".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_str(buf: &mut Vec<u8>, text: &str) {
        buf.push(text.len() as u8);
        buf.extend_from_slice(text.as_bytes());
    }

    fn push_device(
        buf: &mut Vec<u8>,
        id: [u8; 16],
        alias: &str,
        grade: u8,
        rank: u8,
        host: Option<&str>,
    ) {
        buf.extend_from_slice(&id);
        push_str(buf, alias);
        buf.push(grade);
        buf.push(rank);
        match host {
            Some(host) => {
                buf.push(1);
                push_str(buf, host);
            }
            None => buf.push(0),
        }
        buf.extend_from_slice(&[0xA5; 32]);
        buf.extend_from_slice(&[0x5A; 32]);
        buf.extend_from_slice(&[0x11; 64]);
        buf.extend_from_slice(&42u64.to_le_bytes());
        push_str(buf, "cert");
    }

    fn push_own_app(buf: &mut Vec<u8>, id: [u8; 16], alias: &str, approved: bool) {
        buf.extend_from_slice(&id);
        push_str(buf, alias);
        buf.extend_from_slice(&[127, 0, 0, 1]);
        buf.extend_from_slice(&8790u16.to_be_bytes());
        buf.push(u8::from(approved));
    }

    fn header(local_app: [u8; 16], local_device: [u8; 16], approved: bool) -> Vec<u8> {
        let mut buf = vec![0x00];
        buf.extend_from_slice(&local_app);
        push_str(&mut buf, "discordium");
        buf.extend_from_slice(&[127, 0, 0, 1]);
        buf.extend_from_slice(&8790u16.to_be_bytes());
        buf.push(u8::from(approved));
        buf.extend_from_slice(&[0x22; 16]);
        buf.extend_from_slice(&local_device);
        push_str(&mut buf, "owner");
        buf.extend_from_slice(&[0x44; 16]);
        buf
    }

    fn lone_device() -> Vec<u8> {
        let local = [0x33; 16];
        let app = [0x11; 16];
        let mut buf = header(app, local, true);
        buf.push(1);
        push_device(&mut buf, local, "laptop", 0, 0, None);
        buf.push(1);
        push_own_app(&mut buf, app, "discordium", true);
        buf.push(0);
        buf
    }

    #[test]
    fn device_grade_selects_device() {
        let dir = parse_get_data(&lone_device()).unwrap();
        assert_eq!(dir.local_app_alias, "discordium");
        assert!(dir.local_app_approved);
        assert_eq!(dir.owner_alias, "owner");
        assert_eq!(dir.own_devices.len(), 1);
        assert_eq!(dir.own_devices[0].alias, "laptop");
        assert_eq!(dir.own_devices[0].apps[0].alias, "discordium");
        assert!(dir.contacts.is_empty());
        assert_eq!(choose_role(&dir).unwrap(), Role::Device);
        assert_eq!(
            role_summary(&dir, Role::Device),
            "This device serves the interface."
        );
    }

    #[test]
    fn device_stays_device_when_an_own_server_exists() {
        let local = [0x33; 16];
        let server = [0x01; 16];
        let app = [0x11; 16];
        let mut buf = header(app, local, false);
        buf.push(2);
        push_device(&mut buf, local, "laptop", 0, 0, None);
        buf.push(1);
        push_own_app(&mut buf, app, "discordium", false);
        push_device(&mut buf, server, "home", 1, 1, Some("10.0.0.2:7777"));
        buf.push(1);
        push_own_app(&mut buf, [0x12; 16], "discordium", true);
        // A contact server with a lower rank must not become the record.
        buf.push(1);
        push_str(&mut buf, "other");
        buf.extend_from_slice(&[0x55; 16]);
        buf.push(1);
        push_device(
            &mut buf,
            [0x66; 16],
            "other-sg",
            1,
            1,
            Some("10.1.0.2:7777"),
        );
        buf.push(1);
        buf.extend_from_slice(&[0x77; 16]);
        push_str(&mut buf, "discordium");

        let dir = parse_get_data(&buf).unwrap();
        assert!(!dir.local_app_approved);
        assert_eq!(dir.contacts.len(), 1);
        assert_eq!(dir.contacts[0].devices[0].apps[0].alias, "discordium");
        assert!(dir.contacts[0].devices[0].apps[0].approved);
        assert_eq!(choose_role(&dir).unwrap(), Role::Device);
        assert_eq!(record_holder(&dir).unwrap().alias, "home");
    }

    #[test]
    fn lowest_server_selects_record() {
        let local = [0x10; 16];
        let other = [0x20; 16];
        let app = [0x11; 16];
        let mut buf = header(app, local, true);
        buf.push(2);
        push_device(&mut buf, other, "spare", 1, 2, Some("10.0.0.3:7777"));
        buf.push(0);
        push_device(&mut buf, local, "home", 1, 1, Some("10.0.0.2:7777"));
        buf.push(1);
        push_own_app(&mut buf, app, "discordium", true);
        buf.push(0);

        let dir = parse_get_data(&buf).unwrap();
        assert_eq!(choose_role(&dir).unwrap(), Role::Record);
        assert_eq!(
            role_summary(&dir, Role::Record),
            "This server holds the record."
        );
    }

    #[test]
    fn higher_server_selects_standby() {
        let local = [0x20; 16];
        let other = [0x10; 16];
        let app = [0x11; 16];
        let mut buf = header(app, local, true);
        buf.push(2);
        push_device(&mut buf, other, "home", 1, 1, Some("10.0.0.2:7777"));
        buf.push(1);
        push_own_app(&mut buf, [0x12; 16], "discordium", true);
        push_device(&mut buf, local, "spare", 1, 2, Some("10.0.0.3:7777"));
        buf.push(1);
        push_own_app(&mut buf, app, "discordium", true);
        buf.push(0);

        let dir = parse_get_data(&buf).unwrap();
        assert_eq!(choose_role(&dir).unwrap(), Role::Standby);
        assert_eq!(
            role_summary(&dir, Role::Standby),
            "This server is standing by and will not open a store. The record is held by home."
        );
    }

    #[test]
    fn equal_rank_uses_the_lower_device_id() {
        let lower = [0x01; 16];
        let higher = [0x02; 16];
        let app = [0x11; 16];

        let mut as_lower = header(app, lower, true);
        as_lower.push(2);
        push_device(&mut as_lower, higher, "b", 1, 1, None);
        as_lower.push(0);
        push_device(&mut as_lower, lower, "a", 1, 1, None);
        as_lower.push(1);
        push_own_app(&mut as_lower, app, "discordium", true);
        as_lower.push(0);
        let dir = parse_get_data(&as_lower).unwrap();
        assert_eq!(choose_role(&dir).unwrap(), Role::Record);

        let mut as_higher = header(app, higher, true);
        as_higher.push(2);
        push_device(&mut as_higher, lower, "a", 1, 1, None);
        as_higher.push(0);
        push_device(&mut as_higher, higher, "b", 1, 1, None);
        as_higher.push(1);
        push_own_app(&mut as_higher, app, "discordium", true);
        as_higher.push(0);
        let dir = parse_get_data(&as_higher).unwrap();
        assert_eq!(choose_role(&dir).unwrap(), Role::Standby);
    }

    #[test]
    fn unranked_server_defers_to_a_numbered_rank() {
        let local = [0x20; 16];
        let ranked = [0x10; 16];
        let app = [0x11; 16];
        let mut buf = header(app, local, true);
        buf.push(2);
        push_device(&mut buf, ranked, "home", 1, 1, None);
        buf.push(0);
        push_device(&mut buf, local, "spare", 1, 0, None);
        buf.push(1);
        push_own_app(&mut buf, app, "discordium", true);
        buf.push(0);

        let dir = parse_get_data(&buf).unwrap();
        assert_eq!(choose_role(&dir).unwrap(), Role::Standby);
        assert_eq!(record_holder(&dir).unwrap().id, ranked);
    }

    #[test]
    fn a_lone_unranked_server_holds_the_record() {
        let local = [0x10; 16];
        let app = [0x11; 16];
        let mut buf = header(app, local, true);
        buf.push(1);
        push_device(&mut buf, local, "home", 1, 0, None);
        buf.push(1);
        push_own_app(&mut buf, app, "discordium", true);
        buf.push(0);

        let dir = parse_get_data(&buf).unwrap();
        assert_eq!(choose_role(&dir).unwrap(), Role::Record);
    }

    #[test]
    fn rejected_truncated_and_trailing_replies_fail() {
        assert_eq!(
            parse_get_data(&[0x01, 0x02]).unwrap_err(),
            ParseError::Rejected { code: Some(0x02) }
        );
        let mut short = lone_device();
        short.pop();
        assert_eq!(parse_get_data(&short).unwrap_err(), ParseError::Truncated);
        let mut extra = lone_device();
        extra.push(0);
        assert_eq!(parse_get_data(&extra).unwrap_err(), ParseError::Trailing);
    }

    #[test]
    fn missing_local_device_has_no_role() {
        let app = [0x11; 16];
        let mut buf = header(app, [0x33; 16], true);
        buf.push(1);
        push_device(&mut buf, [0x99; 16], "someone-else", 0, 0, None);
        buf.push(0);
        buf.push(0);
        let dir = parse_get_data(&buf).unwrap();
        assert_eq!(
            choose_role(&dir).unwrap_err(),
            RoleError::LocalDeviceMissing
        );
    }
}
