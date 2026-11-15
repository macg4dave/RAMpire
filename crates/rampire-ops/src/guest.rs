//! Reaching inside a VM from the host, so `vm add` can install or upgrade
//! its guest package: through the QEMU guest agent when it runs commands,
//! else over SSH, as the host's root, with root's own keys.
//!
//! The host package carries the guest packages of its own build in
//! [`GUEST_PACKAGES`]: the guest always gets the build the host runs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::attest::Libvirt;

/// Where the host package keeps `rampire-guest.deb` and `rampire-guest.rpm`.
pub const GUEST_PACKAGES: &str = "/usr/share/rampire/guest";

/// How the host gets in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Agent,
    Ssh(String),
}

/// A running VM the host can run commands in.
pub struct Guest<'a> {
    libvirt: &'a Libvirt,
    vm: String,
    route: Route,
}

/// What installing the guest package found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// `uname -r` in the guest.
    pub kernel: String,
    /// The guest service's build, as `rampire-guest version` prints it.
    pub build: String,
    /// The package turned on something that takes effect at the next cold
    /// start: pressure stall information on a RHEL kernel.
    pub restart_needed: bool,
}

impl<'a> Guest<'a> {
    /// The way into `vm`: `login` (`USER@HOST`) when given; else its guest
    /// agent when it runs commands; else root at the first address libvirt
    /// knows for it. A VM just started gets up to `wait` to come up.
    pub fn reach(
        libvirt: &'a Libvirt,
        vm: &str,
        login: Option<&str>,
        wait: Duration,
    ) -> Result<Self, String> {
        let guest = |route| Self {
            libvirt,
            vm: vm.to_owned(),
            route,
        };
        if let Some(login) = login {
            let guest = guest(Route::Ssh(login.to_owned()));
            guest.until_up(wait)?;
            return Ok(guest);
        }
        let deadline = Instant::now() + wait;
        loop {
            if agent_runs_commands(libvirt, vm) {
                return Ok(guest(Route::Agent));
            }
            if let Some(address) = address(libvirt, vm) {
                let guest = guest(Route::Ssh(format!("root@{address}")));
                guest.until_up(deadline.saturating_duration_since(Instant::now()))?;
                return Ok(guest);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{vm} has no address libvirt knows and no guest agent that runs commands: \
                     give its SSH login with --guest-login USER@HOST"
                ));
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    /// How it is reached, for a person: "over SSH as root@192.168.122.5".
    pub fn way(&self) -> String {
        match &self.route {
            Route::Agent => "through its guest agent".to_owned(),
            Route::Ssh(login) => format!("over SSH as {login}"),
        }
    }

    /// Wait for SSH to let the host in, as a VM that was just started boots.
    fn until_up(&self, wait: Duration) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        loop {
            match self.run("true") {
                Ok(_) => return Ok(()),
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_secs(3)),
                Err(error) => {
                    let Route::Ssh(login) = &self.route else {
                        return Err(error);
                    };
                    return Err(format!(
                        "can't log in to {} as {login}: {error}. Authorize the host root's SSH key \
                         there, or give a login that works with --guest-login USER@HOST",
                        self.vm
                    ));
                }
            }
        }
    }

    /// Run a shell script in the guest; its output, or why it failed.
    pub fn run(&self, script: &str) -> Result<String, String> {
        match &self.route {
            Route::Ssh(login) => ssh(login, &["sh", "-s"], script.as_bytes()),
            Route::Agent => self.agent_run(script),
        }
    }

    /// Copy `local` to `remote` in the guest.
    fn put(&self, local: &Path, remote: &str) -> Result<(), String> {
        let bytes =
            std::fs::read(local).map_err(|error| format!("read {}: {error}", local.display()))?;
        match &self.route {
            Route::Ssh(login) => {
                ssh(login, &["sh", "-c", &format!("cat > {remote}")], &bytes).map(|_| ())
            }
            Route::Agent => self.agent_put(&bytes, remote),
        }
    }

    /// Install or upgrade the guest package of this build, and read what
    /// `vm add` needs from the guest.
    pub fn install(&self) -> Result<Installed, String> {
        // Who it runs as, which package it takes, and its kernel.
        let probe = self.run(
            "id -u; . /etc/os-release; case \" $ID $ID_LIKE \" in \
             *' debian '*) echo deb;; *' rhel '*|*' fedora '*|*' centos '*) echo rpm;; \
             *) echo \"none $ID\";; esac; uname -r",
        )?;
        let lines: Vec<&str> = probe.lines().map(str::trim).collect();
        let [uid, kind, kernel] = lines[..] else {
            return Err(format!("{} gave an unexpected answer: {probe}", self.vm));
        };
        if kind != "deb" && kind != "rpm" {
            return Err(format!(
                "{} runs {}, which rampire-guest has no package for (Debian or RHEL family)",
                self.vm,
                kind.trim_start_matches("none ")
            ));
        }
        let package = package_path(kind);
        if !package.exists() {
            return Err(format!(
                "{} is missing: this host's RAMpire was installed without the guest packages",
                package.display()
            ));
        }
        let remote = format!("/tmp/rampire-guest.{kind}");
        self.put(&package, &remote)?;
        let sudo = if uid == "0" { "" } else { "sudo -n " };
        let install = match kind {
            "deb" => format!(
                "{sudo}env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --allow-downgrades {remote}"
            ),
            _ => format!("{sudo}dnf install -y -q {remote}"),
        };
        self.run(&format!("set -e; {install} >/dev/null; rm -f {remote}"))
            .map_err(|error| format!("install rampire-guest in {}: {error}", self.vm))?;
        let after = self.run(
            "/usr/libexec/rampire-guest version; \
             [ -e /proc/pressure/memory ] && echo psi || echo restart",
        )?;
        let mut after = after.lines().map(str::trim);
        Ok(Installed {
            kernel: kernel.to_owned(),
            build: after.next().unwrap_or_default().to_owned(),
            restart_needed: after.next() == Some("restart"),
        })
    }

    fn agent(&self, command: &serde_json::Value) -> Result<serde_json::Value, String> {
        let reply = self
            .libvirt
            .virsh(&["qemu-agent-command", &self.vm, &command.to_string()])?;
        let reply: serde_json::Value =
            serde_json::from_str(&reply).map_err(|error| format!("guest agent: {error}"))?;
        Ok(reply["return"].clone())
    }

    fn agent_run(&self, script: &str) -> Result<String, String> {
        let pid = self.agent(&serde_json::json!({
            "execute": "guest-exec",
            "arguments": {"path": "/bin/sh", "arg": ["-c", script], "capture-output": true}
        }))?["pid"]
            .as_i64()
            .ok_or("the guest agent started no process")?;
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let status = self.agent(&serde_json::json!({
                "execute": "guest-exec-status", "arguments": {"pid": pid}
            }))?;
            if status["exited"].as_bool() == Some(true) {
                let text = |key: &str| {
                    String::from_utf8_lossy(&base64_decode(status[key].as_str().unwrap_or("")))
                        .into_owned()
                };
                return match status["exitcode"].as_i64() {
                    Some(0) => Ok(text("out-data")),
                    code => Err(format!(
                        "exited with {}: {}",
                        code.map_or("a signal".to_owned(), |code| code.to_string()),
                        text("err-data").trim()
                    )),
                };
            }
            if Instant::now() > deadline {
                return Err("the command in the guest took over 10 minutes".to_owned());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn agent_put(&self, bytes: &[u8], remote: &str) -> Result<(), String> {
        let handle = self.agent(&serde_json::json!({
            "execute": "guest-file-open", "arguments": {"path": remote, "mode": "w"}
        }))?;
        for chunk in bytes.chunks(256 * 1024) {
            self.agent(&serde_json::json!({
                "execute": "guest-file-write",
                "arguments": {"handle": handle, "buf-b64": crate::tui::term::base64(chunk)}
            }))?;
        }
        self.agent(&serde_json::json!({
            "execute": "guest-file-close", "arguments": {"handle": handle}
        }))
        .map(|_| ())
    }
}

/// The guest package of this build for a `deb` or `rpm` guest.
pub fn package_path(kind: &str) -> PathBuf {
    Path::new(GUEST_PACKAGES).join(format!("rampire-guest.{kind}"))
}

/// Whether the VM's guest agent answers and runs commands and files.
fn agent_runs_commands(libvirt: &Libvirt, vm: &str) -> bool {
    let Ok(reply) = libvirt.virsh(&["qemu-agent-command", vm, r#"{"execute":"guest-info"}"#])
    else {
        return false;
    };
    let Ok(reply) = serde_json::from_str::<serde_json::Value>(&reply) else {
        return false;
    };
    let enabled = |name: &str| {
        reply["return"]["supported_commands"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|command| command["name"] == name && command["enabled"] == true)
    };
    [
        "guest-exec",
        "guest-exec-status",
        "guest-file-open",
        "guest-file-write",
        "guest-file-close",
    ]
    .iter()
    .all(|name| enabled(name))
}

/// The VM's first IPv4 address, from its DHCP lease or the host's ARP table.
fn address(libvirt: &Libvirt, vm: &str) -> Option<String> {
    ["lease", "arp"].iter().find_map(|source| {
        libvirt
            .virsh(&["domifaddr", vm, "--source", source])
            .ok()
            .and_then(|text| first_ipv4(&text))
    })
}

/// The first IPv4 address in `virsh domifaddr`'s table.
fn first_ipv4(table: &str) -> Option<String> {
    table.lines().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        let at = words.iter().position(|word| *word == "ipv4")?;
        let address = words.get(at + 1)?.split('/').next()?;
        (!address.is_empty()).then(|| address.to_owned())
    })
}

/// Run `command` over SSH as `login`, feeding it `input`; its output.
fn ssh(login: &str, command: &[&str], input: &[u8]) -> Result<String, String> {
    let mut child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ServerAliveInterval=15",
            "--",
            login,
        ])
        .args(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("run ssh: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("ssh: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("ssh: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Standard padded base64, as the guest agent sends output.
fn base64_decode(text: &str) -> Vec<u8> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let (mut bits, mut count) = (0u32, 0);
    for six in text.bytes().filter_map(value) {
        bits = bits << 6 | u32::from(six);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_ipv4_address_is_read_from_domifaddr() {
        let table = " Name       MAC address          Protocol     Address
-------------------------------------------------------------------------------
 vnet3      52:54:00:aa:bb:cc    ipv6         fe80::1/64
 vnet3      52:54:00:aa:bb:cc    ipv4         192.168.122.57/24
";
        assert_eq!(first_ipv4(table).as_deref(), Some("192.168.122.57"));
        assert_eq!(first_ipv4(" Name MAC Protocol Address\n---\n"), None);
    }

    #[test]
    fn base64_from_the_agent_decodes() {
        for text in ["", "f", "fo", "foo", "rampire-guest 0.1.0\n"] {
            let encoded = crate::tui::term::base64(text.as_bytes());
            assert_eq!(base64_decode(&encoded), text.as_bytes(), "{encoded}");
        }
    }
}
