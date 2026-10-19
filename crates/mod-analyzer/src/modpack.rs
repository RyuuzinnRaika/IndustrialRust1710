//! Read-only Modpack inventory. Does not extract ZIPs or execute Java bytecode.
use crate::inspect_jar_bytes;
use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Seek};
use std::path::Path;
use zip::ZipArchive;

const MAX_PACK_ENTRIES: usize = 10_000;
const MAX_MOD_FILES: usize = 512;
const MAX_JAR_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_JAR_BYTES: u64 = 256 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Debug, Serialize)]
pub struct ModFile {
    pub path: String,
    pub jar_bytes: u64,
    pub classes: usize,
    pub invalid_classes: usize,
    pub post_java8_classes: usize,
    pub references_fml: bool,
    pub references_forge: bool,
    pub core_plugin: Option<String>,
    pub tweak_class: Option<String>,
    pub mod_ids: Vec<String>,
    /// Dependencies declared in mcmod.info only, not @Mod annotation dependencies.
    pub metadata_dependencies: Vec<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

impl ModFile {
    fn new(path: String, jar_bytes: u64) -> Self {
        Self {
            path,
            jar_bytes,
            classes: 0,
            invalid_classes: 0,
            post_java8_classes: 0,
            references_fml: false,
            references_forge: false,
            core_plugin: None,
            tweak_class: None,
            mod_ids: Vec::new(),
            metadata_dependencies: Vec::new(),
            warnings: Vec::new(),
            error: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ModpackReport {
    pub format_version: u32,
    pub source: String,
    pub minecraft_version: &'static str,
    pub forge_baseline: &'static str,
    pub scanned_files: usize,
    pub mods: Vec<ModFile>,
    pub warnings: Vec<String>,
    pub compatibility_verified: bool,
}

impl ModpackReport {
    fn new(source: &Path) -> Self {
        Self {
            format_version: 1,
            source: source.display().to_string(),
            minecraft_version: "1.7.10",
            forge_baseline: "10.13.4.1614",
            scanned_files: 0,
            mods: Vec::new(),
            warnings: Vec::new(),
            compatibility_verified: false,
        }
    }
    fn append(&mut self, info: ModFile) -> io::Result<()> {
        if self.mods.len() >= MAX_MOD_FILES {
            return Err(invalid("too many mod files"));
        }
        self.mods.push(info);
        self.scanned_files += 1;
        Ok(())
    }
}

fn is_mod_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let is_archive = lower.ends_with(".jar") || lower.ends_with(".zip");
    is_archive && normalized.split('/').any(|part| part == "mods")
}

fn manifest_value(manifest: &str, key: &str) -> Option<String> {
    // Continuation lines belong to the previous manifest attribute.
    let mut unfolded: Vec<String> = Vec::new();
    for line in manifest.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(last) = unfolded.last_mut() {
                last.push_str(rest);
            }
        } else {
            unfolded.push(line.to_string());
        }
    }
    unfolded.into_iter().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.eq_ignore_ascii_case(key) {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

fn metadata_values(data: &[u8], info: &mut ModFile) {
    let parsed: Value = match serde_json::from_slice(data) {
        Ok(value) => value,
        Err(e) => {
            info.warnings
                .push(format!("mcmod.info is not valid JSON: {e}"));
            return;
        }
    };
    let list = if let Some(list) = parsed.as_array() {
        list.clone()
    } else if let Some(obj) = parsed.as_object() {
        obj.get("modList")
            .or_else(|| obj.get("modlist"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![parsed.clone()])
    } else {
        info.warnings.push("mcmod.info has no mod list".into());
        return;
    };
    for value in list {
        let Some(obj) = value.as_object() else {
            continue;
        };
        if let Some(id) = obj
            .get("modid")
            .or_else(|| obj.get("modId"))
            .and_then(Value::as_str)
        {
            if !id.trim().is_empty() && !info.mod_ids.iter().any(|item| item == id) {
                info.mod_ids.push(id.to_string());
            }
        }
        if let Some(deps) = obj.get("dependencies") {
            match deps {
                Value::Array(items) => {
                    for dep in items.iter().filter_map(Value::as_str) {
                        if !info.metadata_dependencies.iter().any(|x| x == dep) {
                            info.metadata_dependencies.push(dep.to_string());
                        }
                    }
                }
                Value::String(s) if !s.is_empty() => {
                    info.metadata_dependencies.push(s.clone());
                }
                _ => {}
            }
        }
    }
    info.mod_ids.sort();
    info.metadata_dependencies.sort();
    info.metadata_dependencies.dedup();
}

fn inspect_mod_bytes(path: String, bytes: &[u8]) -> ModFile {
    let mut info = ModFile::new(path, bytes.len() as u64);
    match inspect_jar_bytes(bytes) {
        Ok(report) => {
            info.classes = report.classes;
            info.invalid_classes = report.invalid_classes;
            info.post_java8_classes = report.post_java8_classes;
            info.references_fml = report.references_fml;
            info.references_forge = report.references_forge;
            info.warnings.extend(report.errors);
        }
        Err(e) => {
            info.error = Some(format!("JAR static scan: {e}"));
            return info;
        }
    }
    let mut archive = match ZipArchive::new(Cursor::new(bytes)) {
        Ok(archive) => archive,
        Err(e) => {
            info.error = Some(format!("invalid JAR archive: {e}"));
            return info;
        }
    };
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            info.warnings
                .push(format!("unreadable JAR entry at index {i}"));
            continue;
        };
        let name = entry.name().to_ascii_lowercase();
        if name != "mcmod.info" && name != "meta-inf/manifest.mf" {
            continue;
        }
        if entry.size() > MAX_METADATA_BYTES {
            info.warnings
                .push(format!("{name}: metadata exceeds limit"));
            continue;
        }
        let mut data = Vec::new();
        if entry
            .take(MAX_METADATA_BYTES + 1)
            .read_to_end(&mut data)
            .is_err()
            || data.len() as u64 > MAX_METADATA_BYTES
        {
            info.warnings
                .push(format!("{name}: cannot read metadata safely"));
            continue;
        }
        if name == "mcmod.info" {
            metadata_values(&data, &mut info);
        } else {
            let manifest = String::from_utf8_lossy(&data);
            info.core_plugin = manifest_value(&manifest, "FMLCorePlugin");
            info.tweak_class = manifest_value(&manifest, "TweakClass");
        }
    }
    if info.mod_ids.is_empty() {
        info.warnings
            .push("Mod IDs not found in mcmod.info; @Mod and injected mods not resolved".into());
    }
    info
}

pub fn inspect_pack_reader<R: Read + Seek>(reader: R, source: &Path) -> io::Result<ModpackReport> {
    let mut pack = ZipArchive::new(reader).map_err(|_| invalid("invalid modpack ZIP"))?;
    if pack.len() > MAX_PACK_ENTRIES {
        return Err(invalid("too many modpack ZIP entries"));
    }
    let mut report = ModpackReport::new(source);
    let mut total = 0u64;
    for i in 0..pack.len() {
        let entry = pack
            .by_index(i)
            .map_err(|_| invalid("unreadable pack entry"))?;
        let path = entry.name().to_string();
        if entry.is_dir() || !is_mod_file(&path) {
            continue;
        }
        if entry.size() > MAX_JAR_BYTES {
            let mut info = ModFile::new(path, entry.size());
            info.error = Some("mod file exceeds 32 MiB read limit".into());
            report.append(info)?;
            continue;
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid("size overflow"))?;
        if total > MAX_TOTAL_JAR_BYTES {
            return Err(invalid("total mod bytes exceed 256 MiB limit"));
        }
        let entry_size = entry.size();
        let mut bytes = Vec::new();
        match entry.take(MAX_JAR_BYTES + 1).read_to_end(&mut bytes) {
            Ok(n) if n as u64 <= MAX_JAR_BYTES => {
                report.append(inspect_mod_bytes(path, &bytes))?;
            }
            Ok(_) => {
                let mut info = ModFile::new(path, bytes.len() as u64);
                info.error = Some("decompressed mod exceeds limit".into());
                report.append(info)?;
            }
            Err(e) => {
                let mut info = ModFile::new(path, entry_size);
                info.error = Some(format!("decompression failed: {e}"));
                report.append(info)?;
            }
        }
    }
    report.mods.sort_by(|a, b| a.path.cmp(&b.path));
    if report.mods.is_empty() {
        report
            .warnings
            .push("No mods/*.jar or mods/*.zip files found".into());
    }
    Ok(report)
}

pub fn inspect_pack_zip(path: &Path) -> io::Result<ModpackReport> {
    inspect_pack_reader(File::open(path)?, path)
}

fn scan_directory(
    dir: &Path,
    depth: usize,
    report: &mut ModpackReport,
    total: &mut u64,
) -> io::Result<()> {
    if depth > 2 {
        return Ok(());
    }
    let mut files = fs::read_dir(dir)?
        .map(|x| x.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    files.sort();
    for path in files {
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            scan_directory(&path, depth + 1, report, total)?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if !lower.ends_with(".jar") && !lower.ends_with(".zip") {
            continue;
        }
        let size = fs::metadata(&path)?.len();
        let name = path.display().to_string();
        if size > MAX_JAR_BYTES {
            let mut info = ModFile::new(name, size);
            info.error = Some("mod file exceeds 32 MiB read limit".into());
            report.append(info)?;
            continue;
        }
        *total = total
            .checked_add(size)
            .ok_or_else(|| invalid("size overflow"))?;
        if *total > MAX_TOTAL_JAR_BYTES {
            return Err(invalid("total mod bytes exceed 256 MiB limit"));
        }
        let data = fs::read(&path)?;
        report.append(inspect_mod_bytes(name, &data))?;
    }
    Ok(())
}

pub fn inspect_modpack(path: &Path) -> io::Result<ModpackReport> {
    if path.is_file() {
        return inspect_pack_zip(path);
    }
    if !path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "modpack not found"));
    }
    let dir = if path.join("mods").is_dir() {
        path.join("mods")
    } else {
        path.to_path_buf()
    };
    let mut report = ModpackReport::new(path);
    let mut total = 0;
    scan_directory(&dir, 0, &mut report, &mut total)?;
    report.mods.sort_by(|a, b| a.path.cmp(&b.path));
    if report.mods.is_empty() {
        report
            .warnings
            .push("No mod archives found under directory".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for &(name, bytes) in entries {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_nested_modpack_without_extraction() {
        let jar = archive(&[
            (
                "mcmod.info",
                br#"[{"modid":"BuildCraft|Core","dependencies":["required-after:Forge"]}]"#,
            ),
            (
                "META-INF/MANIFEST.MF",
                b"Manifest-Version: 1.0\r\nFMLCorePlugin: example.Plugin\r\n",
            ),
        ]);
        let pack = archive(&[
            ("pack/mods/buildcraft.jar", &jar),
            ("pack/world/level.dat", b"private"),
        ]);
        let report = inspect_pack_reader(Cursor::new(pack), Path::new("test.zip")).unwrap();
        assert_eq!(report.scanned_files, 1);
        assert_eq!(report.mods[0].mod_ids, ["BuildCraft|Core"]);
        assert_eq!(
            report.mods[0].metadata_dependencies,
            ["required-after:Forge"]
        );
        assert_eq!(
            report.mods[0].core_plugin.as_deref(),
            Some("example.Plugin")
        );
        assert!(!report.compatibility_verified);
    }

    #[test]
    fn ignores_non_mod_paths_and_rejects_invalid_zip() {
        let pack = archive(&[("world/foo.jar", b"secret"), ("mods/a.txt", b"anything")]);
        let report = inspect_pack_reader(Cursor::new(pack), Path::new("test.zip")).unwrap();
        assert!(report.mods.is_empty());
        assert!(inspect_pack_reader(Cursor::new(b"bad zip"), Path::new("bad.zip")).is_err());
    }

    #[test]
    fn parses_manifest_continuation_lines() {
        assert_eq!(
            manifest_value("FMLCorePlugin: hello.\n World\n", "fmlcoreplugin").as_deref(),
            Some("hello.World")
        );
    }
}
