//! File-backed administration. Offline usernames are NOT verified identities.
//! Player-originated commands must never gain OP authority in offline mode.
use crate::whitelist::valid_name;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

const MAX_DB_BYTES: u64 = 128 * 1024;
const MAX_RECORDS: usize = 2048;

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_owned())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(
        default,
        rename = "bypassesPlayerLimit",
        skip_serializing_if = "Option::is_none"
    )]
    pub bypasses_player_limit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpEntry {
    pub ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerPermission {
    pub name: String,
    #[serde(default = "allowed")]
    pub chat: bool,
    #[serde(default = "allowed")]
    pub build: bool,
    #[serde(default = "allowed")]
    pub interact: bool,
}
fn allowed() -> bool {
    true
}
impl Default for PlayerPermission {
    fn default() -> Self {
        Self {
            name: String::new(),
            chat: true,
            build: true,
            interact: true,
        }
    }
}

fn regular_file(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > MAX_DB_BYTES {
        return Err(invalid(
            "administration database must be a regular bounded file",
        ));
    }
    Ok(())
}
fn read_array<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Vec<T>> {
    regular_file(path)?;
    let content = fs::read(path)?;
    if content.len() as u64 > MAX_DB_BYTES {
        return Err(invalid("administration database exceeds size limit"));
    }
    let value: Vec<T> = serde_json::from_slice(&content)
        .map_err(|_| invalid("invalid administration JSON schema"))?;
    if value.len() > MAX_RECORDS {
        return Err(invalid("too many administration entries"));
    }
    Ok(value)
}
fn persist_array<T: Serialize>(path: &Path, data: &[T]) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(data).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_DB_BYTES {
        return Err(invalid("administration database too large"));
    }
    regular_file(path)?;
    // Never follow an existing symlink or overwrite an existing temp name.
    let candidate = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)?;
    let written = (|| -> io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&candidate, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                dir.sync_all()?;
            }
        }
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(candidate);
    }
    written
}
fn ensure_array(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => regular_file(path),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
            file.write_all(b"[]\n")?;
            file.sync_all()
        }
        Err(e) => Err(e),
    }
}
fn validate_named(items: &[NamedEntry]) -> io::Result<()> {
    let mut seen = BTreeSet::new();
    for item in items {
        if !valid_name(&item.name) || !seen.insert(item.name.to_ascii_lowercase()) {
            return Err(invalid("invalid or duplicate admin player name"));
        }
        if item.level.is_some_and(|l| !(1..=4).contains(&l)) {
            return Err(invalid("invalid OP level"));
        }
        if item.uuid.as_deref().is_some_and(|uuid| !valid_uuid(uuid)) {
            return Err(invalid("invalid authenticated UUID in admin database"));
        }
        if item.reason.as_ref().is_some_and(|r| r.len() > 256) {
            return Err(invalid("BAN reason too long"));
        }
    }
    Ok(())
}
fn validate_ops(entries: &[NamedEntry]) -> io::Result<()> {
    validate_named(entries)?;
    let mut uuids = BTreeSet::new();
    for entry in entries {
        if let Some(uuid) = &entry.uuid {
            if !uuids.insert(uuid.to_ascii_lowercase()) {
                return Err(invalid("duplicate authenticated OP UUID"));
            }
        }
    }
    Ok(())
}

/// Name-only bans apply to a username; verified bans belong to immutable
/// account UUIDs. Multiple historical UUIDs may legitimately share a name.
fn validate_banned_players(entries: &[NamedEntry]) -> io::Result<()> {
    let mut names = BTreeSet::new();
    let mut uuids = BTreeSet::new();
    for entry in entries {
        if !valid_name(&entry.name) {
            return Err(invalid("invalid banned player name"));
        }
        if entry
            .reason
            .as_ref()
            .is_some_and(|r| r.len() > 256 || r.chars().any(char::is_control))
        {
            return Err(invalid("invalid BAN reason"));
        }
        if let Some(uuid) = &entry.uuid {
            if !valid_uuid(uuid) || !uuids.insert(uuid.to_ascii_lowercase()) {
                return Err(invalid("invalid or duplicate banned account UUID"));
            }
        } else if !names.insert(entry.name.to_ascii_lowercase()) {
            return Err(invalid("duplicate name-only ban"));
        }
    }
    Ok(())
}

fn valid_uuid(uuid: &str) -> bool {
    let bytes = uuid.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
fn entry_uuid_matches(entry: &NamedEntry, uuid: &str) -> bool {
    entry
        .uuid
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case(uuid))
}
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ipv6)),
        other => other,
    }
}
fn validate_ips(entries: &[IpEntry]) -> io::Result<()> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        let ip: IpAddr = entry.ip.parse().map_err(|_| invalid("invalid BAN IP"))?;
        if !seen.insert(canonical_ip(ip)) {
            return Err(invalid("duplicate banned IP"));
        }
        if entry.reason.as_ref().is_some_and(|r| r.len() > 256) {
            return Err(invalid("BAN reason too long"));
        }
    }
    Ok(())
}
fn validate_permissions(entries: &[PlayerPermission]) -> io::Result<()> {
    let mut names = BTreeSet::new();
    for p in entries {
        if !valid_name(&p.name) || !names.insert(p.name.to_ascii_lowercase()) {
            return Err(invalid("invalid or duplicate permissions name"));
        }
    }
    Ok(())
}

pub struct Administration {
    root: PathBuf,
}
impl Administration {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let store = Self { root };
        for name in [
            "ops.json",
            "banned-players.json",
            "banned-ips.json",
            "player-permissions.json",
        ] {
            ensure_array(&store.file(name))?;
        }
        store.ops()?;
        store.banned_players()?;
        store.banned_ips()?;
        store.permissions()?;
        Ok(store)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn ops(&self) -> io::Result<Vec<NamedEntry>> {
        let data = read_array(&self.file("ops.json"))?;
        validate_ops(&data)?;
        Ok(data)
    }
    fn banned_players(&self) -> io::Result<Vec<NamedEntry>> {
        let data = read_array(&self.file("banned-players.json"))?;
        validate_banned_players(&data)?;
        Ok(data)
    }
    fn banned_ips(&self) -> io::Result<Vec<IpEntry>> {
        let data = read_array(&self.file("banned-ips.json"))?;
        validate_ips(&data)?;
        Ok(data)
    }
    fn permissions(&self) -> io::Result<Vec<PlayerPermission>> {
        let data = read_array(&self.file("player-permissions.json"))?;
        validate_permissions(&data)?;
        Ok(data)
    }
    /// A verified account keeps its authority across a username change, but
    /// a name alone (including a recycled old name) can NEVER confer OP.
    pub fn operator_level(&self, _name: &str, verified_uuid: Option<&str>) -> io::Result<u8> {
        let operators = self.ops()?;
        Ok(verified_uuid
            .filter(|uuid| valid_uuid(uuid))
            .map_or(0, |uuid| {
                operators
                    .iter()
                    .filter(|entry| entry_uuid_matches(entry, uuid))
                    .map(|entry| entry.level.unwrap_or(4))
                    .max()
                    .unwrap_or(0)
            }))
    }
    pub fn is_operator(&self, name: &str, verified_uuid: Option<&str>) -> io::Result<bool> {
        Ok(self.operator_level(name, verified_uuid)? > 0)
    }
    pub fn banned(&self, name: &str, address: IpAddr) -> io::Result<Option<String>> {
        self.banned_identity(name, None, address)
    }
    /// A verified account cannot evade a UUID-linked ban by changing its name.
    /// A new owner of a recycled name cannot inherit the previous account's ban.
    /// Before authentication (or in offline mode) only name-only and IP bans apply.
    pub fn banned_identity(
        &self,
        name: &str,
        verified_uuid: Option<&str>,
        address: IpAddr,
    ) -> io::Result<Option<String>> {
        if verified_uuid.is_some_and(|id| !valid_uuid(id)) {
            return Err(invalid("invalid verified ban identity"));
        }
        let by_name = self.banned_players()?;
        let by_ip = self.banned_ips()?;
        if let Some(entry) = by_name.iter().find(|e| match e.uuid.as_deref() {
            Some(uuid) => verified_uuid.is_some_and(|id| uuid.eq_ignore_ascii_case(id)),
            None => e.name.eq_ignore_ascii_case(name),
        }) {
            return Ok(Some(
                entry
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Player is banned".into()),
            ));
        }
        if let Some(entry) = by_ip.iter().find(|e| {
            e.ip.parse::<IpAddr>()
                .is_ok_and(|ip| canonical_ip(ip) == canonical_ip(address))
        }) {
            return Ok(Some(
                entry
                    .reason
                    .clone()
                    .unwrap_or_else(|| "IP address is banned".into()),
            ));
        }
        Ok(None)
    }
    pub fn ip_banned(&self, address: IpAddr) -> io::Result<bool> {
        Ok(self.banned_ips()?.iter().any(|e| {
            e.ip.parse::<IpAddr>()
                .is_ok_and(|ip| canonical_ip(ip) == canonical_ip(address))
        }))
    }
    pub fn permission(&self, name: &str) -> io::Result<PlayerPermission> {
        Ok(self
            .permissions()?
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| PlayerPermission {
                name: name.into(),
                ..PlayerPermission::default()
            }))
    }
    pub fn set_op(&self, name: &str, enabled: bool) -> io::Result<()> {
        self.update_op(name, enabled, None)
    }
    /// Console may grant OP only to an identity already authenticated by
    /// the online-mode handshake. Never accept a username as proof of UUID.
    pub fn set_op_verified(&self, name: &str, uuid: &str) -> io::Result<()> {
        self.update_op(name, true, Some(uuid))
    }
    /// Revoke the verified account, including an older username alias.
    /// Callers must get the UUID from the authenticated live session.
    pub fn revoke_op_verified(&self, name: &str, uuid: &str) -> io::Result<()> {
        self.update_op(name, false, Some(uuid))
    }
    fn update_op(&self, name: &str, enabled: bool, uuid: Option<&str>) -> io::Result<()> {
        if !valid_name(name) {
            return Err(invalid("invalid OP username"));
        }
        if uuid.is_some_and(|id| !valid_uuid(id)) {
            return Err(invalid("invalid authenticated OP UUID"));
        }
        let mut ops = self.ops()?;
        // Re-granting after a rename must not leave a second UUID-linked OP.
        // De-op by UUID must revoke any previously recorded name alias.
        ops.retain(|e| {
            !e.name.eq_ignore_ascii_case(name) && !uuid.is_some_and(|id| entry_uuid_matches(e, id))
        });
        if enabled {
            ops.push(NamedEntry {
                name: name.into(),
                uuid: uuid.map(str::to_owned),
                reason: None,
                level: Some(4),
                bypasses_player_limit: Some(false),
                created: None,
                source: Some("CONSOLE".into()),
                expires: None,
            });
        }
        persist_array(&self.file("ops.json"), &ops)
    }
    pub fn set_player_ban(
        &self,
        name: &str,
        reason: Option<&str>,
        enabled: bool,
    ) -> io::Result<()> {
        self.set_player_ban_verified(name, None, reason, enabled)
    }
    /// Bind a new ban to a server-verified account UUID where available.
    /// A console ban of an offline target remains a legacy name-only ban.
    pub fn set_player_ban_verified(
        &self,
        name: &str,
        verified_uuid: Option<&str>,
        reason: Option<&str>,
        enabled: bool,
    ) -> io::Result<()> {
        if !valid_name(name)
            || verified_uuid.is_some_and(|id| !valid_uuid(id))
            || reason.is_some_and(|r| r.len() > 256 || r.chars().any(char::is_control))
        {
            return Err(invalid("invalid player BAN"));
        }
        let mut bans = self.banned_players()?;
        if enabled {
            // A verified account's rename or a recycled username must not
            // silently lift an independent UUID-linked ban.
            bans.retain(|e| {
                if let Some(uuid) = verified_uuid {
                    !entry_uuid_matches(e, uuid)
                } else {
                    e.uuid.is_some() || !e.name.eq_ignore_ascii_case(name)
                }
            });
        } else if let Some(uuid) = verified_uuid {
            // UUID-targeted pardon never revokes another account's ban.
            bans.retain(|e| !entry_uuid_matches(e, uuid));
        } else {
            // Preserve familiar /pardon Name for an unambiguous old record,
            // but require /pardon-uuid for recycled names with multiple UUIDs.
            let matches = bans
                .iter()
                .filter(|e| e.uuid.is_some() && e.name.eq_ignore_ascii_case(name))
                .count();
            if matches > 1 {
                return Err(invalid("ambiguous banned name: use /pardon-uuid"));
            }
            bans.retain(|e| !e.name.eq_ignore_ascii_case(name));
        }
        if enabled {
            bans.push(NamedEntry {
                name: name.into(),
                uuid: verified_uuid.map(str::to_owned),
                reason: reason.map(str::to_owned),
                level: None,
                bypasses_player_limit: None,
                created: None,
                source: Some("Server".into()),
                expires: None,
            });
        }
        persist_array(&self.file("banned-players.json"), &bans)
    }

    /// Explicitly revoke an authenticated account's ban even after it has
    /// changed names or is no longer online. UUIDs are not inferred from names.
    pub fn pardon_uuid(&self, uuid: &str) -> io::Result<()> {
        if !valid_uuid(uuid) {
            return Err(invalid("invalid pardon UUID"));
        }
        let mut bans = self.banned_players()?;
        bans.retain(|entry| !entry_uuid_matches(entry, uuid));
        persist_array(&self.file("banned-players.json"), &bans)
    }

    pub fn set_ip_ban(&self, address: &str, reason: Option<&str>, enabled: bool) -> io::Result<()> {
        let ip: IpAddr = address.parse().map_err(|_| invalid("invalid IP address"))?;
        if reason.is_some_and(|r| r.len() > 256 || r.chars().any(char::is_control)) {
            return Err(invalid("invalid IP BAN reason"));
        }
        let mut bans = self.banned_ips()?;
        bans.retain(|e| {
            e.ip.parse::<IpAddr>()
                .is_ok_and(|b| canonical_ip(b) != canonical_ip(ip))
        });
        if enabled {
            bans.push(IpEntry {
                ip: canonical_ip(ip).to_string(),
                reason: reason.map(str::to_owned),
                created: None,
                source: Some("Server".into()),
                expires: None,
            });
        }
        persist_array(&self.file("banned-ips.json"), &bans)
    }
    pub fn set_permission(&self, name: &str, mode: &str, enabled: bool) -> io::Result<()> {
        if !valid_name(name) {
            return Err(invalid("invalid player name"));
        }
        let mut permissions = self.permissions()?;
        let mut user = permissions
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned()
            .unwrap_or_else(|| PlayerPermission {
                name: name.into(),
                ..PlayerPermission::default()
            });
        match mode {
            "chat" => user.chat = enabled,
            "build" => user.build = enabled,
            "interact" => user.interact = enabled,
            _ => return Err(invalid("invalid permission kind")),
        }
        permissions.retain(|p| !p.name.eq_ignore_ascii_case(name));
        permissions.push(user);
        persist_array(&self.file("player-permissions.json"), &permissions)
    }
    pub fn audit(&self, actor: &str, operation: &str, outcome: &str) -> io::Result<()> {
        if actor.len() > 32 || operation.len() > 512 || outcome.len() > 40 {
            return Err(invalid("invalid audit record"));
        }
        let path = self.file("admin-audit.jsonl");
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.file_type().is_file() => {
                return Err(invalid("audit log must be a regular file"))
            }
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        let record =
            json!({"unix_seconds":stamp,"actor":actor,"action":operation,"result":outcome});
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{record}")?;
        file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ip_normalization_and_no_offline_operator_elevation() {
        assert_eq!(
            canonical_ip("::ffff:192.0.2.1".parse::<IpAddr>().unwrap()),
            "192.0.2.1".parse::<IpAddr>().unwrap()
        );
        let root = std::env::temp_dir().join(format!(
            "ir1710-security-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        store.set_op("Operator", true).unwrap();
        assert!(!store.is_operator("Operator", None).unwrap());
        assert!(!store
            .is_operator("Operator", Some("d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca"))
            .unwrap());
        store
            .set_op_verified("Operator", "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca")
            .unwrap();
        assert!(store
            .is_operator("Operator", Some("d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca"))
            .unwrap());
        assert_eq!(
            store
                .operator_level("Operator", Some("d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca"))
                .unwrap(),
            4
        );
        assert!(!store
            .is_operator("Operator", Some("d3a0a8b1-4753-4a66-b9c8-f4ac1234c9cb"))
            .unwrap());
        store
            .set_player_ban("Spammer", Some("abuse"), true)
            .unwrap();
        assert_eq!(
            store
                .banned("spammer", "127.0.0.1".parse().unwrap())
                .unwrap(),
            Some("abuse".into())
        );
        store.set_ip_ban("::ffff:192.0.2.1", None, true).unwrap();
        assert!(store.ip_banned("192.0.2.1".parse().unwrap()).unwrap());
        store.set_ip_ban("192.0.2.1", None, false).unwrap();
        assert!(!store
            .ip_banned("::ffff:192.0.2.1".parse().unwrap())
            .unwrap());
        store.set_player_ban("Spammer", None, false).unwrap();
        assert!(store
            .banned("spammer", "127.0.0.1".parse().unwrap())
            .unwrap()
            .is_none());
        store.set_permission("Visitor", "build", false).unwrap();
        assert!(!store.permission("visitor").unwrap().build);
        store.audit("CONSOLE", "/ban Spammer", "success").unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn operator_follows_verified_uuid_across_rename_and_revocation() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-op-rename-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        let uuid = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        let other = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9cb";
        store.set_op_verified("OldName", uuid).unwrap();
        assert_eq!(store.operator_level("NewName", Some(uuid)).unwrap(), 4);
        assert_eq!(store.operator_level("OldName", None).unwrap(), 0);
        assert_eq!(store.operator_level("OldName", Some(other)).unwrap(), 0);

        let uppercase = uuid.to_ascii_uppercase();
        store.set_op_verified("NewName", &uppercase).unwrap();
        let entries = store.ops().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "NewName");
        assert_eq!(store.operator_level("OldName", Some(uuid)).unwrap(), 4);

        let reloaded = Administration::new(root.clone()).unwrap();
        reloaded.revoke_op_verified("NewName", uuid).unwrap();
        assert_eq!(reloaded.operator_level("OldName", Some(uuid)).unwrap(), 0);
        assert_eq!(reloaded.operator_level("NewName", Some(uuid)).unwrap(), 0);
        assert!(reloaded.ops().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_verified_op_identity_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-op-duplicate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        store
            .set_op_verified("First", "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca")
            .unwrap();
        let mut entries = store.ops().unwrap();
        let mut duplicate = entries[0].clone();
        duplicate.name = "Second".into();
        duplicate.uuid = duplicate.uuid.map(|value| value.to_ascii_uppercase());
        entries.push(duplicate);
        fs::write(root.join("ops.json"), serde_json::to_vec(&entries).unwrap()).unwrap();
        assert!(store
            .operator_level("First", entries[0].uuid.as_deref())
            .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_uuid_ban_survives_name_change_and_reload() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-uuid-ban-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        let uuid = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        let ip: IpAddr = "192.0.2.42".parse().unwrap();
        store
            .set_player_ban_verified("OldName", Some(uuid), Some("account ban"), true)
            .unwrap();
        let reloaded = Administration::new(root.clone()).unwrap();
        assert_eq!(
            reloaded.banned_identity("NewName", Some(uuid), ip).unwrap(),
            Some("account ban".into())
        );
        assert!(reloaded
            .banned_identity("NewName", None, ip)
            .unwrap()
            .is_none());
        reloaded
            .set_player_ban_verified("OldName", Some(uuid), None, false)
            .unwrap();
        assert!(reloaded
            .banned_identity("NewName", Some(uuid), ip)
            .unwrap()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_bans_survive_recycled_names_and_selective_pardon() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-ban-recycle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        let ip: IpAddr = "192.0.2.78".parse().unwrap();
        let first = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        let second = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9cb";
        store
            .set_player_ban_verified("Reused", Some(first), Some("first owner"), true)
            .unwrap();

        // Login Start has no trusted UUID, so name-only preflight must not
        // punish an unrelated account just because it reuses an old name.
        assert_eq!(store.banned("Reused", ip).unwrap(), None);
        assert_eq!(
            store.banned_identity("Reused", Some(second), ip).unwrap(),
            None
        );
        assert_eq!(
            store.banned_identity("NewName", Some(first), ip).unwrap(),
            Some("first owner".into())
        );

        store
            .set_player_ban_verified("Reused", Some(second), Some("second owner"), true)
            .unwrap();
        let reloaded = Administration::new(root.clone()).unwrap();
        assert_eq!(reloaded.banned_players().unwrap().len(), 2);
        assert_eq!(
            reloaded
                .banned_identity("NewName", Some(first), ip)
                .unwrap(),
            Some("first owner".into())
        );
        assert_eq!(
            reloaded
                .banned_identity("Reused", Some(second), ip)
                .unwrap(),
            Some("second owner".into())
        );
        assert!(reloaded.set_player_ban("Reused", None, false).is_err());
        assert_eq!(reloaded.banned_players().unwrap().len(), 2);
        reloaded.pardon_uuid(&second.to_ascii_uppercase()).unwrap();
        assert!(reloaded
            .banned_identity("Reused", Some(second), ip)
            .unwrap()
            .is_none());
        assert!(reloaded
            .banned_identity("NewName", Some(first), ip)
            .unwrap()
            .is_some());
        reloaded.pardon_uuid(first).unwrap();
        assert!(reloaded.banned_players().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn name_only_and_ip_bans_remain_effective_before_authentication() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-ban-name-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        let ip: IpAddr = "192.0.2.79".parse().unwrap();
        let account = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        store
            .set_player_ban("Legacy", Some("name ban"), true)
            .unwrap();
        assert_eq!(store.banned("Legacy", ip).unwrap(), Some("name ban".into()));
        assert_eq!(
            store.banned_identity("Legacy", Some(account), ip).unwrap(),
            Some("name ban".into())
        );
        store
            .set_player_ban_verified("Legacy", Some(account), Some("uuid ban"), true)
            .unwrap();
        assert_eq!(store.banned_players().unwrap().len(), 2);
        store.set_player_ban("Legacy", None, false).unwrap();
        assert!(store
            .banned_identity("Renamed", Some(account), ip)
            .unwrap()
            .is_none());

        store
            .set_ip_ban("192.0.2.79", Some("ip ban"), true)
            .unwrap();
        assert_eq!(store.banned("AnyName", ip).unwrap(), Some("ip ban".into()));
        assert_eq!(
            store.banned_identity("Renamed", Some(account), ip).unwrap(),
            Some("ip ban".into())
        );
        assert!(store
            .banned_identity("Whatever", Some("bad uuid"), ip)
            .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_verified_ban_uuids_fail_closed() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-ban-uuid-duplicate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        let uuid = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        store
            .set_player_ban_verified("First", Some(uuid), None, true)
            .unwrap();
        let mut bans = store.banned_players().unwrap();
        let mut duplicate = bans[0].clone();
        duplicate.name = "Second".into();
        duplicate.uuid = Some(uuid.to_ascii_uppercase());
        bans.push(duplicate);
        fs::write(
            root.join("banned-players.json"),
            serde_json::to_vec(&bans).unwrap(),
        )
        .unwrap();
        assert!(store.banned("First", "127.0.0.1".parse().unwrap()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_and_duplicate_bans_rejected() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-admin-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = Administration::new(root.clone()).unwrap();
        fs::write(
            root.join("banned-ips.json"),
            r#"[{"ip":"192.0.2.1"},{"ip":"::ffff:192.0.2.1"}]"#,
        )
        .unwrap();
        assert!(store.ip_banned("192.0.2.1".parse().unwrap()).is_err());
        fs::write(root.join("banned-ips.json"), "broken").unwrap();
        assert!(store.banned("Alice", "127.0.0.1".parse().unwrap()).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
