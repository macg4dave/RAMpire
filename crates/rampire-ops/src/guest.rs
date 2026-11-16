//! Reaching inside a VM from the host, so `vm add` can install or upgrade
//! its guest package: through the QEMU guest agent when it runs commands,
//! else over SSH with RAMpire's own key ([`GUEST_KEY`], made on first use)
//! beside root's; and when neither gets in, with libvirt's disk tools while
//! the VM is shut down ([`install_offline`]), the install running at its
//! next boot.
//!
//! The host package carries the guest packages of its own build in
//! [`GUEST_PACKAGES`]: the guest always gets the build the host runs.
//! A system without dnf or apt, or an image-based one whose `/usr` is read
//! only (Fedora CoreOS, Silverblue), gets the static binary of the same
//! build in `/usr/local/libexec` and its own unit in `/etc/systemd/system`
//! instead: [`Kind::Local`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rampire_core::RawTelemetryEnvelope;

use crate::attest::Libvirt;

/// Where the host package keeps `rampire-guest.deb` and `rampire-guest.rpm`.
pub const GUEST_PACKAGES: &str = "/usr/share/rampire/guest";

/// RAMpire's SSH key for logging in to guests; `.pub` beside it.
pub const GUEST_KEY: &str = "/var/lib/rampire/ssh/id_ed25519";

/// The SSH option naming the guests' host keys RAMpire has seen.
const KNOWN_HOSTS_OPTION: &str = "UserKnownHostsFile=/var/lib/rampire/ssh/known_hosts";

/// RAMpire's public SSH key, made the first time it is needed.
pub fn public_key() -> Result<String, String> {
    let key = Path::new(GUEST_KEY);
    if !key.exists() {
        let parent = key.parent().ok_or("the key has no folder")?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
        let output = Command::new("ssh-keygen")
            .args([
                "-q", "-t", "ed25519", "-N", "", "-C", "rampire", "-f", GUEST_KEY,
            ])
            .output()
            .map_err(|error| format!("run ssh-keygen: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "ssh-keygen: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    let public = format!("{GUEST_KEY}.pub");
    std::fs::read_to_string(&public)
        .map(|text| text.trim().to_owned())
        .map_err(|error| format!("read {public}: {error}"))
}

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

/// How the guest service is installed in a system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The `.deb`, with apt.
    Deb,
    /// The RPM, with dnf.
    Rpm,
    /// The static binary in [`LOCAL_BINARY`] and the unit in
    /// [`LOCAL_UNIT`], for a systemd system without dnf or apt, or one
    /// whose `/usr` is read only (OSTree: Fedora CoreOS, Silverblue).
    Local,
}

/// Where a local install puts the static binary: `/usr/local` is writable
/// on image-based systems (on OSTree it is `/var/usrlocal`).
pub const LOCAL_BINARY: &str = "/usr/local/libexec/rampire-guest";

/// Where a local install puts its unit, which runs [`LOCAL_BINARY`].
pub const LOCAL_UNIT: &str = "/etc/systemd/system/rampire-guest.service";

/// The oldest systemd the unit runs on: it opens the control channel with
/// `OpenFile=`, new in 253.
const OLDEST_SYSTEMD: u32 = 253;

/// Asks a running guest who the login is, how the guest service installs
/// in it, and its kernel, one answer a line.
const PROBE: &str = r#"id -u
if [ -e /run/ostree-booted ]; then echo local
elif command -v apt-get >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1; then echo deb
elif command -v dnf >/dev/null 2>&1; then echo rpm
elif [ -d /run/systemd/system ]; then echo local
else [ -r /etc/os-release ] && . /etc/os-release; echo "none ${PRETTY_NAME:-${ID:-an unknown system}}"
fi
uname -r
"#;

/// Installs or upgrades the guest service in a running guest from the
/// static binary and unit put in `/tmp`, as the packages' scripts do:
/// enabled on the first install only, so an administrator's `disable`
/// stays, and restarted onto the new binary.
const LOCAL_INSTALL: &str = r#"set -e
version=$(systemctl --version | sed -n '1s/^systemd \([0-9]*\).*/\1/p')
if [ "${version:-0}" -lt OLDEST ]; then
    echo "it has systemd ${version:-of an unknown version}; the guest service needs OLDEST or newer" >&2
    exit 1
fi
first=false
[ -e UNIT ] || first=true
install -D -m 0755 /tmp/rampire-guest BINARY
install -D -m 0644 /tmp/rampire-guest.service UNIT
rm -f /tmp/rampire-guest /tmp/rampire-guest.service /tmp/rampire-guest-install.sh
if command -v restorecon >/dev/null 2>&1; then
    restorecon -F BINARY UNIT 2>/dev/null || true
fi
systemctl daemon-reload
if $first; then systemctl enable rampire-guest.service; fi
if systemctl is-enabled --quiet rampire-guest.service; then
    systemctl restart rampire-guest.service
fi
"#;

/// [`LOCAL_INSTALL`] with its paths filled in.
fn local_install_script() -> String {
    LOCAL_INSTALL
        .replace("OLDEST", &OLDEST_SYSTEMD.to_string())
        .replace("BINARY", LOCAL_BINARY)
        .replace("UNIT", LOCAL_UNIT)
}

/// What the probe's answer says: the login's user id, how to install, the
/// kernel; or why the guest service can't be installed.
fn read_probe(vm: &str, probe: &str) -> Result<(String, Kind, String), String> {
    let lines: Vec<&str> = probe.lines().map(str::trim).collect();
    let [uid, kind, kernel] = lines[..] else {
        return Err(format!("{vm} gave an unexpected answer: {probe}"));
    };
    let kind = match kind {
        "deb" => Kind::Deb,
        "rpm" => Kind::Rpm,
        "local" => Kind::Local,
        other => {
            return Err(format!(
                "{vm} runs {}, which has no dnf, apt or systemd, so rampire-guest can't be installed in it",
                other.trim_start_matches("none ")
            ))
        }
    };
    Ok((uid.to_owned(), kind, kernel.to_owned()))
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
    /// knows for it. A VM just started gets up to `wait` to come up, or
    /// until `cancel` is set.
    pub fn reach(
        libvirt: &'a Libvirt,
        vm: &str,
        login: Option<&str>,
        wait: Duration,
        cancel: &crate::ui::Cancel,
    ) -> Result<Self, String> {
        let guest = |route| Self {
            libvirt,
            vm: vm.to_owned(),
            route,
        };
        if let Some(login) = login {
            let guest = guest(Route::Ssh(login.to_owned()));
            guest.until_up(wait, cancel)?;
            return Ok(guest);
        }
        let deadline = Instant::now() + wait;
        loop {
            cancel.check(&format!("{vm} to boot"))?;
            if agent_runs_commands(libvirt, vm) {
                return Ok(guest(Route::Agent));
            }
            if let Some(address) = address(libvirt, vm) {
                let guest = guest(Route::Ssh(format!("root@{address}")));
                guest.until_up(deadline.saturating_duration_since(Instant::now()), cancel)?;
                return Ok(guest);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{vm} has no address libvirt knows and no guest agent that runs commands: \
                     give its SSH login with --guest-login USER@HOST"
                ));
            }
            cancel.sleep(Duration::from_secs(3));
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
    fn until_up(&self, wait: Duration, cancel: &crate::ui::Cancel) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        loop {
            cancel.check(&format!("{} to let RAMpire log in", self.vm))?;
            match self.run("true") {
                Ok(_) => return Ok(()),
                // A VM still booting refuses or doesn't answer yet; a guest
                // that turns the key down won't change its mind, and asking
                // again only trips its rate limits.
                Err(error) if Instant::now() < deadline && !error.contains("Permission denied") => {
                    cancel.sleep(Duration::from_secs(5));
                }
                Err(error) => {
                    let Route::Ssh(login) = &self.route else {
                        return Err(error);
                    };
                    return Err(format!(
                        "can't log in to {} as {login}: {} (RAMpire's key, to let it in next time: {}.pub)",
                        self.vm,
                        error.trim_end_matches('.'),
                        GUEST_KEY
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
        // Who it runs as, how the guest service installs, and its kernel.
        let (uid, kind, kernel) = read_probe(&self.vm, &self.run(PROBE)?)?;
        let sudo = if uid == "0" { "" } else { "sudo -n " };
        let binary = match kind {
            Kind::Deb | Kind::Rpm => {
                let extension = if kind == Kind::Deb { "deb" } else { "rpm" };
                let package = present(package_path(extension))?;
                let remote = format!("/tmp/rampire-guest.{extension}");
                self.put(&package, &remote)?;
                let install = match kind {
                    Kind::Deb => format!(
                        "{sudo}env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --allow-downgrades {remote}"
                    ),
                    _ => format!("{sudo}dnf install -y -q {remote}"),
                };
                self.run(&format!("set -e; {install} >/dev/null; rm -f {remote}"))
                    .map_err(|error| format!("install rampire-guest in {}: {error}", self.vm))?;
                "/usr/libexec/rampire-guest"
            }
            Kind::Local => {
                self.put(&present(static_binary_path())?, "/tmp/rampire-guest")?;
                self.put(&present(local_unit_path())?, "/tmp/rampire-guest.service")?;
                let script = crate::context::TempDir::new(&self.vm)?;
                let path = script.path().join("install.sh");
                std::fs::write(&path, local_install_script()).map_err(|error| error.to_string())?;
                self.put(&path, "/tmp/rampire-guest-install.sh")?;
                self.run(&format!(
                    "{sudo}sh /tmp/rampire-guest-install.sh >/dev/null"
                ))
                .map_err(|error| {
                    format!(
                        "install rampire-guest in {} as {LOCAL_BINARY}: {error}",
                        self.vm
                    )
                })?;
                LOCAL_BINARY
            }
        };
        let after = self.run(&format!(
            "{binary} version; [ -e /proc/pressure/memory ] && echo psi || echo restart"
        ))?;
        let mut after = after.lines().map(str::trim);
        let build = after.next().unwrap_or_default().to_owned();
        Ok(Installed {
            kernel,
            build,
            // The packages turn pressure stall information on with grubby,
            // for the next start; a local install leaves the kernel as it is.
            restart_needed: kind != Kind::Local && after.next() == Some("restart"),
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
        // Each chunk goes base64-encoded in one argument to virsh, and Linux
        // takes no argument over 128 KiB (MAX_ARG_STRLEN): 64 KiB is about
        // 87 KiB encoded, with room for the JSON around it.
        for chunk in bytes.chunks(AGENT_CHUNK) {
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

/// The bytes of a file each guest-agent write carries; see `agent_put`.
const AGENT_CHUNK: usize = 64 * 1024;

/// What the guest service is doing inside a running VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestService {
    /// `systemctl is-active`: `active`, `activating` (restarting after it
    /// stopped), `failed`, `inactive`...
    pub state: String,
    /// The error it last logged, in its own words, without the layers it
    /// was passed up through.
    pub error: Option<String>,
}

impl GuestService {
    /// Its state, said for a person.
    pub fn described(&self) -> &'static str {
        match self.state.as_str() {
            "active" => "runs",
            // systemd starts it again after it stopped on an error.
            "activating" => "keeps stopping and being started again",
            "failed" => "stopped on an error",
            "inactive" => "isn't started",
            _ => "isn't running",
        }
    }
}

/// What the guest service is doing inside a running VM, read through its
/// guest agent: `systemctl is-active` and the error in its last log lines.
/// `None` when the agent can't run commands, so nothing can be read from
/// inside.
pub fn service_state(libvirt: &Libvirt, vm: &str) -> Option<GuestService> {
    if !agent_runs_commands(libvirt, vm) {
        return None;
    }
    let guest = Guest {
        libvirt,
        vm: vm.to_owned(),
        route: Route::Agent,
    };
    // is-active exits non-zero when the service isn't active: `|| true`
    // keeps its answer.
    let text = guest
        .run(
            "systemctl is-active rampire-guest.service 2>/dev/null || true; \
             journalctl -u rampire-guest.service -n 10 --no-pager -o cat 2>/dev/null || true",
        )
        .ok()?;
    Some(guest_service_in(&text))
}

/// [`service_state`]'s answer: the state on the first line, the log after.
fn guest_service_in(text: &str) -> GuestService {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let state = lines.next().unwrap_or("unknown").to_owned();
    let error = lines.filter_map(guest_error).next_back();
    GuestService { state, error }
}

/// The error in one of the guest service's log lines, if it says one: its
/// last failed poll, or why it stopped. systemd's own lines about the unit
/// say nothing the state doesn't.
fn guest_error(line: &str) -> Option<String> {
    let (_, error) = line
        .split_once("rampire-guest failed: ")
        .or_else(|| line.split_once("poll skipped; reclaim history reset: "))?;
    let mut error = error.trim();
    // The layers an error is passed up through, outermost first.
    loop {
        let inner = match error.split_once(" consecutive polls failed: ") {
            Some((count, rest)) if count.bytes().all(|b| b.is_ascii_digit()) => rest,
            _ => [
                "guest memory request failed: ",
                "target geometry is invalid: ",
                "target policy is invalid: ",
            ]
            .iter()
            .find_map(|layer| error.strip_prefix(layer))
            .unwrap_or(error),
        };
        if inner == error {
            break;
        }
        error = inner;
    }
    Some(error.trim_end_matches('.').to_owned())
}

/// The build a guest's service runs, from its publication.
pub fn publication_build(publication: Option<&RawTelemetryEnvelope>) -> Option<&str> {
    publication?
        .diagnostics
        .as_ref()
        .map(|diagnostics| diagnostics.build_id.as_str())
        .filter(|build| !build.is_empty())
}

/// Whether the guest runs another build than `host_build`, the build of
/// the guest packages this host carries: an update is available. Unknown
/// (no publication, or a host build without an identity) is not.
pub fn update_available(
    host_build: Option<&str>,
    publication: Option<&RawTelemetryEnvelope>,
) -> bool {
    matches!(
        (host_build, publication_build(publication)),
        (Some(host), Some(guest)) if host != guest
    )
}

/// The build `vm`'s guest service publishes now, from the controller's copy.
pub fn running_build(vm: &str) -> Option<String> {
    let publication =
        crate::top::read_publication(&rampire_host::guest_channel::publication_copy_path(vm));
    publication_build(publication.as_ref()).map(str::to_owned)
}

/// Whether libguestfs' `virt-customize` and `virt-ls` are on the host, for
/// [`install_offline`].
pub fn offline_available() -> bool {
    ["virt-customize", "virt-ls"].iter().all(|tool| {
        Command::new(tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Where the packages wait in the guest for its next boot.
const OFFLINE_FOLDER: &str = "/var/lib/rampire-guest-install";

/// The first-boot scripts earlier installs queued that haven't run yet:
/// virt-customize keeps them, numbered, under the script's own name.
const OFFLINE_SCRIPTS: &str = "/usr/lib/virt-sysprep/scripts/*rampire-guest-install*";

/// Run at the guest's next boot: install the guest service the way a
/// login would ([`PROBE`]): the package its system takes, else the static
/// binary and its own unit ([`Kind::Local`]); then boot again when the
/// package turned on what needs a fresh start.
///
/// rpm runs without its plugins: from the first-boot service, its SELinux
/// plugin can't move the package's scripts into their own domain, so every
/// script fails ("failed to exec scriptlet interpreter /bin/sh") and the
/// older package is never removed. The files get their SELinux labels after,
/// as the plugin would have given them, and the service is restarted onto
/// the new binary whatever the package's scripts did.
const FIRST_BOOT: &str = r#"#!/bin/sh
# Written by rampire: installs the guest service, then goes.
set -e
dir=/var/lib/rampire-guest-install
standalone=false
if command -v apt-get >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1; then
    dpkg -i "$dir/rampire-guest.deb"
elif command -v dnf >/dev/null 2>&1; then
    rpm -U --replacepkgs --noplugins "$dir/rampire-guest.rpm"
    if command -v restorecon >/dev/null 2>&1; then
        rpm -ql rampire-guest | xargs -r restorecon -F 2>/dev/null || true
    fi
else
    standalone=true
    first=false
    [ -e UNIT ] || first=true
    install -D -m 0755 "$dir/rampire-guest" BINARY
    install -D -m 0644 "$dir/rampire-guest.service" UNIT
    if command -v restorecon >/dev/null 2>&1; then
        restorecon -F BINARY UNIT 2>/dev/null || true
    fi
    systemctl daemon-reload
    if $first; then systemctl enable rampire-guest.service; fi
fi
rm -rf "$dir"
systemctl daemon-reload
# The package turned on pressure stall information for the next start:
# boot again first, and the service starts then, so no request of its is
# cut off by the reboot.
if ! $standalone && [ ! -e /proc/pressure/memory ]; then
    systemctl reboot
    exit 0
fi
if systemctl is-enabled --quiet rampire-guest.service; then
    systemctl restart rampire-guest.service
fi
"#;

/// [`FIRST_BOOT`] with its paths filled in.
fn first_boot_script() -> String {
    FIRST_BOOT
        .replace("BINARY", LOCAL_BINARY)
        .replace("UNIT", LOCAL_UNIT)
}

/// Whether `vm`'s disk holds an OSTree system (Fedora CoreOS, Silverblue),
/// read as [`inspect_disk`] reads it. Its `/usr` is a deployment's, made of
/// files the OSTree repository shares, so nothing may be written there
/// from outside: the first-boot install can't be put on its disk.
fn is_ostree(vm: &str) -> Result<bool, String> {
    let output = Command::new("virt-ls")
        .env("LIBGUESTFS_BACKEND", "direct")
        .args(["-d", vm, "/"])
        .output()
        .map_err(|error| format!("run virt-ls: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "virt-ls: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|entry| entry.trim() == "ostree"))
}

/// Install the guest package of this build in a VM without logging in:
/// with it shut down (it is, when `running` is false; else it is shut down
/// here), put the packages on its disk and an install that runs at its
/// next boot. The VM is left shut off for the caller to start. Returns
/// its newest installed kernel.
/// What a VM's disk says about the system on it, read with
/// virt-inspector: its name, the package format its package manager
/// installs (`rpm`, `deb`) when it has one, and the type of its root file
/// system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSystem {
    pub product: Option<String>,
    pub package_format: Option<String>,
    pub root_filesystem: Option<String>,
}

/// Systems whose root can't be written at all, so the guest service has
/// nowhere to go, known by name when the file system doesn't say.
const READ_ONLY_SYSTEMS: &[&str] = &["Home Assistant OS"];

impl DiskSystem {
    /// Whether the guest service can be installed in it: as a package, or
    /// as the static binary with its own unit ([`Kind::Local`]). A root
    /// file system that is read only by its kind (squashfs, erofs), as Home
    /// Assistant OS has, takes neither.
    pub fn takes_guest(&self) -> bool {
        let read_only_root = matches!(
            self.root_filesystem.as_deref(),
            Some("squashfs" | "erofs" | "iso9660")
        );
        let read_only_system = self.product.as_deref().is_some_and(|product| {
            READ_ONLY_SYSTEMS
                .iter()
                .any(|known| product.starts_with(known))
        });
        !read_only_root && !read_only_system
    }

    fn from_inspector(xml: &str) -> Self {
        let first = |within: &str, tag: &str| {
            let open = format!("<{tag}>");
            let start = within.find(&open)? + open.len();
            let end = within[start..].find(&format!("</{tag}>"))? + start;
            Some(within[start..end].trim().to_owned()).filter(|text| !text.is_empty())
        };
        // The root's entry among the file systems: <filesystem dev="ROOT">.
        let root_filesystem = first(xml, "root").and_then(|root| {
            let entry = format!("<filesystem dev=\"{root}\">");
            let start = xml.find(&entry)? + entry.len();
            let end = xml[start..].find("</filesystem>")? + start;
            first(&xml[start..end], "type")
        });
        Self {
            product: first(xml, "product_name"),
            package_format: first(xml, "package_format").filter(|format| format != "unknown"),
            root_filesystem,
        }
    }
}

/// Read the system on `vm`'s disk, read-only. libguestfs runs QEMU
/// itself (its direct backend) and reads a snapshot of the disk, so a
/// running VM can be read too: libvirt's own backend can't take a running
/// VM's locked disk.
pub fn inspect_disk(vm: &str) -> Result<DiskSystem, String> {
    let output = Command::new("virt-inspector")
        .env("LIBGUESTFS_BACKEND", "direct")
        .args(["-d", vm, "--no-applications", "--no-icon"])
        .output()
        .map_err(|error| format!("run virt-inspector: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "virt-inspector: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(DiskSystem::from_inspector(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub fn install_offline(libvirt: &Libvirt, vm: &str, running: bool) -> Result<String, String> {
    if !offline_available() {
        return Err(
            "libvirt's disk tools aren't installed: install guestfs-tools (RHEL, Fedora) or libguestfs-tools (Debian)"
                .to_owned(),
        );
    }
    for path in [
        package_path("deb"),
        package_path("rpm"),
        static_binary_path(),
        local_unit_path(),
    ] {
        present(path)?;
    }
    // Read before it is shut down, so an OSTree VM is left running.
    if is_ostree(vm)? {
        return Err(format!(
            "{vm} runs an OSTree system (Fedora CoreOS, Silverblue): rampire-guest can't be put on its disk from outside, only installed through a login. Start {vm} with its guest agent running, or give an SSH login with --guest-login USER@HOST"
        ));
    }
    if running {
        crate::attest::shut_down(vm, libvirt, Duration::from_secs(300))?;
    }
    let installed = put_on_disk(vm);
    // A VM shut down for the install is started again whatever happened,
    // so a failed install doesn't leave it off.
    if let (true, Err(failed)) = (running, &installed) {
        if let Err(error) = libvirt.virsh(&["start", vm]) {
            return Err(format!(
                "{failed}; and {vm}, shut down for the install, didn't start again: {error}"
            ));
        }
    }
    installed
}

/// The guest packages and the first-boot script that installs them, put on
/// the shut-down `vm`'s disk; its newest kernel release.
fn put_on_disk(vm: &str) -> Result<String, String> {
    let modules = tool("virt-ls", &["-d", vm, "/lib/modules"])?;
    let kernel = newest(
        modules
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
    )
    .ok_or_else(|| format!("{vm} has no kernel in /lib/modules"))?;
    let temporary = crate::context::TempDir::new(vm)?;
    let script = temporary.path().join("rampire-guest-install.sh");
    std::fs::write(&script, first_boot_script()).map_err(|error| error.to_string())?;
    let upload = |path: PathBuf| {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{}:{OFFLINE_FOLDER}/{name}", path.display())
    };
    let script = script
        .to_str()
        .ok_or("the install script's path is not UTF-8")?
        .to_owned();
    // Another reader of the disk (the TUI reading what system a VM runs, a
    // backup) holds a lock for a few seconds: wait for it, a little.
    let mut tries = 0;
    loop {
        tries += 1;
        match tool(
            "virt-customize",
            &[
                "-q",
                "-d",
                vm,
                "--no-network",
                // An earlier install that never ran would run too, and could
                // install an older build after this one: only this one stays.
                "--delete",
                OFFLINE_SCRIPTS,
                "--delete",
                OFFLINE_FOLDER,
                "--mkdir",
                OFFLINE_FOLDER,
                "--upload",
                &upload(package_path("deb")),
                "--upload",
                &upload(package_path("rpm")),
                "--upload",
                &upload(static_binary_path()),
                "--upload",
                &upload(local_unit_path()),
                "--firstboot",
                &script,
                "--selinux-relabel",
            ],
        ) {
            Ok(_) => break,
            Err(error) if is_lock_error(&error) && tries < LOCK_TRIES => {
                std::thread::sleep(Duration::from_secs(5));
            }
            Err(error) if is_lock_error(&error) => {
                return Err(format!(
                    "{error}\nsomething else had {vm}'s disk open for over {} s: try again once nothing else uses it",
                    LOCK_TRIES * 5
                ))
            }
            Err(error) => return Err(error),
        }
    }
    Ok(kernel)
}

/// How many times a disk tool tries for a disk another process has locked,
/// 5 s apart.
const LOCK_TRIES: u64 = 8;

/// Whether a libguestfs tool failed because another process holds the
/// disk's lock (QEMU's image locking).
fn is_lock_error(error: &str) -> bool {
    error.contains("Failed to get \"write\" lock")
        || error.contains("Failed to get shared \"write\" lock")
}

/// Run a libguestfs tool; its output, or why it failed.
fn tool(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("run {program}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The newest of these kernel releases, comparing their numbers as numbers.
fn newest<'a>(releases: impl Iterator<Item = &'a str>) -> Option<String> {
    let key = |release: &str| -> Vec<u64> {
        release
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    releases
        .filter(|release| release.starts_with(|c: char| c.is_ascii_digit()))
        .max_by(|a, b| key(a).cmp(&key(b)))
        .map(str::to_owned)
}

/// The guest package of this build for a `deb` or `rpm` guest.
pub fn package_path(kind: &str) -> PathBuf {
    Path::new(GUEST_PACKAGES).join(format!("rampire-guest.{kind}"))
}

/// The static guest binary of this build, for [`Kind::Local`].
pub fn static_binary_path() -> PathBuf {
    Path::new(GUEST_PACKAGES).join("rampire-guest")
}

/// The unit a local install puts in [`LOCAL_UNIT`]; it runs [`LOCAL_BINARY`].
pub fn local_unit_path() -> PathBuf {
    Path::new(GUEST_PACKAGES).join("rampire-guest.service")
}

/// `path`, when the host package put it there.
fn present(path: PathBuf) -> Result<PathBuf, String> {
    if path.exists() {
        Ok(path)
    } else {
        Err(format!(
            "{} is missing: this host's RAMpire was installed without the guest packages",
            path.display()
        ))
    }
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
    public_key()?;
    let mut child = Command::new("ssh")
        .args([
            "-i",
            GUEST_KEY,
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ServerAliveInterval=15",
            // Its own host keys, beside its key, not root's; and only
            // errors, so a notice such as "Permanently added" doesn't end
            // up in what RAMpire says.
            "-o",
            KNOWN_HOSTS_OPTION,
            "-o",
            "LogLevel=ERROR",
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
    fn an_update_is_available_when_the_guest_runs_another_build() {
        let text = include_str!("../../../fixtures/publication.json");
        let mut publication: RawTelemetryEnvelope = serde_json::from_str(text).unwrap();
        let build = publication_build(Some(&publication)).unwrap().to_owned();
        assert!(!update_available(Some(&build), Some(&publication)));
        assert!(update_available(Some("newer"), Some(&publication)));
        assert!(!update_available(None, Some(&publication)));
        assert!(!update_available(Some("newer"), None));
        publication.diagnostics = None;
        assert!(!update_available(Some("newer"), Some(&publication)));
    }

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
    fn the_newest_kernel_is_chosen_by_its_numbers() {
        let releases = [
            "6.12.0-55.el10.x86_64",
            "6.12.0-124.el10.x86_64",
            "6.9.0-1.el10.x86_64",
            "extramodules",
        ];
        assert_eq!(
            newest(releases.into_iter()).as_deref(),
            Some("6.12.0-124.el10.x86_64")
        );
        assert_eq!(newest(std::iter::empty()), None);
    }

    #[test]
    fn a_disk_another_process_holds_is_told_apart() {
        assert!(is_lock_error(
            "virt-customize: error: libguestfs error: could not create appliance through libvirt. ... Failed to get \"write\" lockIs another process using the image"
        ));
        assert!(!is_lock_error(
            "virt-customize: error: no operating systems found"
        ));
    }

    #[test]
    fn a_file_chunk_fits_in_one_argument() {
        // Base64 and the guest-file-write JSON around it, under the 128 KiB
        // a single argument may have.
        let encoded = crate::tui::term::base64(&vec![0xff; AGENT_CHUNK]);
        let command = serde_json::json!({
            "execute": "guest-file-write",
            "arguments": {"handle": 1_000_000, "buf-b64": encoded}
        })
        .to_string();
        assert!(command.len() < 128 * 1024, "{}", command.len());
    }

    #[test]
    fn base64_from_the_agent_decodes() {
        for text in ["", "f", "fo", "foo", "rampire-guest 0.1.0\n"] {
            let encoded = crate::tui::term::base64(text.as_bytes());
            assert_eq!(base64_decode(&encoded), text.as_bytes(), "{encoded}");
        }
    }

    #[test]
    fn a_read_only_root_takes_no_guest_service() {
        let haos = "<operatingsystems><operatingsystem><root>/dev/sda8</root>\
            <name>linux</name><product_name>Home Assistant OS 18.4</product_name>\
            <package_format>unknown</package_format>\
            <filesystems><filesystem dev=\"/dev/sda1\"><type>vfat</type></filesystem>\
            <filesystem dev=\"/dev/sda8\"><type>squashfs</type></filesystem></filesystems>\
            </operatingsystem></operatingsystems>";
        let system = DiskSystem::from_inspector(haos);
        assert_eq!(system.product.as_deref(), Some("Home Assistant OS 18.4"));
        assert_eq!(system.package_format, None);
        assert_eq!(system.root_filesystem.as_deref(), Some("squashfs"));
        assert!(!system.takes_guest());
        // Known by name when the file systems aren't listed.
        let named = DiskSystem {
            root_filesystem: None,
            ..system.clone()
        };
        assert!(!named.takes_guest());
        // Without dnf or apt, on a writable root: the static binary.
        let arch = DiskSystem {
            product: Some("Arch Linux".to_owned()),
            package_format: Some("pacman".to_owned()),
            root_filesystem: Some("ext4".to_owned()),
        };
        assert!(arch.takes_guest());
        let debian = "<operatingsystem><root>/dev/vda1</root><distro>debian</distro>\
            <product_name>Debian GNU/Linux 13</product_name>\
            <package_format>deb</package_format><package_management>apt</package_management>\
            <filesystems><filesystem dev=\"/dev/vda1\"><type>ext4</type></filesystem></filesystems>";
        let debian = DiskSystem::from_inspector(debian);
        assert_eq!(debian.root_filesystem.as_deref(), Some("ext4"));
        assert!(debian.takes_guest());
        assert_eq!(DiskSystem::from_inspector("").product, None);
    }

    #[test]
    fn the_probe_picks_how_the_guest_service_installs() {
        assert_eq!(
            read_probe("web", "0\nlocal\n6.12.0\n").unwrap(),
            ("0".to_owned(), Kind::Local, "6.12.0".to_owned())
        );
        assert_eq!(read_probe("web", "1000\ndeb\n6.1\n").unwrap().1, Kind::Deb);
        assert_eq!(read_probe("web", "0\nrpm\n6.12\n").unwrap().1, Kind::Rpm);
        let error = read_probe("web", "0\nnone Alpine Linux v3.22\n6.12\n").unwrap_err();
        assert!(error.contains("runs Alpine Linux v3.22"), "{error}");
        assert!(read_probe("web", "0\n").is_err());
    }

    #[test]
    fn the_install_scripts_name_the_local_paths() {
        for script in [local_install_script(), first_boot_script()] {
            assert!(script.contains(LOCAL_BINARY), "{script}");
            assert!(script.contains(LOCAL_UNIT), "{script}");
            assert!(
                !script.contains("BINARY") && !script.contains(" UNIT"),
                "{script}"
            );
        }
        assert!(local_install_script().contains("-lt 253"));
    }

    #[test]
    fn the_scripts_run_in_the_guest_are_valid_shell() {
        for (name, script) in [
            ("probe", PROBE.to_owned()),
            ("local install", local_install_script()),
            ("first boot", first_boot_script()),
        ] {
            let checked = std::process::Command::new("sh")
                .args(["-n", "-c", &script])
                .output()
                .unwrap();
            assert!(
                checked.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&checked.stderr)
            );
        }
        // The reboot for pressure stall information comes before the
        // service is started, so no request of its is cut off.
        let first_boot = first_boot_script();
        assert!(
            first_boot.find("systemctl reboot")
                < first_boot.find("systemctl restart rampire-guest"),
            "{first_boot}"
        );
    }

    #[test]
    fn the_guest_services_own_error_is_read_out_of_its_log() {
        // torrent101 on 2026-11-16, as the wait for it showed it.
        let log = "activating\n\
            host via control channel org.rampire.control.0\n\
            settings from host: vm=torrent101 min_ram=536870912 max_ram=3221225472\n\
            poll skipped; reclaim history reset: guest memory request failed: target geometry is invalid: configured RAM limits cannot be met by this VM's live memory geometry\n\
            rampire-guest failed: 5 consecutive polls failed: guest memory request failed: target geometry is invalid: configured RAM limits cannot be met by this VM's live memory geometry\n\
            rampire-guest.service: Main process exited, code=exited, status=1/FAILURE\n\
            rampire-guest.service: Failed with result 'exit-code'.\n";
        let service = guest_service_in(log);
        assert_eq!(service.state, "activating");
        assert_eq!(
            service.error.as_deref(),
            Some("configured RAM limits cannot be met by this VM's live memory geometry")
        );
        assert_eq!(
            service.described(),
            "keeps stopping and being started again"
        );
        let quiet = guest_service_in("active\nsettings from host: vm=web\n");
        assert_eq!((quiet.described(), quiet.error), ("runs", None));
        assert_eq!(
            guest_error("rampire-guest failed: publish: broken pipe").as_deref(),
            Some("publish: broken pipe")
        );
    }
}
