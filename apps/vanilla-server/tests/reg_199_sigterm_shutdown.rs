//! REG-199 / #835: POSIX SIGTERM must use the same durable drain as /stop.
//! Only disposable worlds and loopback sockets are used. Not a deterministic
//! journal mid-rename fault injection; journal crash recovery is tested separately.
#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    process: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Server {
    fn launch(world: &Path, whitelist: &Path, scratch: &Path, name: &str) -> Self {
        let stdout = scratch.join(format!("{name}.stdout.log"));
        let stderr = scratch.join(format!("{name}.stderr.log"));
        let process = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", "127.0.0.1:0", "--world"])
            .arg(world)
            .arg("--whitelist")
            .arg(whitelist)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(File::create(&stdout).unwrap()))
            .stderr(Stdio::from(File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        Self {
            process,
            stdout,
            stderr,
        }
    }

    fn command(&mut self, text: &str) {
        self.process
            .stdin
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }

    fn wait_for_log(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let contents = fs::read_to_string(&self.stdout).unwrap_or_default();
            if contents.contains(needle) {
                return;
            }
            if let Some(status) = self.process.try_wait().unwrap() {
                let errors = fs::read_to_string(&self.stderr).unwrap_or_default();
                panic!("server exited before {needle:?}: {status}; stderr: {errors}");
            }
            assert!(
                Instant::now() < deadline,
                "server never printed {needle:?}; logs: {} and {}",
                self.stdout.display(),
                self.stderr.display()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "server did not finish shutdown; logs: {} and {}",
                self.stdout.display(),
                self.stderr.display()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

fn send_sigterm(pid: u32) -> io::Result<()> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("kill -TERM failed: {status}")))
    }
}

#[test]
fn sigterm_drains_console_save_then_restarts_without_losing_world_edit() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!(
        "ir1710-reg199-sigterm-{}-{stamp}",
        std::process::id()
    ));
    // Never reuse or delete a pre-existing world on test startup.
    fs::create_dir(&scratch).unwrap();
    let world = scratch.join("flat.irw");
    let whitelist = scratch.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"RustTester"}]"#).unwrap();

    let mut first = Server::launch(&world, &whitelist, &scratch, "sigterm");
    first.wait_for_log("listening on");
    // The live console intentionally keeps stdin open and blocked after this
    // command, so SIGTERM must not deadlock while joining the console worker.
    first.command("/setblock 2 70 2 stone\n");
    first.wait_for_log("Block set at 2 70 2");
    send_sigterm(first.process.id()).unwrap();
    let status = first.wait_for_exit();
    assert!(
        status.success(),
        "SIGTERM must drain and exit successfully; stderr: {}",
        fs::read_to_string(&first.stderr).unwrap_or_default()
    );
    assert!(
        fs::read_to_string(&first.stdout)
            .unwrap()
            .contains("Server safely stopped;"),
        "SIGTERM did not reach durable shutdown"
    );
    drop(first);
    assert!(world.is_file(), "the persisted IRW world is missing");
    assert!(
        matches!(
            fs::symlink_metadata(scratch.join("flat.pending-txn.json")),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        ),
        "pending redo must have been resolved before clean exit"
    );

    let mut second = Server::launch(&world, &whitelist, &scratch, "restarted");
    second.wait_for_log("listening on");
    second.command("/testforblock 2 70 2 stone\n");
    second.wait_for_log("testforblock: matched stone at 2 70 2");
    second.command("/stop\n");
    assert!(second.wait_for_exit().success(), "restart /stop failed");
    drop(second);
    fs::remove_dir_all(scratch).unwrap();
}
