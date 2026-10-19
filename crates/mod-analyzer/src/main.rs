use mod_analyzer::{inspect_jar, modpack::inspect_modpack};
use std::path::Path;

fn usage() {
    println!("Usage:");
    println!("  mod-analyzer <mod.jar> [mod2.jar ...]");
    println!("  mod-analyzer --modpack <server-directory|modpack.zip> [--json report.json]");
}

fn run_modpack(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if !(args.len() == 2 || (args.len() == 4 && args[2] == "--json")) {
        usage();
        return Err("invalid --modpack arguments".into());
    }
    let report = inspect_modpack(Path::new(&args[1]))?;
    let serialized = serde_json::to_string_pretty(&report)?;
    if args.len() == 4 {
        std::fs::write(&args[3], &serialized)?;
        println!(
            "Modpack report: {} files, written to {} (read-only inspection)",
            report.scanned_files, args[3]
        );
    } else {
        println!("{serialized}");
    }
    if report.mods.iter().any(|mod_file| mod_file.error.is_some()) {
        return Err("some Mod archives could not be inspected; see report".into());
    }
    Ok(())
}

fn main() {
    let files: Vec<_> = std::env::args().skip(1).collect();
    if files.first().is_some_and(|s| s == "--modpack") {
        if let Err(e) = run_modpack(&files) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if files.is_empty() || files.iter().any(|s| s == "--help") {
        usage();
        if files.is_empty() {
            std::process::exit(2);
        }
        return;
    }
    let mut fail = false;
    for file in files {
        match inspect_jar(Path::new(&file)) {
            Ok(r) => {
                println!(
                    "{file}: entries={} classes={} invalid={} java9plus={} mcmod.info={} coremod={} FML={} Forge={}",
                    r.entries,
                    r.classes,
                    r.invalid_classes,
                    r.post_java8_classes,
                    r.has_mcmod_info,
                    r.has_coremod_manifest,
                    r.references_fml,
                    r.references_forge
                );
                for p in r.referenced_packages {
                    println!("  API {p}");
                }
                for e in r.errors {
                    eprintln!("  WARN {e}");
                }
                if r.invalid_classes > 0 {
                    fail = true;
                }
            }
            Err(e) => {
                eprintln!("{file}: {e}");
                fail = true;
            }
        }
    }
    if fail {
        std::process::exit(1);
    }
}
