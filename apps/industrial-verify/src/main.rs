//! Local-only validation coordinator. It never modifies worlds or launches GitHub Actions.
use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Options {
    automated_only: bool,
    client_report: Option<PathBuf>,
    evidence_root: Option<PathBuf>,
    output_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    NotRun,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotRun => "NOT_RUN",
        }
    }
}

struct Check {
    name: &'static str,
    command: String,
    status: Status,
    exit_code: Option<i32>,
    log: Option<PathBuf>,
    note: String,
}

fn usage() -> &'static str {
    "IndustrialRust1710 local verification (Minecraft Java 1.7.10 / Protocol 5)\n\
Usage: cargo run --release -p industrial-verify -- [OPTIONS]\n\
  --automated-only           Allow real-client E42 to remain NOT_RUN\n\
  --client-report PATH       Existing E42 human-observation JSON report\n\
  --evidence-root PATH       Directory containing E42 evidence files\n\
  --output-dir PATH          Directory for logs and report.json\n\
  -h, --help                 Show this help\n\
Without --automated-only, missing real-client evidence causes exit code 2.\n\
Exit codes: 0 = requested checks passed; 1 = failed checks; 2 = incomplete or setup error.\n"
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--automated-only" => options.automated_only = true,
            "--client-report" => {
                options.client_report = Some(PathBuf::from(
                    args.next().ok_or("--client-report requires a path")?,
                ));
            }
            "--evidence-root" => {
                options.evidence_root = Some(PathBuf::from(
                    args.next().ok_or("--evidence-root requires a path")?,
                ));
            }
            "--output-dir" => {
                options.output_dir = Some(PathBuf::from(
                    args.next().ok_or("--output-dir requires a path")?,
                ));
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    if options.client_report.is_some() != options.evidence_root.is_some() {
        return Err("--client-report and --evidence-root must be provided together".into());
    }
    if options.automated_only && options.client_report.is_some() {
        return Err("Use either --automated-only or E42 evidence options, not both".into());
    }
    Ok(Some(options))
}

fn root_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate must be in apps/industrial-verify")
        .to_path_buf()
}

fn absolute_path(value: PathBuf) -> io::Result<PathBuf> {
    if value.is_absolute() {
        Ok(value)
    } else {
        Ok(env::current_dir()?.join(value))
    }
}

fn git_head(root: &Path) -> String {
    match Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
    {
        Ok(result) if result.status.success() => {
            String::from_utf8_lossy(&result.stdout).trim().to_owned()
        }
        _ => "unknown".to_owned(),
    }
}

fn run_check(
    name: &'static str,
    executable: &str,
    args: &[&str],
    root: &Path,
    output_dir: &Path,
    log_name: &str,
) -> io::Result<Check> {
    let command = format!("{executable} {}", args.join(" "));
    let log = output_dir.join(format!("{log_name}.log"));
    println!("RUN   {name}: {command}");
    let stdout = File::create(&log)?;
    let stderr = stdout.try_clone()?;
    let outcome = Command::new(executable)
        .args(args)
        .current_dir(root)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status();
    let (status, exit_code, note) = match outcome {
        Ok(exit) if exit.success() => (Status::Pass, exit.code(), String::new()),
        Ok(exit) => (
            Status::Fail,
            exit.code(),
            format!("Command exited with status {exit}"),
        ),
        Err(error) => (
            Status::NotRun,
            None,
            format!("Could not start command: {error}"),
        ),
    };
    println!("{:<7}{name}", status.as_str());
    if status != Status::Pass {
        if let Ok(content) = fs::read_to_string(&log) {
            let tail = content.lines().rev().take(12).collect::<Vec<_>>();
            if !tail.is_empty() {
                println!("  Log tail (newest last):");
                for line in tail.into_iter().rev() {
                    println!("  {line}");
                }
            }
        }
    }
    Ok(Check {
        name,
        command,
        status,
        exit_code,
        log: Some(log),
        note,
    })
}

fn not_run(name: &'static str, note: &str) -> Check {
    println!("NOT_RUN {name}: {note}");
    Check {
        name,
        command: String::new(),
        status: Status::NotRun,
        exit_code: None,
        log: None,
        note: note.to_owned(),
    }
}

fn python_command() -> Option<&'static str> {
    let candidates: &[&str] = if cfg!(windows) {
        &["python", "py", "python3"]
    } else {
        &["python3", "python"]
    };
    for &candidate in candidates {
        if let Ok(result) = Command::new(candidate).arg("--version").output() {
            let version = format!(
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            if result.status.success() && version.contains("Python 3.") {
                return Some(candidate);
            }
        }
    }
    None
}

fn json_string(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch.is_control() => result.push_str(&format!("\\u{:04x}", ch as u32)),
            _ => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn write_report(path: &Path, sha: &str, checks: &[Check], exit_code: i32) -> io::Result<()> {
    let mut report = format!(
        "{{\n  \"schema_version\": 1,\n  \"commit\": {},\n  \"exit_code\": {},\n  \"checks\": [\n",
        json_string(sha),
        exit_code
    );
    for (index, check) in checks.iter().enumerate() {
        if index != 0 {
            report.push_str(",\n");
        }
        let log = check
            .log
            .as_ref()
            .map_or(String::from("null"), |p| json_string(&p.display().to_string()));
        let code = check
            .exit_code
            .map_or(String::from("null"), |number| number.to_string());
        report.push_str(&format!(
            "    {{\"name\":{},\"status\":{},\"command\":{},\"exit_code\":{},\"log\":{},\"note\":{}}}",
            json_string(check.name),
            json_string(check.status.as_str()),
            json_string(&check.command),
            code,
            log,
            json_string(&check.note)
        ));
    }
    report.push_str("\n  ]\n}\n");
    fs::write(path, report)
}

fn execute(options: Options) -> io::Result<i32> {
    let root = root_dir();
    let sha = git_head(&root);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let default_dir = root.join("target").join("verification").join(format!(
        "{}-{timestamp}-{}",
        sha.chars().take(12).collect::<String>(),
        std::process::id()
    ));
    let output_dir = match options.output_dir {
        Some(path) => absolute_path(path)?,
        None => default_dir,
    };
    fs::create_dir_all(&output_dir)?;
    println!("IndustrialRust1710 Minecraft verification");
    println!("Commit: {sha}");
    println!("Reports: {}", output_dir.display());
    println!("Existing worlds and GitHub Actions are never operated on.\n");

    let mut checks = Vec::new();
    let commands: [(&str, &[&str], &str); 5] = [
        ("rustfmt", &["fmt", "--all", "--", "--check"], "format"),
        (
            "Rust unit/integration/Protocol 5 TCP",
            &["test", "--workspace", "--all-targets"],
            "tests",
        ),
        (
            "Clippy strict",
            &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
            "clippy",
        ),
        (
            "Release server binaries",
            &["build", "--release", "-p", "vanilla-server", "-p", "legacy-status-server"],
            "build",
        ),
        (
            "Server --help smoke",
            &["run", "--release", "-p", "vanilla-server", "--", "--help"],
            "smoke",
        ),
    ];
    for (name, args, log) in commands {
        checks.push(run_check(name, "cargo", args, &root, &output_dir, log)?);
    }

    let python = python_command();
    match python {
        Some(executable) => checks.push(run_check(
            "Python verifier unit tests",
            executable,
            &["-m", "unittest", "discover", "-s", "tools", "-p", "test_*.py", "-v"],
            &root,
            &output_dir,
            "python",
        )?),
        None => checks.push(not_run(
            "Python verifier unit tests",
            "Python 3 is required for the existing tools/test_*.py suite",
        )),
    }

    match (options.client_report, options.evidence_root, python) {
        (Some(report), Some(evidence), Some(executable)) => {
            let report = absolute_path(report)?;
            let evidence = absolute_path(evidence)?;
            let report_text = report.to_string_lossy().to_string();
            let evidence_text = evidence.to_string_lossy().to_string();
            checks.push(run_check(
                "E42 client evidence validation",
                executable,
                &[
                    "tools/e42_acceptance.py",
                    "verify",
                    "--report",
                    &report_text,
                    "--evidence-root",
                    &evidence_text,
                    "--require-pass",
                ],
                &root,
                &output_dir,
                "e42",
            )?);
        }
        (Some(_), Some(_), None) => checks.push(not_run(
            "E42 client evidence validation",
            "Python 3 is required to validate the E42 evidence",
        )),
        _ => checks.push(not_run(
            "E42 client evidence validation",
            "Unmodified Minecraft 1.7.10 needs human observations and evidence (9 E42 cases)",
        )),
    }

    let passed = checks.iter().filter(|c| c.status == Status::Pass).count();
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let missing = checks.iter().filter(|c| c.status == Status::NotRun).count();
    let incomplete = checks.iter().any(|c| {
        c.status == Status::NotRun
            && !(options.automated_only && c.name == "E42 client evidence validation")
    });
    let exit_code = if failed > 0 {
        1
    } else if incomplete {
        2
    } else {
        0
    };
    let report_file = output_dir.join("report.json");
    write_report(&report_file, &sha, &checks, exit_code)?;
    println!("\nSummary: {passed} PASS, {failed} FAIL, {missing} NOT_RUN");
    println!("Report: {}", report_file.display());
    if options.automated_only {
        println!("Only the automated checks are an exit-code gate. E42 is NOT_RUN.");
    }
    println!(
        concat!(
            "A PASS here does not establish complete vanilla 1.7.10 gameplay compatibility, ",
            "24-case client acceptance, or coverage of all requirements."
        )
    );
    Ok(exit_code)
}

fn main() {
    let args = env::args().skip(1);
    match parse_args(args) {
        Ok(None) => print!("{}", usage()),
        Ok(Some(options)) => match execute(options) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("Verifier setup failed: {error}");
                std::process::exit(2);
            }
        },
        Err(error) => {
            eprintln!("{error}\n\n{}", usage());
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_options_must_be_paired() {
        assert!(parse_args(["--client-report".into(), "report.json".into()]).is_err());
        assert!(parse_args(["--evidence-root".into(), "evidence".into()]).is_err());
        assert!(parse_args([
            "--client-report".into(),
            "report.json".into(),
            "--evidence-root".into(),
            "evidence".into(),
        ])
        .is_ok());
    }

    #[test]
    fn json_escapes_special_characters() {
        assert_eq!(json_string("a\\b\"\n"), "\"a\\\\b\\\"\\n\"");
    }

    #[test]
    fn client_and_automated_only_are_mutually_exclusive() {
        assert!(parse_args([
            "--automated-only".into(),
            "--client-report".into(),
            "report.json".into(),
            "--evidence-root".into(),
            "evidence".into(),
        ])
        .is_err());
    }
}
