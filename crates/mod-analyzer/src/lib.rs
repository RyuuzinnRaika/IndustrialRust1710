//! Read-only Java ClassFile / Forge JAR static scanner; does not execute Mod code.
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use zip::ZipArchive;
const MAX_ENTRIES: usize = 100_000;
const MAX_CLASS_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CLASS_TOTAL: u64 = 256 * 1024 * 1024;
fn bad(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
struct Reader<'a> {
    input: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.offset.checked_add(n).ok_or_else(|| bad("overflow"))?;
        let s = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| bad("truncated class"))?;
        self.offset = end;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let a = self.take(2)?;
        Ok(u16::from_be_bytes([a[0], a[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let a = self.take(4)?;
        Ok(u32::from_be_bytes([a[0], a[1], a[2], a[3]]))
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ClassSummary {
    pub name: String,
    pub major_version: u16,
    pub references: BTreeSet<String>,
}
pub fn inspect_class(bytes: &[u8]) -> io::Result<ClassSummary> {
    let mut p = Reader {
        input: bytes,
        offset: 0,
    };
    if p.u32()? != 0xcafebabe {
        return Err(bad("not a Java class"));
    }
    let _minor = p.u16()?;
    let major = p.u16()?;
    let count = p.u16()? as usize;
    if count == 0 {
        return Err(bad("invalid pool size"));
    }
    let mut utf8 = vec![None; count];
    let mut classes = vec![None; count];
    let mut i = 1;
    while i < count {
        let tag = p.u8()?;
        match tag {
            1 => {
                let n = p.u16()? as usize;
                utf8[i] = Some(String::from_utf8_lossy(p.take(n)?).into_owned());
            }
            3 | 4 => {
                p.take(4)?;
            }
            5 | 6 => {
                p.take(8)?;
                i += 1;
                if i >= count {
                    return Err(bad("invalid wide pool entry"));
                }
            }
            7 => {
                classes[i] = Some(p.u16()? as usize);
            }
            8 | 16 | 19 | 20 => {
                p.take(2)?;
            }
            9 | 10 | 11 | 12 | 17 | 18 => {
                p.take(4)?;
            }
            15 => {
                p.take(3)?;
            }
            _ => return Err(bad("unknown pool tag")),
        }
        i += 1;
    }
    let _flags = p.u16()?;
    let class = p.u16()? as usize;
    let _super = p.u16()?;
    let index = classes
        .get(class)
        .copied()
        .flatten()
        .ok_or_else(|| bad("missing class ref"))?;
    let name = utf8
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| bad("missing name"))?
        .clone();
    let mut references = BTreeSet::new();
    for index in classes.into_iter().flatten() {
        let n = utf8
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| bad("invalid class reference"))?;
        references.insert(n.clone());
    }
    Ok(ClassSummary {
        name,
        major_version: major,
        references,
    })
}
#[derive(Debug, Default)]
pub struct JarReport {
    pub entries: usize,
    pub classes: usize,
    pub invalid_classes: usize,
    pub post_java8_classes: usize,
    pub has_mcmod_info: bool,
    pub has_coremod_manifest: bool,
    pub references_fml: bool,
    pub references_forge: bool,
    pub referenced_packages: BTreeSet<String>,
    pub errors: Vec<String>,
}
pub fn inspect_jar(path: &Path) -> io::Result<JarReport> {
    let f = File::open(path)?;
    let mut archive = ZipArchive::new(f).map_err(|_| bad("invalid JAR"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(bad("too many JAR entries"));
    }
    let mut report = JarReport {
        entries: archive.len(),
        ..JarReport::default()
    };
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|_| bad("bad JAR entry"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if name.eq_ignore_ascii_case("mcmod.info") {
            report.has_mcmod_info = true;
        }
        if name.eq_ignore_ascii_case("META-INF/MANIFEST.MF") {
            if entry.size() > 131072 {
                return Err(bad("manifest too large"));
            }
            let mut bytes = Vec::new();
            entry.take(131073).read_to_end(&mut bytes)?;
            if bytes.len() > 131072 {
                return Err(bad("manifest read limit"));
            }
            report.has_coremod_manifest = String::from_utf8_lossy(&bytes).lines().any(|s| {
                s.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("fmlcoreplugin:")
            });
            continue;
        }
        if !name.ends_with(".class") {
            continue;
        }
        report.classes += 1;
        if entry.size() > MAX_CLASS_BYTES {
            report.invalid_classes += 1;
            report.errors.push(format!("{name}: oversized class"));
            continue;
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| bad("total overflow"))?;
        if total > MAX_CLASS_TOTAL {
            return Err(bad("too many decompressed class bytes"));
        }
        let mut bytes = Vec::new();
        entry.take(MAX_CLASS_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CLASS_BYTES {
            return Err(bad("class read limit"));
        }
        match inspect_class(&bytes) {
            Ok(class) => {
                if class.major_version > 52 {
                    report.post_java8_classes += 1;
                }
                for r in class.references {
                    if r.starts_with("cpw/mods/fml/") {
                        report.references_fml = true;
                    }
                    if r.starts_with("net/minecraftforge/") {
                        report.references_forge = true;
                    }
                    if r.starts_with("cpw/mods/fml/")
                        || r.starts_with("net/minecraftforge/")
                        || r.starts_with("net/minecraft/")
                    {
                        let p = r.rsplit_once('/').map_or(r.as_str(), |(p, _)| p);
                        report.referenced_packages.insert(p.to_owned());
                    }
                }
            }
            Err(e) => {
                report.invalid_classes += 1;
                if report.errors.len() < 32 {
                    report.errors.push(format!("{name}: {e}"));
                }
            }
        }
    }
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn demo() -> Vec<u8> {
        let mut b = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52, 0, 3, 1, 0, 4];
        b.extend_from_slice(b"Test");
        b.extend_from_slice(&[7, 0, 1, 0, 1, 0, 2, 0, 0]);
        b
    }
    #[test]
    fn reads_class() {
        let c = inspect_class(&demo()).unwrap();
        assert_eq!(c.name, "Test");
        assert_eq!(c.major_version, 52);
        assert!(c.references.contains("Test"));
    }
    #[test]
    fn detects_corruption() {
        let mut d = demo();
        d[0] = 0;
        assert!(inspect_class(&d).is_err());
        assert!(inspect_class(&demo()[..12]).is_err());
    }
}
