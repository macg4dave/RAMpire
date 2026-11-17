//! One long-lived libvirt connection for the controller's per-poll commands.
//!
//! Every `virsh` process opens its own libvirt connection, and libvirt asks
//! polkit to authorize each one. At a 1 s poll that was several polkit checks
//! a second; polkitd exhausted its file descriptors and then refused every
//! check, which stopped the controller (27 October). This session connects
//! once and serves `dumpxml`, `qemu-agent-command`, `qemu-monitor-command`
//! and live `update-memory-device` over that connection: with several VMs
//! growing at once, a `virsh` process (and polkit check) per resize cost
//! about 230 ms each, one after another. It also serves the reads that
//! check a VM's recorded setup before each resize (`domuuid`,
//! `domxml-to-native`, `version`), byte for byte as `virsh` prints them:
//! run as `virsh`, each was a polkit check, and with polkit out of open
//! files they were refused and latched the VM (16 November). Rare
//! commands (suspend/resume) stay on `virsh`.
//!
//! libvirt is loaded at runtime with `dlopen`: the host has the runtime
//! library but not its development headers or unversioned link symlink.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CStr, CString};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::virsh::VirshError;

/// `VIR_DOMAIN_QEMU_AGENT_COMMAND_DEFAULT`, which virsh also uses.
const AGENT_TIMEOUT_DEFAULT: c_int = -1;
/// `VIR_DOMAIN_AFFECT_LIVE`, as `virsh update-memory-device --live`.
const AFFECT_LIVE: c_uint = 1;

type Ptr = *mut c_void;

type ConnectOpen = unsafe extern "C" fn(*const c_char) -> Ptr;
type HandleCall = unsafe extern "C" fn(Ptr) -> c_int;
type LookupByName = unsafe extern "C" fn(Ptr, *const c_char) -> Ptr;
type GetXmlDesc = unsafe extern "C" fn(Ptr, c_uint) -> *mut c_char;
type GetState = unsafe extern "C" fn(Ptr, *mut c_int, *mut c_int, c_uint) -> c_int;
type ListAllDomains = unsafe extern "C" fn(Ptr, *mut *mut Ptr, c_uint) -> c_int;
type GetName = unsafe extern "C" fn(Ptr) -> *const c_char;
type GetId = unsafe extern "C" fn(Ptr) -> c_uint;
type AgentCommand = unsafe extern "C" fn(Ptr, *const c_char, c_int, c_uint) -> *mut c_char;
type MonitorCommand = unsafe extern "C" fn(Ptr, *const c_char, *mut *mut c_char, c_uint) -> c_int;
type UpdateDeviceFlags = unsafe extern "C" fn(Ptr, *const c_char, c_uint) -> c_int;
type GetUuidString = unsafe extern "C" fn(Ptr, *mut c_char) -> c_int;
type GetLibraryVersion = unsafe extern "C" fn(*mut c_ulong, *const c_char, *mut c_ulong) -> c_int;
type GetType = unsafe extern "C" fn(Ptr) -> *const c_char;
type GetHypervisorVersion = unsafe extern "C" fn(Ptr, *mut c_ulong) -> c_int;
type XmlToNative = unsafe extern "C" fn(Ptr, *const c_char, *const c_char, c_uint) -> *mut c_char;
type LastErrorMessage = unsafe extern "C" fn() -> *const c_char;
type ResetLastError = unsafe extern "C" fn();
type ErrorFunc = unsafe extern "C" fn(Ptr, Ptr);
type SetErrorFunc = unsafe extern "C" fn(Ptr, Option<ErrorFunc>);

/// libvirt's error handler: nothing. By default libvirt prints every error
/// to stderr as well as keeping it for `virGetLastErrorMessage`, and on a
/// terminal the TUI owns, each failed call (an agent that isn't up while a
/// VM restarts) wrote over the screen. Errors are read where they happen.
unsafe extern "C" fn ignore_error(_user_data: Ptr, _error: Ptr) {}

struct Api {
    connect_open: ConnectOpen,
    connect_close: HandleCall,
    connect_is_alive: HandleCall,
    lookup_by_name: LookupByName,
    domain_free: HandleCall,
    get_xml_desc: GetXmlDesc,
    get_state: GetState,
    list_all_domains: ListAllDomains,
    get_name: GetName,
    get_id: GetId,
    agent_command: AgentCommand,
    monitor_command: MonitorCommand,
    update_device_flags: UpdateDeviceFlags,
    get_uuid_string: GetUuidString,
    get_library_version: GetLibraryVersion,
    get_type: GetType,
    get_hypervisor_version: GetHypervisorVersion,
    xml_to_native: XmlToNative,
    last_error_message: LastErrorMessage,
    reset_last_error: ResetLastError,
}

impl Api {
    fn load() -> Result<Self, String> {
        use std::mem::transmute;
        let libvirt = open_library(c"libvirt.so.0")?;
        let qemu = open_library(c"libvirt-qemu.so.0")?;
        // SAFETY: each symbol is resolved from the library that exports it and
        // cast to the signature documented in libvirt's public headers. The
        // libraries are never closed, so the pointers stay valid.
        unsafe {
            let set_error_func =
                transmute::<Ptr, SetErrorFunc>(symbol(libvirt, c"virSetErrorFunc")?);
            set_error_func(std::ptr::null_mut(), Some(ignore_error));
            Ok(Self {
                connect_open: transmute::<Ptr, ConnectOpen>(symbol(libvirt, c"virConnectOpen")?),
                connect_close: transmute::<Ptr, HandleCall>(symbol(libvirt, c"virConnectClose")?),
                connect_is_alive: transmute::<Ptr, HandleCall>(symbol(
                    libvirt,
                    c"virConnectIsAlive",
                )?),
                lookup_by_name: transmute::<Ptr, LookupByName>(symbol(
                    libvirt,
                    c"virDomainLookupByName",
                )?),
                domain_free: transmute::<Ptr, HandleCall>(symbol(libvirt, c"virDomainFree")?),
                get_xml_desc: transmute::<Ptr, GetXmlDesc>(symbol(
                    libvirt,
                    c"virDomainGetXMLDesc",
                )?),
                get_state: transmute::<Ptr, GetState>(symbol(libvirt, c"virDomainGetState")?),
                list_all_domains: transmute::<Ptr, ListAllDomains>(symbol(
                    libvirt,
                    c"virConnectListAllDomains",
                )?),
                get_name: transmute::<Ptr, GetName>(symbol(libvirt, c"virDomainGetName")?),
                get_id: transmute::<Ptr, GetId>(symbol(libvirt, c"virDomainGetID")?),
                agent_command: transmute::<Ptr, AgentCommand>(symbol(
                    qemu,
                    c"virDomainQemuAgentCommand",
                )?),
                monitor_command: transmute::<Ptr, MonitorCommand>(symbol(
                    qemu,
                    c"virDomainQemuMonitorCommand",
                )?),
                update_device_flags: transmute::<Ptr, UpdateDeviceFlags>(symbol(
                    libvirt,
                    c"virDomainUpdateDeviceFlags",
                )?),
                get_uuid_string: transmute::<Ptr, GetUuidString>(symbol(
                    libvirt,
                    c"virDomainGetUUIDString",
                )?),
                get_library_version: transmute::<Ptr, GetLibraryVersion>(symbol(
                    libvirt,
                    c"virGetVersion",
                )?),
                get_type: transmute::<Ptr, GetType>(symbol(libvirt, c"virConnectGetType")?),
                get_hypervisor_version: transmute::<Ptr, GetHypervisorVersion>(symbol(
                    libvirt,
                    c"virConnectGetVersion",
                )?),
                xml_to_native: transmute::<Ptr, XmlToNative>(symbol(
                    libvirt,
                    c"virConnectDomainXMLToNative",
                )?),
                last_error_message: transmute::<Ptr, LastErrorMessage>(symbol(
                    libvirt,
                    c"virGetLastErrorMessage",
                )?),
                reset_last_error: transmute::<Ptr, ResetLastError>(symbol(
                    libvirt,
                    c"virResetLastError",
                )?),
            })
        }
    }

    /// The calling thread's last libvirt error. Must run on the thread that
    /// made the failing call.
    fn last_error(&self) -> String {
        // SAFETY: libvirt returns a thread-local, NUL-terminated string that
        // stays valid until the next libvirt call on this thread.
        let message = unsafe { (self.last_error_message)() };
        if message.is_null() {
            return "unknown libvirt error".to_owned();
        }
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

fn open_library(name: &CStr) -> Result<Ptr, String> {
    // SAFETY: dlopen takes a NUL-terminated name; the handle is never closed.
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(format!("load {}: {}", name.to_string_lossy(), dl_error()));
    }
    Ok(handle)
}

fn symbol(library: Ptr, name: &CStr) -> Result<Ptr, String> {
    // SAFETY: library is a live dlopen handle and name is NUL-terminated.
    let address = unsafe { libc::dlsym(library, name.as_ptr()) };
    if address.is_null() {
        return Err(format!(
            "resolve {}: {}",
            name.to_string_lossy(),
            dl_error()
        ));
    }
    Ok(address)
}

fn dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated message.
    let message = unsafe { libc::dlerror() };
    if message.is_null() {
        "unknown dynamic loader error".to_owned()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Takes ownership of a libvirt-allocated string.
fn take_string(value: *mut c_char) -> String {
    // SAFETY: libvirt returns a malloc'd NUL-terminated string the caller frees.
    let text = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(value.cast()) };
    text
}

enum Request {
    /// The live definition, or the saved one when `inactive`.
    DumpXml {
        vm: String,
        inactive: bool,
    },
    /// The domain's state, as `virsh domstate` names it.
    State {
        vm: String,
    },
    /// The domains' names, as `virsh list --name` prints them, with
    /// `virConnectListAllDomains` flags: 0 for every domain.
    List {
        flags: c_uint,
    },
    Agent {
        vm: String,
        command: String,
    },
    Monitor {
        vm: String,
        command: String,
    },
    /// A live device update; `xml` is the device's complete element.
    UpdateDevice {
        vm: String,
        xml: String,
    },
    /// The domain's UUID, as `virsh domuuid` prints it.
    Uuid {
        vm: String,
    },
    /// The QEMU command line for the domain's live definition, as `virsh
    /// domxml-to-native qemu-argv --domain` prints it.
    Native {
        vm: String,
    },
    /// What `virsh version` prints. `compiled` is the version the `virsh`
    /// binary was built against, which only it knows: `virsh --version`
    /// gives it without a connection.
    Version {
        compiled: String,
    },
}

/// Why a request failed.
#[derive(Debug)]
enum Failure {
    /// The connection couldn't be opened or was lost: libvirt refused it
    /// (polkit) or isn't answering. Nothing reached the VM.
    Unavailable(String),
    /// libvirt answered with an error.
    Error(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Error(message)
    }
}

type Job = (Request, Sender<Result<String, Failure>>);

/// The worker thread owns the connection, so libvirt's thread-local error
/// messages are always read on the thread that failed.
struct Worker {
    api: Api,
    uri: CString,
    connection: Ptr,
    /// The daemon records each connection in its journal; an observer such
    /// as `top` draws a screen, where the line would be noise.
    log_connections: bool,
}

impl Worker {
    fn serve(mut self, jobs: Receiver<Job>) {
        for (request, reply) in jobs {
            let mut result = self.execute(&request);
            if let Err(Failure::Error(message)) = &result {
                // SAFETY: connection is a live handle owned by this worker.
                if !self.connection.is_null()
                    && unsafe { (self.api.connect_is_alive)(self.connection) } != 1
                {
                    self.disconnect();
                    result = Err(Failure::Unavailable(message.clone()));
                }
            }
            if reply.send(result).is_err() {
                // The caller timed out and replaced this worker.
                break;
            }
        }
        self.disconnect();
    }

    fn connect(&mut self) -> Result<Ptr, Failure> {
        if self.connection.is_null() {
            // SAFETY: uri is NUL-terminated; a null result reports an error.
            let connection = unsafe { (self.api.connect_open)(self.uri.as_ptr()) };
            if connection.is_null() {
                return Err(Failure::Unavailable(format!(
                    "connect to libvirt: {}",
                    self.api.last_error()
                )));
            }
            if self.log_connections {
                eprintln!("rampire: event=libvirt_connected");
            }
            self.connection = connection;
        }
        Ok(self.connection)
    }

    fn disconnect(&mut self) {
        if !self.connection.is_null() {
            // SAFETY: connection is a live handle owned by this worker.
            unsafe { (self.api.connect_close)(self.connection) };
            self.connection = std::ptr::null_mut();
        }
    }

    fn execute(&mut self, request: &Request) -> Result<String, Failure> {
        // SAFETY: resets this thread's error so a stale one is never reported.
        unsafe { (self.api.reset_last_error)() };
        let connection = self.connect()?;
        let vm = match request {
            // Not about one domain: answered without looking one up.
            Request::List { flags } => return Ok(self.list(connection, *flags)?),
            Request::Version { compiled } => return Ok(self.version(connection, compiled)?),
            Request::DumpXml { vm, .. }
            | Request::State { vm }
            | Request::Agent { vm, .. }
            | Request::Monitor { vm, .. }
            | Request::UpdateDevice { vm, .. }
            | Request::Uuid { vm }
            | Request::Native { vm } => vm,
        };
        let name = CString::new(vm.as_str()).map_err(|_| "VM name contains NUL".to_owned())?;
        // SAFETY: connection is live and name is NUL-terminated.
        let domain = unsafe { (self.api.lookup_by_name)(connection, name.as_ptr()) };
        if domain.is_null() {
            return Err(Failure::Error(format!(
                "failed to get domain '{vm}': {}",
                self.api.last_error()
            )));
        }
        let result = self.on_domain(connection, domain, request);
        // SAFETY: domain was returned by lookup and is freed exactly once.
        unsafe { (self.api.domain_free)(domain) };
        Ok(result?)
    }

    /// `request`, for the live `domain` it names.
    fn on_domain(&self, connection: Ptr, domain: Ptr, request: &Request) -> Result<String, String> {
        match request {
            Request::List { .. } | Request::Version { .. } => {
                Err("answered without a domain".to_owned())
            }
            Request::Uuid { .. } => {
                // VIR_UUID_STRING_BUFLEN: 36 characters and the NUL.
                let mut uuid = [0 as c_char; 37];
                // SAFETY: domain is live and uuid has the room libvirt writes.
                match unsafe { (self.api.get_uuid_string)(domain, uuid.as_mut_ptr()) } {
                    // SAFETY: on success libvirt wrote a NUL-terminated string.
                    0 => Ok(format!(
                        "{}\n\n",
                        unsafe { CStr::from_ptr(uuid.as_ptr()) }.to_string_lossy()
                    )),
                    _ => Err(self.api.last_error()),
                }
            }
            Request::Native { .. } => {
                // virsh converts the domain's live definition (flags 0) and
                // prints the result as it is.
                // SAFETY: domain is live; flags 0 matches `virsh dumpxml`.
                let xml = self.owned(unsafe { (self.api.get_xml_desc)(domain, 0) })?;
                let xml = CString::new(xml).map_err(|_| "domain XML contains NUL".to_owned())?;
                // SAFETY: connection is live; both strings are NUL-terminated
                // and libvirt returns a malloc'd string or null.
                self.owned(unsafe {
                    (self.api.xml_to_native)(connection, c"qemu-argv".as_ptr(), xml.as_ptr(), 0)
                })
            }
            // virsh prints the XML and a newline after it: the same bytes,
            // since a setup's record is a hash of them.
            // SAFETY: domain is live; the flags are 0, or
            // VIR_DOMAIN_XML_INACTIVE (2) as `virsh dumpxml --inactive` uses.
            Request::DumpXml { inactive, .. } => self
                .owned(unsafe {
                    (self.api.get_xml_desc)(domain, if *inactive { XML_INACTIVE } else { 0 })
                })
                .map(|xml| xml + "\n"),
            Request::State { .. } => {
                let mut state: c_int = 0;
                let mut reason: c_int = 0;
                // SAFETY: domain is live and both pointers are to locals.
                match unsafe { (self.api.get_state)(domain, &mut state, &mut reason, 0) } {
                    0 => Ok(format!("{}\n\n", state_name(state))),
                    _ => Err(self.api.last_error()),
                }
            }
            Request::Agent { command, .. } => match CString::new(command.as_str()) {
                // SAFETY: domain is live and command is NUL-terminated.
                Ok(command) => self.owned(unsafe {
                    (self.api.agent_command)(domain, command.as_ptr(), AGENT_TIMEOUT_DEFAULT, 0)
                }),
                Err(_) => Err("agent command contains NUL".to_owned()),
            },
            Request::Monitor { command, .. } => match CString::new(command.as_str()) {
                Ok(command) => {
                    let mut reply: *mut c_char = std::ptr::null_mut();
                    // SAFETY: domain is live, command is NUL-terminated and
                    // reply receives a malloc'd string on success; flags 0 is
                    // VIR_DOMAIN_QEMU_MONITOR_COMMAND_DEFAULT, as virsh uses.
                    let status = unsafe {
                        (self.api.monitor_command)(domain, command.as_ptr(), &mut reply, 0)
                    };
                    if status == 0 {
                        self.owned(reply)
                    } else {
                        Err(self.api.last_error())
                    }
                }
                Err(_) => Err("monitor command contains NUL".to_owned()),
            },
            Request::UpdateDevice { xml, .. } => match CString::new(xml.as_str()) {
                // SAFETY: domain is live and xml is NUL-terminated.
                Ok(xml) => match unsafe {
                    (self.api.update_device_flags)(domain, xml.as_ptr(), AFFECT_LIVE)
                } {
                    0 => Ok(String::new()),
                    _ => Err(self.api.last_error()),
                },
                Err(_) => Err("device XML contains NUL".to_owned()),
            },
        }
    }

    /// What `virsh version` prints for this connection, given the version
    /// `virsh` was built against.
    fn version(&self, connection: Ptr, compiled: &str) -> Result<String, String> {
        // SAFETY: connection is live; libvirt returns a static string or null.
        let kind = unsafe { (self.api.get_type)(connection) };
        if kind.is_null() {
            return Err(self.api.last_error());
        }
        let (mut library, mut api, mut hypervisor): (c_ulong, c_ulong, c_ulong) = (0, 0, 0);
        // SAFETY: the pointers are to locals; kind is NUL-terminated.
        if unsafe { (self.api.get_library_version)(&mut library, kind, &mut api) } != 0 {
            return Err(self.api.last_error());
        }
        // SAFETY: connection is live and the pointer is to a local.
        if unsafe { (self.api.get_hypervisor_version)(connection, &mut hypervisor) } != 0 {
            return Err(self.api.last_error());
        }
        // SAFETY: kind was checked non-null; libvirt owns it.
        let kind = unsafe { CStr::from_ptr(kind) }.to_string_lossy();
        Ok(version_text(compiled, &kind, library, api, hypervisor))
    }

    /// The domains' names in `virsh list`'s order: running ones by their
    /// ID, then the rest by name; one a line, and a blank line after, as
    /// virsh prints them.
    fn list(&self, connection: Ptr, flags: c_uint) -> Result<String, String> {
        let mut domains: *mut Ptr = std::ptr::null_mut();
        // SAFETY: connection is live; libvirt allocates the array and a
        // reference to each domain, all freed below.
        let count = unsafe { (self.api.list_all_domains)(connection, &mut domains, flags) };
        if count < 0 {
            return Err(self.api.last_error());
        }
        let mut named = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            // SAFETY: index is within the `count` domains libvirt returned.
            let domain = unsafe { *domains.add(index) };
            // SAFETY: domain is live; its name is owned by it, copied here
            // before it is freed.
            let name = unsafe { (self.api.get_name)(domain) };
            if !name.is_null() {
                let id = unsafe { (self.api.get_id)(domain) };
                let name = unsafe { CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned();
                named.push((id, name));
            }
            // SAFETY: each domain reference is freed exactly once.
            unsafe { (self.api.domain_free)(domain) };
        }
        if !domains.is_null() {
            // SAFETY: the array was allocated by libvirt with malloc.
            unsafe { libc::free(domains.cast()) };
        }
        // An inactive domain's ID is (unsigned) -1, so it sorts last.
        named.sort();
        let mut text = String::new();
        for (_, name) in named {
            text.push_str(&name);
            text.push('\n');
        }
        text.push('\n');
        Ok(text)
    }

    /// A libvirt-allocated result, or this thread's error when it is null.
    fn owned(&self, output: *mut c_char) -> Result<String, String> {
        if output.is_null() {
            Err(self.api.last_error())
        } else {
            Ok(take_string(output))
        }
    }
}

/// `virsh version`'s text, from libvirt's numbers: major * 1,000,000 +
/// minor * 1,000 + release. A hypervisor version of 0 is one libvirt
/// couldn't learn, which virsh says in place of it.
fn version_text(
    compiled: &str,
    kind: &str,
    library: c_ulong,
    api: c_ulong,
    hypervisor: c_ulong,
) -> String {
    let dotted = |version: c_ulong| {
        format!(
            "{}.{}.{}",
            version / 1_000_000,
            version / 1_000 % 1_000,
            version % 1_000
        )
    };
    let running = if hypervisor == 0 {
        format!("Cannot extract running {kind} hypervisor version")
    } else {
        format!("Running hypervisor: {kind} {}", dotted(hypervisor))
    };
    format!(
        "Compiled against library: libvirt {compiled}\nUsing library: libvirt {}\nUsing API: {kind} {}\n{running}\n\n",
        dotted(library),
        dotted(api)
    )
}

/// `virConnectListAllDomains`'s VIR_CONNECT_LIST_DOMAINS_SHUTOFF.
const LIST_SHUTOFF: c_uint = 1 << 6;

/// `virDomainGetXMLDesc`'s VIR_DOMAIN_XML_INACTIVE: the saved definition.
const XML_INACTIVE: c_uint = 2;

/// A `virDomainState` as `virsh domstate` prints it.
fn state_name(state: c_int) -> &'static str {
    match state {
        1 => "running",
        2 => "idle",
        3 => "paused",
        4 => "in shutdown",
        5 => "shut off",
        6 => "crashed",
        7 => "pmsuspended",
        _ => "no state",
    }
}

/// The `<memory model='virtio-mem'>` element with `alias` from live domain
/// XML, with its `<requested>` set to `requested_kib`: the device XML
/// `virsh update-memory-device` builds. `None` unless exactly one such
/// device and one KiB `<requested>` in it are found.
fn virtio_mem_update_xml(domain_xml: &str, alias: &str, requested_kib: u64) -> Option<String> {
    const START: &str = "<memory model='virtio-mem'";
    const END: &str = "</memory>";
    const OPEN: &str = "<requested unit='KiB'>";
    const CLOSE: &str = "</requested>";
    let alias_tag = format!("<alias name='{alias}'/>");
    let mut found = domain_xml.match_indices(START).filter_map(|(start, _)| {
        let end = start + domain_xml[start..].find(END)? + END.len();
        let element = &domain_xml[start..end];
        element.contains(&alias_tag).then_some(element)
    });
    let element = found.next()?;
    if found.next().is_some() || element.matches(OPEN).count() != 1 {
        return None;
    }
    let value_start = element.find(OPEN)? + OPEN.len();
    let value_end = value_start + element[value_start..].find(CLOSE)?;
    element[value_start..value_end].parse::<u64>().ok()?;
    Some(format!(
        "{}{requested_kib}{}",
        &element[..value_start],
        &element[value_end..]
    ))
}

/// A shared, lazily connected libvirt session. A call that exceeds the
/// timeout abandons its worker (and connection) and the next call starts a
/// fresh one, as killing a timed-out `virsh` did.
pub struct LibvirtSession {
    uri: String,
    timeout: Duration,
    jobs: Mutex<Option<Sender<Job>>>,
    log_connections: bool,
}

impl std::fmt::Debug for LibvirtSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LibvirtSession")
            .field("uri", &self.uri)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl LibvirtSession {
    pub fn new(uri: impl Into<String>, timeout: Duration) -> Self {
        Self {
            uri: uri.into(),
            timeout,
            jobs: Mutex::new(None),
            log_connections: false,
        }
    }

    /// Like `new`, but each new connection is logged: for the daemon.
    pub fn logged(uri: impl Into<String>, timeout: Duration) -> Self {
        Self {
            log_connections: true,
            ..Self::new(uri, timeout)
        }
    }

    /// Serves the `virsh` commands this session supports, or `None` to let
    /// the caller run `virsh`.
    pub fn run(&self, arguments: &[String]) -> Option<Result<String, VirshError>> {
        let request = match arguments {
            [operation, vm] if operation == "dumpxml" => Request::DumpXml {
                vm: vm.clone(),
                inactive: false,
            },
            // Both orders virsh takes the flag in.
            [operation, flag, vm] | [operation, vm, flag]
                if operation == "dumpxml" && flag == "--inactive" =>
            {
                Request::DumpXml {
                    vm: vm.clone(),
                    inactive: true,
                }
            }
            [operation, vm] if operation == "domstate" => Request::State { vm: vm.clone() },
            [operation, vm] if operation == "domuuid" => Request::Uuid { vm: vm.clone() },
            [operation, format, flag, vm]
                if operation == "domxml-to-native"
                    && format == "qemu-argv"
                    && flag == "--domain" =>
            {
                Request::Native { vm: vm.clone() }
            }
            [operation, all, name] if operation == "list" && all == "--all" && name == "--name" => {
                Request::List { flags: 0 }
            }
            [operation, state, name]
                if operation == "list" && state == "--state-shutoff" && name == "--name" =>
            {
                Request::List {
                    flags: LIST_SHUTOFF,
                }
            }
            [operation, vm, command] if operation == "qemu-agent-command" => Request::Agent {
                vm: vm.clone(),
                command: command.clone(),
            },
            [operation, vm, command] if operation == "qemu-monitor-command" => Request::Monitor {
                vm: vm.clone(),
                command: command.clone(),
            },
            [operation, vm, alias_flag, alias, size_flag, kib, live]
                if operation == "update-memory-device"
                    && alias_flag == "--alias"
                    && size_flag == "--requested-size"
                    && live == "--live" =>
            {
                // Build the device XML from live state as virsh does. Anything
                // short of a complete element leaves the command to virsh, so
                // nothing is attempted here that virsh would not attempt.
                let requested_kib = kib.parse().ok()?;
                let domain_xml = self
                    .call(Request::DumpXml {
                        vm: vm.clone(),
                        inactive: false,
                    })
                    .ok()?;
                let xml = virtio_mem_update_xml(&domain_xml, alias, requested_kib)?;
                Request::UpdateDevice {
                    vm: vm.clone(),
                    xml,
                }
            }
            _ => return None,
        };
        Some(self.call(request))
    }

    /// What `virsh version` prints, over this connection. `compiled` is
    /// what `virsh --version` printed: the one number only `virsh` knows.
    pub fn version(&self, compiled: &str) -> Result<String, VirshError> {
        self.call(Request::Version {
            compiled: compiled.trim().to_owned(),
        })
    }

    fn call(&self, request: Request) -> Result<String, VirshError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs.is_none() {
            *jobs = Some(self.spawn().map_err(|error| VirshError::Failed {
                status: "libvirt session unavailable".to_owned(),
                stderr: error,
            })?);
        }
        let (reply, response) = mpsc::channel();
        let sent = jobs
            .as_ref()
            .is_some_and(|sender| sender.send((request, reply)).is_ok());
        let result = if sent {
            response.recv_timeout(self.timeout)
        } else {
            Err(RecvTimeoutError::Disconnected)
        };
        match result {
            Ok(result) => result.map_err(|failure| match failure {
                Failure::Unavailable(message) => VirshError::Unavailable(message),
                Failure::Error(message) => VirshError::Failed {
                    status: "libvirt error".to_owned(),
                    stderr: format!("error: {message}"),
                },
            }),
            Err(error) => {
                // Abandon the worker; a stuck call cannot block later ones.
                *jobs = None;
                Err(match error {
                    RecvTimeoutError::Timeout => VirshError::Timeout(self.timeout),
                    RecvTimeoutError::Disconnected => {
                        VirshError::Unavailable("the libvirt session ended".to_owned())
                    }
                })
            }
        }
    }

    fn spawn(&self) -> Result<Sender<Job>, String> {
        let api = Api::load()?;
        let uri = CString::new(self.uri.as_str()).map_err(|_| "URI contains NUL".to_owned())?;
        let (sender, receiver) = mpsc::channel();
        let log_connections = self.log_connections;
        thread::Builder::new()
            .name("libvirt-session".to_owned())
            .spawn(move || {
                Worker {
                    api,
                    uri,
                    connection: std::ptr::null_mut(),
                    log_connections,
                }
                .serve(receiver)
            })
            .map_err(|error| format!("start libvirt session thread: {error}"))?;
        Ok(sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn only_per_poll_commands_use_the_session() {
        let session = LibvirtSession::new("test:///default", Duration::from_secs(5));
        for other in [
            arguments(&["update-memory-device", "guest", "--alias", "a"]),
            arguments(&[
                "update-memory-device",
                "guest",
                "--alias",
                "a",
                "--requested-size",
                "1024",
                "--config",
            ]),
            arguments(&["suspend", "guest"]),
            arguments(&["resume", "guest"]),
            // Served by `Virsh`, which knows the binary's own version.
            arguments(&["version"]),
            arguments(&["domxml-to-native", "native-format", "--domain", "guest"]),
            arguments(&["dumpxml", "guest", "--security-info"]),
            arguments(&["domstate", "guest", "--reason"]),
        ] {
            assert!(session.run(&other).is_none(), "{other:?}");
        }
    }

    #[test]
    fn the_reads_the_operator_tools_make_are_served() {
        // Served: answered by the session, here an error from a host
        // without libvirt or without such a VM, never None.
        let session = LibvirtSession::new("test:///default", Duration::from_secs(5));
        for served in [
            arguments(&["dumpxml", "guest", "--inactive"]),
            arguments(&["dumpxml", "--inactive", "guest"]),
            arguments(&["domstate", "guest"]),
            arguments(&["list", "--all", "--name"]),
            arguments(&["list", "--state-shutoff", "--name"]),
            // The reads that check a VM's recorded setup before a resize.
            arguments(&["domuuid", "guest"]),
            arguments(&["domxml-to-native", "qemu-argv", "--domain", "guest"]),
        ] {
            assert!(session.run(&served).is_some(), "{served:?}");
        }
        // Where libvirt is installed, they answer as virsh does, here on
        // libvirt's test driver and its running domain "test".
        // Byte for byte: a setup's record is a hash of what was read.
        let virsh = |args: &[&str]| {
            std::process::Command::new("virsh")
                .args(["-c", "test:///default"])
                .args(args)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let installed = [
            "/usr/lib64/libvirt.so.0",
            "/usr/lib/x86_64-linux-gnu/libvirt.so.0",
        ]
        .iter()
        .any(|path| std::path::Path::new(path).exists());
        let answered = session.run(&arguments(&["domstate", "test"]));
        assert!(
            !installed || matches!(answered, Some(Ok(_))),
            "libvirt is installed but the session didn't answer: {answered:?}"
        );
        if let Some(Ok(state)) = answered {
            for list in [
                ["list", "--all", "--name"],
                ["list", "--state-shutoff", "--name"],
            ] {
                let served = session.run(&arguments(&list)).unwrap().unwrap();
                assert_eq!(Some(served), virsh(&list), "{list:?}");
            }
            assert_eq!(Some(state), virsh(&["domstate", "test"]));
            let inactive = session
                .run(&arguments(&["dumpxml", "--inactive", "test"]))
                .unwrap()
                .unwrap();
            assert_eq!(Some(inactive), virsh(&["dumpxml", "--inactive", "test"]));
            let live = session
                .run(&arguments(&["dumpxml", "test"]))
                .unwrap()
                .unwrap();
            assert_eq!(Some(live), virsh(&["dumpxml", "test"]));
            let uuid = session
                .run(&arguments(&["domuuid", "test"]))
                .unwrap()
                .unwrap();
            assert_eq!(Some(uuid), virsh(&["domuuid", "test"]));
            let compiled = virsh(&["--version"]).expect("virsh --version");
            assert_eq!(
                Some(session.version(&compiled).unwrap()),
                virsh(&["version"])
            );
        }
        assert_eq!(state_name(1), "running");
        assert_eq!(state_name(5), "shut off");
        assert_eq!(state_name(99), "no state");
    }

    /// On a real QEMU VM, which the test driver can't stand in for, as root
    /// (docs/testing.md has the command). Reads only.
    #[test]
    #[ignore = "reads a running QEMU VM named by RAMPIRE_SESSION_VM, as root"]
    fn reads_a_real_vm_as_virsh_does() {
        use crate::attestation::VirshCompatibilityEvidenceSource;
        use crate::virsh::{Virsh, VirshCommand};
        let vm = std::env::var("RAMPIRE_SESSION_VM").expect("RAMPIRE_SESSION_VM");
        let alias = std::env::var("RAMPIRE_SESSION_ALIAS").expect("RAMPIRE_SESSION_ALIAS");
        let timeout = Duration::from_secs(30);
        let session = Virsh::persistent_with_connection("virsh", timeout, "qemu:///system");
        let process = Virsh::with_connection("virsh", timeout, "qemu:///system");
        // Byte for byte where the bytes are stable.
        for args in [
            vec!["dumpxml", vm.as_str()],
            vec!["domuuid", vm.as_str()],
            vec!["version"],
        ] {
            let args = arguments(&args);
            assert_eq!(
                session.run(&args).unwrap(),
                process.run(&args).unwrap(),
                "{args:?}"
            );
        }
        // And the VM's RAM as its recorded setup reads it.
        let evidence = |command: Virsh| {
            VirshCompatibilityEvidenceSource::new(command, vm.as_str(), alias.as_str())
                .collect()
                .expect("evidence")
        };
        assert_eq!(evidence(session), evidence(process));
    }

    #[test]
    fn version_text_is_what_virsh_prints() {
        assert_eq!(
            version_text("11.10.0", "QEMU", 11_010_000, 11_010_000, 10_001_000),
            "Compiled against library: libvirt 11.10.0\nUsing library: libvirt 11.10.0\nUsing API: QEMU 11.10.0\nRunning hypervisor: QEMU 10.1.0\n\n"
        );
        assert!(version_text("11.10.0", "QEMU", 11_010_000, 11_010_000, 0)
            .contains("\nCannot extract running QEMU hypervisor version\n"));
    }

    #[test]
    fn a_connection_libvirt_refuses_is_unavailable() {
        // Nothing listens at this socket: the connection itself fails, as
        // when polkit refuses it, and nothing reached a VM.
        let session = LibvirtSession::new(
            "qemu+unix:///system?socket=/nonexistent/rampire-test-sock",
            Duration::from_secs(5),
        );
        let installed = [
            "/usr/lib64/libvirt.so.0",
            "/usr/lib/x86_64-linux-gnu/libvirt.so.0",
        ]
        .iter()
        .any(|path| std::path::Path::new(path).exists());
        match session.run(&arguments(&["dumpxml", "guest"])) {
            // A host without the library (the build container): the
            // session can't start, and there is nothing to show.
            Some(Err(VirshError::Failed { status, .. }))
                if !installed && status == "libvirt session unavailable" => {}
            Some(Err(error)) => assert!(error.is_unavailable(), "{error}"),
            other => assert!(!installed, "{other:?}"),
        }
    }

    const DEVICE: &str = "    <memory model='virtio-mem'>
      <target>
        <size unit='KiB'>15728640</size>
        <node>0</node>
        <block unit='KiB'>2048</block>
        <requested unit='KiB'>284672</requested>
        <current unit='KiB'>284672</current>
        <address base='0x100000000'/>
      </target>
      <alias name='ua-virtiomem0'/>
      <address type='pci' domain='0x0000' bus='0x00' slot='0x07' function='0x0'/>
    </memory>
";

    fn live_xml(devices: &str) -> String {
        format!(
            "<domain type='kvm' id='7'>
  <devices>
    <memory model='dimm'>
      <target><size unit='KiB'>1048576</size></target>
      <alias name='dimm0'/>
    </memory>
{devices}  </devices>
</domain>"
        )
    }

    #[test]
    fn a_resize_sends_the_devices_live_element_with_only_requested_changed() {
        let xml =
            virtio_mem_update_xml(&live_xml(DEVICE), "ua-virtiomem0", 1_048_576).expect("element");
        assert!(xml.starts_with("<memory model='virtio-mem'>"), "{xml}");
        assert!(xml.ends_with("</memory>"), "{xml}");
        assert!(
            xml.contains("<requested unit='KiB'>1048576</requested>"),
            "{xml}"
        );
        assert!(
            xml.contains("<current unit='KiB'>284672</current>"),
            "{xml}"
        );
        assert!(!xml.contains("dimm"), "{xml}");
        // No such device, two of them, or no KiB requested: left to virsh.
        assert_eq!(virtio_mem_update_xml(&live_xml(DEVICE), "other", 1), None);
        let twice = live_xml(&format!("{DEVICE}{DEVICE}"));
        assert_eq!(virtio_mem_update_xml(&twice, "ua-virtiomem0", 1), None);
        let bytes = DEVICE.replace(
            "<requested unit='KiB'>284672",
            "<requested unit='bytes'>291504128",
        );
        assert_eq!(
            virtio_mem_update_xml(&live_xml(&bytes), "ua-virtiomem0", 1),
            None
        );
    }

    /// libvirt's built-in test driver: no daemon, polkit or VM required.
    #[test]
    fn serves_dumpxml_and_reports_errors_from_one_connection() {
        if Api::load().is_err() {
            eprintln!("libvirt runtime library unavailable; skipped");
            return;
        }
        let session = LibvirtSession::new("test:///default", Duration::from_secs(5));
        for _ in 0..3 {
            let xml = session
                .run(&arguments(&["dumpxml", "test"]))
                .expect("served")
                .expect("test domain XML");
            assert!(xml.contains("<name>test</name>"), "{xml}");
        }
        let error = session
            .run(&arguments(&["dumpxml", "missing"]))
            .expect("served")
            .expect_err("unknown domain");
        assert!(error.to_string().contains("missing"), "{error}");
    }

    /// Live check against a running VM with a guest agent:
    /// `RAMPIRE_LIVE_VM=<domain> cargo test -p rampire-host live_ -- --ignored`
    #[test]
    #[ignore]
    fn live_session_matches_virsh_on_a_running_vm() {
        let vm = std::env::var("RAMPIRE_LIVE_VM").expect("RAMPIRE_LIVE_VM");
        let session = LibvirtSession::new("qemu:///system", Duration::from_secs(10));
        let xml = session
            .run(&arguments(&["dumpxml", &vm]))
            .expect("served")
            .expect("domain XML");
        let virsh = std::process::Command::new("virsh")
            .args(["-c", "qemu:///system", "dumpxml", &vm])
            .output()
            .expect("virsh");
        assert_eq!(
            xml.trim_end(),
            String::from_utf8_lossy(&virsh.stdout).trim_end()
        );
        for _ in 0..3 {
            let pong = session
                .run(&arguments(&[
                    "qemu-agent-command",
                    &vm,
                    r#"{"execute":"guest-ping"}"#,
                ]))
                .expect("served")
                .expect("guest-ping");
            assert!(pong.contains("\"return\""), "{pong}");
        }
    }
}
