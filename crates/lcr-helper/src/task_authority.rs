use crate::{
    broker_install::{protected_broker_path_for_version, ProtectedBrokerPath},
    distribution::CURRENT_VERSION,
};
use semver::Version;
use std::{cmp::Ordering, path::Path};
pub const BROKER_TASK_NAME: &str = "Linh Lan Bang - LCR Input Broker";
pub const BROKER_TASK_DESCRIPTION: &str =
    "Starts LCR's elevated physical-input observer on demand while LCR is running. \
It does not access microphone audio, Discord credentials, or network services.";

pub const BROKER_EXECUTABLE_NAME: &str = "LCRInputBroker.exe";

pub const BROKER_TASK_ARGUMENTS: &str = "$(Arg0) $(Arg1) $(Arg2) $(Arg3) $(Arg4) $(Arg5)";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskContractError {
    InvalidUserSid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokerTaskContract {
    user_sid: String,
    broker_path: ProtectedBrokerPath,
}

impl BrokerTaskContract {
    /// Binds the fixed Task Scheduler authority to an already-proven protected
    /// Broker-path capability.
    ///
    /// The Task Scheduler layer deliberately cannot accept arbitrary filesystem
    /// paths. Only `broker_install` can mint this production capability.
    pub fn new(
        user_sid: &str,
        broker_path: &ProtectedBrokerPath,
    ) -> Result<Self, TaskContractError> {
        if !valid_sid_text(user_sid) {
            return Err(TaskContractError::InvalidUserSid);
        }

        Ok(Self {
            user_sid: user_sid.to_owned(),
            broker_path: broker_path.clone(),
        })
    }

    pub fn user_sid(&self) -> &str {
        &self.user_sid
    }

    pub fn broker_path(&self) -> &Path {
        self.broker_path.as_path()
    }

    /// Returns the already-minted protected Broker-path capability retained by
    /// this exact Task contract.
    ///
    /// This does not construct authority from an arbitrary path; it only exposes
    /// the capability originally supplied to `BrokerTaskContract::new`.
    pub(crate) fn protected_broker_path(&self) -> &ProtectedBrokerPath {
        &self.broker_path
    }

    /// Protected task DACL.
    ///
    /// SYSTEM and local Administrators retain full control. The exact owning
    /// user receives only generic read + generic execute, which is sufficient
    /// to inspect and demand-run the task but deliberately omits write/delete.
    pub fn security_descriptor(&self) -> String {
        format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGX;;;{})", self.user_sid)
    }

    /// Stable Task Scheduler 2.0 XML contract.
    ///
    /// There is deliberately no Triggers element: the broker may only start
    /// from an explicit demand-run while LCR itself is running.
    pub fn xml(&self) -> String {
        let user_sid = xml_escape(&self.user_sid);
        let broker_path = xml_escape(
            self.broker_path
                .as_path()
                .to_str()
                .expect("BrokerTaskContract guarantees a Unicode path"),
        );
        let description = xml_escape(BROKER_TASK_DESCRIPTION);

        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n",
                "<Task version=\"1.2\" ",
                "xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n",
                "  <RegistrationInfo>\n",
                "    <Author>Linh Lan Bang</Author>\n",
                "    <Description>{description}</Description>\n",
                "  </RegistrationInfo>\n",
                "  <Principals>\n",
                "    <Principal id=\"LCRUser\">\n",
                "      <UserId>{user_sid}</UserId>\n",
                "      <LogonType>InteractiveToken</LogonType>\n",
                "      <RunLevel>HighestAvailable</RunLevel>\n",
                "    </Principal>\n",
                "  </Principals>\n",
                "  <Settings>\n",
                "    <MultipleInstancesPolicy>StopExisting</MultipleInstancesPolicy>\n",
                "    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n",
                "    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n",
                "    <AllowHardTerminate>true</AllowHardTerminate>\n",
                "    <StartWhenAvailable>false</StartWhenAvailable>\n",
                "    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n",
                "    <AllowStartOnDemand>true</AllowStartOnDemand>\n",
                "    <Enabled>true</Enabled>\n",
                "    <Hidden>false</Hidden>\n",
                "    <RunOnlyIfIdle>false</RunOnlyIfIdle>\n",
                "    <WakeToRun>false</WakeToRun>\n",
                "    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n",
                "  </Settings>\n",
                "  <Actions Context=\"LCRUser\">\n",
                "    <Exec>\n",
                "      <Command>{broker_path}</Command>\n",
                "      <Arguments>{arguments}</Arguments>\n",
                "    </Exec>\n",
                "  </Actions>\n",
                "</Task>\n"
            ),
            description = description,
            user_sid = user_sid,
            broker_path = broker_path,
            arguments = BROKER_TASK_ARGUMENTS,
        )
    }
}

pub(crate) fn valid_sid_text(value: &str) -> bool {
    let mut parts = value.split('-');

    if parts.next() != Some("S") || parts.next() != Some("1") {
        return false;
    }

    let remainder = parts.collect::<Vec<_>>();

    // Identifier authority plus at least one sub-authority.
    remainder.len() >= 2
        && remainder
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }

    escaped
}

use windows::{
    core::{Error as WindowsError, Interface, Result as WindowsResult, BSTR, GUID, HRESULT},
    Win32::System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_MULTITHREADED,
        },
        TaskScheduler::{
            IAction, IExecAction, IRegisteredTask, ITaskFolder, ITaskService, TASK_ACTION_EXEC,
            TASK_CREATE, TASK_CREATE_OR_UPDATE, TASK_DONT_ADD_PRINCIPAL_ACE,
            TASK_IGNORE_REGISTRATION_TRIGGERS, TASK_INSTANCES_STOP_EXISTING,
            TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_HIGHEST,
        },
        Variant::VARIANT,
    },
};

use windows::Win32::Foundation::VARIANT_BOOL;
#[cfg(test)]
use windows::Win32::System::TaskScheduler::TASK_VALIDATE_ONLY;

const TASK_SCHEDULER_CLASS_ID: GUID = GUID::from_u128(0x0f87369f_a4e5_4cfc_bd3e_73e6154572dd);
const HRESULT_FILE_NOT_FOUND: i32 = 0x8007_0002_u32 as i32;
const BROKER_TASK_REGISTRATION_FLAGS: i32 =
    TASK_CREATE_OR_UPDATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0 | TASK_IGNORE_REGISTRATION_TRIGGERS.0;

const BROKER_TASK_FIRST_INSTALL_FLAGS: i32 =
    TASK_CREATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0 | TASK_IGNORE_REGISTRATION_TRIGGERS.0;
struct ComApartment;

impl ComApartment {
    fn initialize() -> WindowsResult<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }

        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn with_task_scheduler_root<T>(
    operation: impl FnOnce(&ITaskFolder) -> WindowsResult<T>,
) -> WindowsResult<T> {
    let _apartment = ComApartment::initialize()?;

    let service: ITaskService =
        unsafe { CoCreateInstance(&TASK_SCHEDULER_CLASS_ID, None, CLSCTX_INPROC_SERVER)? };

    let empty = VARIANT::default();

    unsafe {
        service.Connect(&empty, &empty, &empty, &empty)?;
    }

    let root_path = BSTR::from(r"\");
    let root = unsafe { service.GetFolder(&root_path)? };

    operation(&root)
}

/// Read-only query for the one fixed LCR broker task.
///
/// This does not create, update, delete, enable, disable, run, or stop a task.
pub fn broker_task_exists_via_scheduler() -> WindowsResult<bool> {
    with_task_scheduler_root(|root| {
        let path = BSTR::from(BROKER_TASK_NAME);

        match unsafe { root.GetTask(&path) } {
            Ok(_) => Ok(true),
            Err(error) if error.code().0 == HRESULT_FILE_NOT_FOUND => Ok(false),
            Err(error) => Err(error),
        }
    })
}

const DACL_SECURITY_INFORMATION: i32 = 0x0000_0004;
const HRESULT_E_FAIL: i32 = 0x8000_4005_u32 as i32;

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrokerTaskSettingsSnapshot {
    multiple_instances: i32,
    disallow_start_if_on_batteries: bool,
    stop_if_going_on_batteries: bool,
    allow_hard_terminate: bool,
    start_when_available: bool,
    run_only_if_network_available: bool,
    allow_start_on_demand: bool,
    enabled: bool,
    hidden: bool,
    run_only_if_idle: bool,
    wake_to_run: bool,
    execution_time_limit: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BrokerTaskActionSnapshot {
    Exec {
        path: String,
        arguments: String,
        working_directory: String,
    },
    Other {
        action_type: i32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrokerTaskSnapshot {
    task_name: String,
    task_path: String,
    registered_enabled: bool,
    author: String,
    description: String,
    principal_id: String,
    user_sid: String,
    logon_type: i32,
    run_level: i32,
    trigger_count: i32,
    settings: BrokerTaskSettingsSnapshot,
    action_context: String,
    actions: Vec<BrokerTaskActionSnapshot>,
    security_descriptor: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerTaskVerification {
    Absent,
    Exact,
    Mismatch,
}

/// Exact recognized persistent Broker authority, including historical
/// versioned paths that still satisfy the full fixed Task contract.
///
/// This is deliberately stronger than discovering a path string. Construction
/// is possible only after the actual registered Task snapshot matches a
/// `BrokerTaskContract` rebuilt from the real Program Files Known Folder and
/// canonical SemVer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerTaskVersionRelation {
    Current,
    Older,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecognizedBrokerTaskAuthority {
    version: String,
    relation: BrokerTaskVersionRelation,
    contract: BrokerTaskContract,
}

impl RecognizedBrokerTaskAuthority {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn relation(&self) -> BrokerTaskVersionRelation {
        self.relation
    }

    pub fn contract(&self) -> &BrokerTaskContract {
        &self.contract
    }

    pub fn broker_path(&self) -> &Path {
        self.contract.broker_path()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecognizedBrokerTaskVerification {
    Absent,
    Exact(RecognizedBrokerTaskAuthority),
    Mismatch,
}

fn bstr_to_string(value: BSTR) -> String {
    value.to_string()
}

fn read_bstr_out(operation: impl FnOnce(*mut BSTR) -> WindowsResult<()>) -> WindowsResult<String> {
    let mut value = BSTR::default();
    operation(&mut value)?;
    Ok(bstr_to_string(value))
}

fn read_variant_bool(
    operation: impl FnOnce(*mut VARIANT_BOOL) -> WindowsResult<()>,
) -> WindowsResult<bool> {
    let mut value = VARIANT_BOOL::default();
    operation(&mut value)?;
    Ok(value.0 != 0)
}

fn task_xml_user_id(xml: &str) -> Option<&str> {
    const OPEN: &str = "<UserId>";
    const CLOSE: &str = "</UserId>";

    if xml.match_indices(OPEN).count() != 1 || xml.match_indices(CLOSE).count() != 1 {
        return None;
    }

    let start = xml.find(OPEN)? + OPEN.len();
    let remainder = &xml[start..];
    let end = remainder.find(CLOSE)?;

    Some(&remainder[..end])
}

fn canonical_task_user_id(principal_user_id: &str, task_xml: &str) -> String {
    task_xml_user_id(task_xml)
        .unwrap_or(principal_user_id)
        .to_owned()
}

fn task_security_readback_matches(actual_sddl: &str, contract: &BrokerTaskContract) -> bool {
    let expected = contract.security_descriptor();

    if actual_sddl == expected {
        return true;
    }

    // Controlled Windows Task Scheduler readback maps the generic task
    // access masks to this object-specific representation and marks the
    // protected DACL auto-inherited. Accept only that exact canonical form.
    let scheduler_canonical = format!(
        "D:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;{})",
        contract.user_sid()
    );

    actual_sddl == scheduler_canonical
}

fn snapshot_registered_broker_task(task: &IRegisteredTask) -> WindowsResult<BrokerTaskSnapshot> {
    let task_name = bstr_to_string(unsafe { task.Name()? });
    let task_path = bstr_to_string(unsafe { task.Path()? });
    let registered_enabled = unsafe { task.Enabled()? }.0 != 0;

    let definition = unsafe { task.Definition()? };

    let registration = unsafe { definition.RegistrationInfo()? };
    let author = read_bstr_out(|value| unsafe { registration.Author(value) })?;
    let description = read_bstr_out(|value| unsafe { registration.Description(value) })?;

    let principal = unsafe { definition.Principal()? };

    let principal_id = read_bstr_out(|value| unsafe { principal.Id(value) })?;

    let principal_user_id = read_bstr_out(|value| unsafe { principal.UserId(value) })?;
    let task_xml = bstr_to_string(unsafe { task.Xml()? });
    let user_sid = canonical_task_user_id(&principal_user_id, &task_xml);

    let mut logon_type = TASK_LOGON_INTERACTIVE_TOKEN;
    unsafe {
        principal.LogonType(&mut logon_type)?;
    }

    let mut run_level = TASK_RUNLEVEL_HIGHEST;
    unsafe {
        principal.RunLevel(&mut run_level)?;
    }

    let triggers = unsafe { definition.Triggers()? };
    let mut trigger_count = 0i32;
    unsafe {
        triggers.Count(&mut trigger_count)?;
    }

    let settings = unsafe { definition.Settings()? };

    let mut multiple_instances = TASK_INSTANCES_STOP_EXISTING;
    unsafe {
        settings.MultipleInstances(&mut multiple_instances)?;
    }

    let disallow_start_if_on_batteries =
        read_variant_bool(|value| unsafe { settings.DisallowStartIfOnBatteries(value) })?;

    let stop_if_going_on_batteries =
        read_variant_bool(|value| unsafe { settings.StopIfGoingOnBatteries(value) })?;

    let allow_hard_terminate =
        read_variant_bool(|value| unsafe { settings.AllowHardTerminate(value) })?;

    let start_when_available =
        read_variant_bool(|value| unsafe { settings.StartWhenAvailable(value) })?;

    let run_only_if_network_available =
        read_variant_bool(|value| unsafe { settings.RunOnlyIfNetworkAvailable(value) })?;

    let allow_start_on_demand =
        read_variant_bool(|value| unsafe { settings.AllowDemandStart(value) })?;

    let enabled = read_variant_bool(|value| unsafe { settings.Enabled(value) })?;

    let hidden = read_variant_bool(|value| unsafe { settings.Hidden(value) })?;

    let run_only_if_idle = read_variant_bool(|value| unsafe { settings.RunOnlyIfIdle(value) })?;

    let wake_to_run = read_variant_bool(|value| unsafe { settings.WakeToRun(value) })?;

    let execution_time_limit =
        read_bstr_out(|value| unsafe { settings.ExecutionTimeLimit(value) })?;

    let settings = BrokerTaskSettingsSnapshot {
        multiple_instances: multiple_instances.0,
        disallow_start_if_on_batteries,
        stop_if_going_on_batteries,
        allow_hard_terminate,
        start_when_available,
        run_only_if_network_available,
        allow_start_on_demand,
        enabled,
        hidden,
        run_only_if_idle,
        wake_to_run,
        execution_time_limit,
    };

    let actions = unsafe { definition.Actions()? };

    let action_context = read_bstr_out(|value| unsafe { actions.Context(value) })?;

    let mut action_count = 0i32;
    unsafe {
        actions.Count(&mut action_count)?;
    }

    let mut action_snapshots = Vec::with_capacity(action_count.max(0) as usize);

    for index in 1..=action_count {
        let action: IAction = unsafe { actions.get_Item(index)? };

        let mut action_type = TASK_ACTION_EXEC;
        unsafe {
            action.Type(&mut action_type)?;
        }

        if action_type == TASK_ACTION_EXEC {
            let exec: IExecAction = action.cast()?;

            let path = read_bstr_out(|value| unsafe { exec.Path(value) })?;

            let arguments = read_bstr_out(|value| unsafe { exec.Arguments(value) })?;

            let working_directory = read_bstr_out(|value| unsafe { exec.WorkingDirectory(value) })?;

            action_snapshots.push(BrokerTaskActionSnapshot::Exec {
                path,
                arguments,
                working_directory,
            });
        } else {
            action_snapshots.push(BrokerTaskActionSnapshot::Other {
                action_type: action_type.0,
            });
        }
    }

    let security_descriptor =
        bstr_to_string(unsafe { task.GetSecurityDescriptor(DACL_SECURITY_INFORMATION)? });

    Ok(BrokerTaskSnapshot {
        task_name,
        task_path,
        registered_enabled,
        author,
        description,
        principal_id,
        user_sid,
        logon_type: logon_type.0,
        run_level: run_level.0,
        trigger_count,
        settings,
        action_context,
        actions: action_snapshots,
        security_descriptor,
    })
}

fn snapshot_matches_contract(snapshot: &BrokerTaskSnapshot, contract: &BrokerTaskContract) -> bool {
    let expected_task_path = format!("\\{BROKER_TASK_NAME}");

    let expected_broker_path = contract
        .broker_path()
        .to_str()
        .expect("BrokerTaskContract guarantees a Unicode path");

    let expected_settings = BrokerTaskSettingsSnapshot {
        multiple_instances: TASK_INSTANCES_STOP_EXISTING.0,
        disallow_start_if_on_batteries: false,
        stop_if_going_on_batteries: false,
        allow_hard_terminate: true,
        start_when_available: false,
        run_only_if_network_available: false,
        allow_start_on_demand: true,
        enabled: true,
        hidden: false,
        run_only_if_idle: false,
        wake_to_run: false,
        execution_time_limit: "PT0S".to_owned(),
    };

    let exact_action = matches!(
        snapshot.actions.as_slice(),
        [
            BrokerTaskActionSnapshot::Exec {
                path,
                arguments,
                working_directory,
            }
        ] if
            path.as_str() == expected_broker_path
                && arguments == BROKER_TASK_ARGUMENTS
                && working_directory.is_empty()
    );

    snapshot.task_name == BROKER_TASK_NAME
        && snapshot.task_path == expected_task_path
        && snapshot.registered_enabled
        && snapshot.author == "Linh Lan Bang"
        && snapshot.description == BROKER_TASK_DESCRIPTION
        && snapshot.principal_id == "LCRUser"
        && snapshot.user_sid == contract.user_sid()
        && snapshot.logon_type == TASK_LOGON_INTERACTIVE_TOKEN.0
        && snapshot.run_level == TASK_RUNLEVEL_HIGHEST.0
        && snapshot.trigger_count == 0
        && snapshot.settings == expected_settings
        && snapshot.action_context == "LCRUser"
        && exact_action
        // Task Scheduler canonicalization is accepted only through the
        // narrow live-proven equivalence above; every ACE/SID/mask still
        // has to match the fixed contract.
        && task_security_readback_matches(&snapshot.security_descriptor, contract)
}

fn read_broker_task_via_scheduler() -> WindowsResult<Option<BrokerTaskSnapshot>> {
    with_task_scheduler_root(|root| {
        let task_name = BSTR::from(BROKER_TASK_NAME);

        match unsafe { root.GetTask(&task_name) } {
            Ok(task) => Ok(Some(snapshot_registered_broker_task(&task)?)),

            Err(error) if error.code().0 == HRESULT_FILE_NOT_FOUND => Ok(None),

            Err(error) => Err(error),
        }
    })
}

fn snapshot_broker_version(snapshot: &BrokerTaskSnapshot) -> Option<&str> {
    let [BrokerTaskActionSnapshot::Exec { path, .. }] = snapshot.actions.as_slice() else {
        return None;
    };

    let path = Path::new(path);

    if path.file_name()?.to_str()? != BROKER_EXECUTABLE_NAME {
        return None;
    }

    let version = path.parent()?.file_name()?.to_str()?;

    (!version.is_empty()).then_some(version)
}

fn recognized_version_relation(
    version: &str,
    current_version: &str,
) -> Option<BrokerTaskVersionRelation> {
    let parsed = Version::parse(version).ok()?;
    let current = Version::parse(current_version).ok()?;

    if parsed.to_string() != version || current.to_string() != current_version {
        return None;
    }

    if version == current_version {
        return Some(BrokerTaskVersionRelation::Current);
    }

    match parsed.cmp_precedence(&current) {
        Ordering::Less => Some(BrokerTaskVersionRelation::Older),

        // A future version is not a migration source. Equal SemVer
        // precedence with different text (for example build metadata)
        // is also not the exact current authority.
        Ordering::Equal | Ordering::Greater => None,
    }
}

fn recognize_broker_task_snapshot_with<F>(
    snapshot: &BrokerTaskSnapshot,
    user_sid: &str,
    current_version: &str,
    resolve_protected_path: F,
) -> RecognizedBrokerTaskVerification
where
    F: FnOnce(&str) -> std::io::Result<ProtectedBrokerPath>,
{
    let Some(version) = snapshot_broker_version(snapshot) else {
        return RecognizedBrokerTaskVerification::Mismatch;
    };

    let Some(relation) = recognized_version_relation(version, current_version) else {
        return RecognizedBrokerTaskVerification::Mismatch;
    };

    let Ok(protected_path) = resolve_protected_path(version) else {
        return RecognizedBrokerTaskVerification::Mismatch;
    };

    let Ok(contract) = BrokerTaskContract::new(user_sid, &protected_path) else {
        return RecognizedBrokerTaskVerification::Mismatch;
    };

    if !snapshot_matches_contract(snapshot, &contract) {
        return RecognizedBrokerTaskVerification::Mismatch;
    }

    RecognizedBrokerTaskVerification::Exact(RecognizedBrokerTaskAuthority {
        version: version.to_owned(),
        relation,
        contract,
    })
}

/// Recognizes the actual fixed Task as one exact canonical LCR Broker version.
///
/// The Task action path is never accepted as authority by itself. Its version
/// component is used only to reconstruct an expected `ProtectedBrokerPath`
/// from the real Program Files Known Folder, after which the complete Task
/// snapshot must match the fixed Broker contract.
///
/// No Task, filesystem, process, UAC, input, or updater state is mutated.
pub fn recognize_broker_task_via_scheduler(
    user_sid: &str,
) -> WindowsResult<RecognizedBrokerTaskVerification> {
    match read_broker_task_via_scheduler()? {
        None => Ok(RecognizedBrokerTaskVerification::Absent),

        Some(snapshot) => Ok(recognize_broker_task_snapshot_with(
            &snapshot,
            user_sid,
            CURRENT_VERSION,
            protected_broker_path_for_version,
        )),
    }
}

/// Reads back actual persistent Task Scheduler authority and compares
/// it against the fixed Broker contract.
pub fn verify_broker_task_via_scheduler(
    contract: &BrokerTaskContract,
) -> WindowsResult<BrokerTaskVerification> {
    match read_broker_task_via_scheduler()? {
        None => Ok(BrokerTaskVerification::Absent),

        Some(snapshot) if snapshot_matches_contract(&snapshot, contract) => {
            Ok(BrokerTaskVerification::Exact)
        }

        Some(_) => Ok(BrokerTaskVerification::Mismatch),
    }
}

fn broker_task_run_parameter_variant(arguments: &[String; 6]) -> WindowsResult<VARIANT> {
    let wide_arguments = arguments
        .iter()
        .map(|argument| {
            argument
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let pointers = wide_arguments
        .iter()
        .map(|argument| windows::core::PCWSTR(argument.as_ptr()))
        .collect::<Vec<_>>();

    unsafe { windows::Win32::System::Variant::InitVariantFromStringArray(&pointers) }
}

fn require_nonzero_task_engine_pid(engine_pid: u32) -> WindowsResult<u32> {
    if engine_pid == 0 {
        Err(windows::core::Error::from_hresult(windows::core::HRESULT(
            HRESULT_E_FAIL,
        )))
    } else {
        Ok(engine_pid)
    }
}

/// Demand-runs only the already-installed fixed protected Broker task.
///
/// The exact registered-task object is read back and matched against the
/// supplied protected Broker contract before that same COM object is run.
/// This function never creates, repairs, updates, deletes, enables, disables,
/// stops, or renames Task Scheduler authority.
///
/// The returned EnginePID is NOT accepted as Broker identity. It is used only
/// as a bounded lifecycle signal while the private Named Pipe is waiting for
/// the real Broker client. Actual Broker identity is established later through
/// GetNamedPipeClientProcessId plus exact session/image/handshake verification.
pub fn demand_run_broker_task_via_scheduler(
    contract: &BrokerTaskContract,
    arguments: &[String; 6],
) -> WindowsResult<u32> {
    with_task_scheduler_root(|root| {
        let task_name = BSTR::from(BROKER_TASK_NAME);
        let task = unsafe { root.GetTask(&task_name)? };

        let snapshot = snapshot_registered_broker_task(&task)?;

        if !snapshot_matches_contract(&snapshot, contract) {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                HRESULT_E_FAIL,
            )));
        }

        let parameters = broker_task_run_parameter_variant(arguments)?;
        let running = unsafe { task.Run(&parameters)? };
        let engine_pid = unsafe { running.EnginePID()? };

        require_nonzero_task_engine_pid(engine_pid)
    })
}
fn normalize_delete_task_result(result: WindowsResult<()>) -> WindowsResult<()> {
    match result {
        Ok(()) => Ok(()),

        Err(error) if error.code().0 == HRESULT_FILE_NOT_FOUND => Ok(()),

        Err(error) => Err(error),
    }
}

/// Deletes only the fixed Broker task and then proves absence.
///
/// No task name or folder path is accepted from the caller.
/// FILE_NOT_FOUND is idempotent rollback success.
pub fn delete_broker_task_and_verify_absent_via_scheduler() -> WindowsResult<()> {
    with_task_scheduler_root(|root| {
        let task_name = BSTR::from(BROKER_TASK_NAME);

        normalize_delete_task_result(unsafe { root.DeleteTask(&task_name, 0) })?;

        match unsafe { root.GetTask(&task_name) } {
            Err(error) if error.code().0 == HRESULT_FILE_NOT_FOUND => Ok(()),

            Ok(_) => Err(WindowsError::from_hresult(HRESULT(HRESULT_E_FAIL))),

            Err(error) => Err(error),
        }
    })
}
fn consume_task_registration(
    registration: WindowsResult<IRegisteredTask>,
    null_success_is_ok: bool,
) -> WindowsResult<()> {
    match registration {
        Ok(task) => {
            // IRegisteredTask is apartment-bound COM authority. Release it
            // before with_task_scheduler_root drops ComApartment and calls
            // CoUninitialize; never let this interface escape that scope.
            drop(task);
            Ok(())
        }
        Err(error) if null_success_is_ok && error.code().is_ok() => Ok(()),
        Err(error) => Err(error),
    }
}

fn register_task_via_scheduler(
    contract: &BrokerTaskContract,
    flags: i32,
    null_success_is_ok: bool,
) -> WindowsResult<()> {
    with_task_scheduler_root(|root| {
        let task_name = BSTR::from(BROKER_TASK_NAME);

        let xml_text = contract.xml();
        let xml = BSTR::from(xml_text.as_str());

        let sddl_text = contract.security_descriptor();
        let sddl = VARIANT::from(BSTR::from(sddl_text.as_str()));

        let empty = VARIANT::default();

        let registration = unsafe {
            root.RegisterTask(
                &task_name,
                &xml,
                flags,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &sddl,
            )
        };

        consume_task_registration(registration, null_success_is_ok)
    })
}

/// Creates or repairs the one protected LCR broker task.
///
/// The caller owns elevation. This function never launches the broker and
/// never creates another task name. Registration is idempotent: the exact
/// contract replaces an existing definition at the fixed task name.
///
/// `TASK_DONT_ADD_PRINCIPAL_ACE` preserves the supplied SDDL as the complete
/// task authority contract. Registration triggers are ignored as an additional
/// demand-only safety boundary.
pub fn register_or_update_broker_task_via_scheduler(
    contract: &BrokerTaskContract,
) -> WindowsResult<()> {
    register_task_via_scheduler(contract, BROKER_TASK_REGISTRATION_FLAGS, false)
}

/// Creates the fixed protected Broker task for a pristine first install.
///
/// Unlike repair/update registration, this uses `TASK_CREATE`, never
/// `TASK_CREATE_OR_UPDATE`. If the fixed task appears after preflight,
/// Task Scheduler rejects the create rather than overwriting that task.
pub fn create_broker_task_first_install_via_scheduler(
    contract: &BrokerTaskContract,
) -> WindowsResult<()> {
    register_task_via_scheduler(contract, BROKER_TASK_FIRST_INSTALL_FLAGS, false)
}
#[cfg(test)]
fn normalize_validate_only_registration(
    validation: WindowsResult<IRegisteredTask>,
) -> WindowsResult<()> {
    // Keep the validate-only null-success projection, but consume any real
    // interface through the same apartment-local ownership boundary.
    consume_task_registration(validation, true)
}

/// Asks the Windows Task Scheduler service to validate the exact P2A XML and
/// SDDL contract. TASK_VALIDATE_ONLY is the sole registration flag: no task
/// is created or updated and no broker process is launched.
#[cfg(test)]
pub(crate) fn validate_task_contract_via_scheduler(
    contract: &BrokerTaskContract,
) -> WindowsResult<()> {
    register_task_via_scheduler(contract, TASK_VALIDATE_ONLY.0, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_install_task_registration_is_create_only() {
        assert_eq!(BROKER_TASK_FIRST_INSTALL_FLAGS, 0x32);
        assert_eq!(BROKER_TASK_REGISTRATION_FLAGS, 0x36);
        assert_ne!(
            BROKER_TASK_FIRST_INSTALL_FLAGS,
            BROKER_TASK_REGISTRATION_FLAGS
        );
    }
    use crate::broker_install::protected_broker_path_for_test;

    const USER_SID: &str = "S-1-5-21-111111111-222222222-333333333-1001";
    const LIVE_USER_SID_ENV: &str = "LCR_R1C2P2A2_USER_SID";

    #[test]
    fn validate_only_projection_accepts_null_success_interface() {
        let projected: WindowsResult<IRegisteredTask> = Err(windows::core::Error::empty());

        assert!(normalize_validate_only_registration(projected).is_ok());
    }

    #[test]
    fn validate_only_projection_preserves_failure_hresult() {
        let access_denied = windows::core::HRESULT(0x8007_0005_u32 as i32);

        let projected: WindowsResult<IRegisteredTask> =
            Err(windows::core::Error::from_hresult(access_denied));

        let error = normalize_validate_only_registration(projected)
            .expect_err("failure HRESULT must remain an error");

        assert_eq!(error.code(), access_denied);
    }

    #[test]
    #[ignore = "requires the local Windows Task Scheduler service"]
    fn task_scheduler_com_validate_only_does_not_persist_task() -> WindowsResult<()> {
        let user_sid =
            std::env::var(LIVE_USER_SID_ENV).expect("live validation SID must be supplied");
        let program_files =
            std::env::var_os("ProgramFiles").expect("ProgramFiles must be available");

        let broker_path = protected_broker_path_for_test(Path::new(&program_files), "0.0.0-test")
            .expect("live validation path must satisfy protected-path contract");

        let contract = BrokerTaskContract::new(&user_sid, &broker_path)
            .expect("live machine inputs form a valid task contract");

        let existed_before = broker_task_exists_via_scheduler()?;
        assert!(
            !existed_before,
            "R1C2P2A2 requires the persistent LCR broker task to be absent before validation"
        );

        validate_task_contract_via_scheduler(&contract)?;

        let existed_after = broker_task_exists_via_scheduler()?;
        assert!(
            !existed_after,
            "TASK_VALIDATE_ONLY must not create the persistent LCR broker task"
        );

        println!("R1C2P2A2_COM_VALIDATE_ONLY=PASS existed_before=false existed_after=false");

        Ok(())
    }

    fn contract() -> BrokerTaskContract {
        let broker_path =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.4-beta")
                .expect("valid protected Broker path");

        BrokerTaskContract::new(USER_SID, &broker_path).expect("valid task contract")
    }

    #[test]
    fn production_registration_flags_are_create_or_update_acl_exact_and_trigger_safe() {
        assert_eq!(BROKER_TASK_REGISTRATION_FLAGS, 0x36);
        assert_eq!(BROKER_TASK_REGISTRATION_FLAGS & TASK_VALIDATE_ONLY.0, 0);
    }

    #[test]
    fn task_contract_binds_exact_protected_broker_path() {
        let contract = contract();

        assert_eq!(
            contract.broker_path(),
            Path::new(r"C:\Program Files\LCR\InputBroker\0.0.4-beta\LCRInputBroker.exe")
        );
        assert_eq!(contract.user_sid(), USER_SID);
    }

    #[test]
    fn task_principal_readback_uses_exact_sid_from_registered_xml() {
        let xml = format!(
            "<Task><Principals><Principal><UserId>{USER_SID}</UserId></Principal></Principals></Task>"
        );

        assert_eq!(canonical_task_user_id("Kjndst", &xml), USER_SID);

        assert_eq!(canonical_task_user_id(USER_SID, "<Task />"), USER_SID);

        let ambiguous =
            format!("<Task><UserId>{USER_SID}</UserId><UserId>{USER_SID}</UserId></Task>");

        assert_eq!(canonical_task_user_id("Kjndst", &ambiguous), "Kjndst");
    }

    #[test]
    fn task_security_readback_accepts_only_live_proven_scheduler_canonicalization() {
        let contract = contract();
        let exact = contract.security_descriptor();

        assert!(task_security_readback_matches(&exact, &contract));

        let scheduler_canonical =
            format!("D:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;{USER_SID})");

        assert!(task_security_readback_matches(
            &scheduler_canonical,
            &contract
        ));

        assert!(!task_security_readback_matches(
            &format!("D:AI(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;{USER_SID})"),
            &contract
        ));

        assert!(!task_security_readback_matches(
            &format!("D:PAR(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;{USER_SID})"),
            &contract
        ));

        assert!(!task_security_readback_matches(
            &format!("D:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{USER_SID})"),
            &contract
        ));
    }

    #[test]
    fn task_security_descriptor_is_least_privilege_for_owner() {
        assert_eq!(
            contract().security_descriptor(),
            concat!(
                "D:P",
                "(A;;GA;;;SY)",
                "(A;;GA;;;BA)",
                "(A;;GRGX;;;S-1-5-21-111111111-222222222-333333333-1001)"
            )
        );
    }

    #[test]
    fn task_xml_is_visible_demand_only_and_trigger_free() {
        let xml = contract().xml();

        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));

        assert!(xml.contains("<MultipleInstancesPolicy>StopExisting</MultipleInstancesPolicy>"));

        assert!(xml.contains("<AllowStartOnDemand>true</AllowStartOnDemand>"));
        assert!(xml.contains("<Hidden>false</Hidden>"));
        assert!(xml.contains("<WakeToRun>false</WakeToRun>"));
        assert!(xml.contains("<StartWhenAvailable>false</StartWhenAvailable>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));

        assert!(!xml.contains("<Triggers"));
        assert!(!xml.contains("BootTrigger"));
        assert!(!xml.contains("LogonTrigger"));
        assert!(!xml.contains("TimeTrigger"));
        assert!(!xml.contains("RegistrationTrigger"));
    }

    #[test]
    fn task_engine_pid_zero_is_rejected_fail_closed() {
        assert!(require_nonzero_task_engine_pid(0).is_err());
        assert_eq!(
            require_nonzero_task_engine_pid(1234).expect("nonzero task engine PID"),
            1234
        );
    }
    #[test]
    fn task_xml_has_only_the_six_broker_launch_parameters() {
        let xml = contract().xml();

        assert!(xml.contains(concat!(
            "<Arguments>",
            "$(Arg0) $(Arg1) $(Arg2) $(Arg3) $(Arg4) $(Arg5)",
            "</Arguments>"
        )));

        for index in 0..6 {
            assert_eq!(xml.matches(&format!("$(Arg{index})")).count(), 1);
        }

        assert!(!xml.contains("$(Arg6)"));
        assert!(!xml.contains("--lcr-input-broker"));
    }

    #[test]
    fn task_xml_escapes_the_exact_command_path() {
        let broker_path =
            protected_broker_path_for_test(Path::new(r"C:\Program & Files"), "0.0.0-test")
                .expect("valid unusual protected Broker path");

        let contract =
            BrokerTaskContract::new(USER_SID, &broker_path).expect("valid task contract");

        let xml = contract.xml();

        assert!(xml.contains(
            r"<Command>C:\Program &amp; Files\LCR\InputBroker\0.0.0-test\LCRInputBroker.exe</Command>"
        ));
    }

    #[test]
    fn task_contract_rejects_untrusted_sid_text() {
        let broker_path =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.4-beta")
                .expect("valid protected Broker path");

        assert_eq!(
            BrokerTaskContract::new("S-1-5-21-1-2-3-1001)(A;;GA;;;WD", &broker_path,),
            Err(TaskContractError::InvalidUserSid)
        );
    }

    #[test]
    fn task_name_is_visible_root_library_name_not_a_hidden_path() {
        assert_eq!(BROKER_TASK_NAME, "Linh Lan Bang - LCR Input Broker");
        assert!(!BROKER_TASK_NAME.contains('\\'));
        assert!(!BROKER_TASK_NAME.starts_with('.'));
    }
    fn exact_task_snapshot() -> BrokerTaskSnapshot {
        let contract = contract();

        BrokerTaskSnapshot {
            task_name: BROKER_TASK_NAME.to_owned(),
            task_path: format!("\\{BROKER_TASK_NAME}"),
            registered_enabled: true,
            author: "Linh Lan Bang".to_owned(),
            description: BROKER_TASK_DESCRIPTION.to_owned(),
            principal_id: "LCRUser".to_owned(),
            user_sid: USER_SID.to_owned(),
            logon_type: TASK_LOGON_INTERACTIVE_TOKEN.0,
            run_level: TASK_RUNLEVEL_HIGHEST.0,
            trigger_count: 0,
            settings: BrokerTaskSettingsSnapshot {
                multiple_instances: TASK_INSTANCES_STOP_EXISTING.0,
                disallow_start_if_on_batteries: false,
                stop_if_going_on_batteries: false,
                allow_hard_terminate: true,
                start_when_available: false,
                run_only_if_network_available: false,
                allow_start_on_demand: true,
                enabled: true,
                hidden: false,
                run_only_if_idle: false,
                wake_to_run: false,
                execution_time_limit: "PT0S".to_owned(),
            },
            action_context: "LCRUser".to_owned(),
            actions: vec![BrokerTaskActionSnapshot::Exec {
                path: contract.broker_path().to_string_lossy().into_owned(),
                arguments: BROKER_TASK_ARGUMENTS.to_owned(),
                working_directory: String::new(),
            }],
            security_descriptor: contract.security_descriptor(),
        }
    }

    fn recognize_snapshot_for_test(
        snapshot: &BrokerTaskSnapshot,
        user_sid: &str,
    ) -> RecognizedBrokerTaskVerification {
        recognize_broker_task_snapshot_with(snapshot, user_sid, "0.0.4-beta", |version| {
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), version)
        })
    }

    #[test]
    fn recognized_task_classifier_accepts_exact_current_authority() {
        let snapshot = exact_task_snapshot();

        let RecognizedBrokerTaskVerification::Exact(authority) =
            recognize_snapshot_for_test(&snapshot, USER_SID)
        else {
            panic!("exact current authority must be recognized");
        };

        assert_eq!(authority.version(), "0.0.4-beta");
        assert_eq!(authority.relation(), BrokerTaskVersionRelation::Current);

        assert_eq!(
            authority.broker_path(),
            Path::new(r"C:\Program Files\LCR\InputBroker\0.0.4-beta\LCRInputBroker.exe")
        );

        assert_eq!(authority.contract().user_sid(), USER_SID);
    }

    #[test]
    fn recognized_task_classifier_accepts_exact_older_canonical_version() {
        let mut snapshot = exact_task_snapshot();

        let older = protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.3-beta")
            .expect("older protected Broker path");

        snapshot.actions = vec![BrokerTaskActionSnapshot::Exec {
            path: older.as_path().to_string_lossy().into_owned(),
            arguments: BROKER_TASK_ARGUMENTS.to_owned(),
            working_directory: String::new(),
        }];

        let RecognizedBrokerTaskVerification::Exact(authority) =
            recognize_snapshot_for_test(&snapshot, USER_SID)
        else {
            panic!("exact older authority must be recognized");
        };

        assert_eq!(authority.version(), "0.0.3-beta");
        assert_eq!(authority.relation(), BrokerTaskVersionRelation::Older);
        assert_eq!(authority.broker_path(), older.as_path());
    }

    #[test]
    fn recognized_task_classifier_rejects_future_canonical_version() {
        let mut snapshot = exact_task_snapshot();

        let future = protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.5-beta")
            .expect("future protected Broker path");

        snapshot.actions = vec![BrokerTaskActionSnapshot::Exec {
            path: future.as_path().to_string_lossy().into_owned(),
            arguments: BROKER_TASK_ARGUMENTS.to_owned(),
            working_directory: String::new(),
        }];

        assert_eq!(
            recognize_snapshot_for_test(&snapshot, USER_SID),
            RecognizedBrokerTaskVerification::Mismatch
        );
    }

    #[test]
    fn recognized_task_classifier_rejects_equal_precedence_different_text() {
        let mut snapshot = exact_task_snapshot();

        let build_variant =
            protected_broker_path_for_test(Path::new(r"C:\Program Files"), "0.0.4-beta+other")
                .expect("canonical build-metadata Broker path");

        snapshot.actions = vec![BrokerTaskActionSnapshot::Exec {
            path: build_variant.as_path().to_string_lossy().into_owned(),
            arguments: BROKER_TASK_ARGUMENTS.to_owned(),
            working_directory: String::new(),
        }];

        assert_eq!(
            recognize_snapshot_for_test(&snapshot, USER_SID),
            RecognizedBrokerTaskVerification::Mismatch
        );
    }

    #[test]
    fn recognized_task_classifier_rejects_arbitrary_path_even_with_semver_parent() {
        let mut snapshot = exact_task_snapshot();

        snapshot.actions = vec![BrokerTaskActionSnapshot::Exec {
            path: r"C:\Untrusted\0.0.3-beta\LCRInputBroker.exe".to_owned(),
            arguments: BROKER_TASK_ARGUMENTS.to_owned(),
            working_directory: String::new(),
        }];

        assert_eq!(
            recognize_snapshot_for_test(&snapshot, USER_SID),
            RecognizedBrokerTaskVerification::Mismatch
        );
    }

    #[test]
    fn recognized_task_classifier_rejects_noncanonical_version_text() {
        let mut snapshot = exact_task_snapshot();

        snapshot.actions = vec![BrokerTaskActionSnapshot::Exec {
            path: concat!(
                r"C:\Program Files\LCR\InputBroker\",
                r"00.0.4-beta\LCRInputBroker.exe"
            )
            .to_owned(),
            arguments: BROKER_TASK_ARGUMENTS.to_owned(),
            working_directory: String::new(),
        }];

        assert_eq!(
            recognize_snapshot_for_test(&snapshot, USER_SID),
            RecognizedBrokerTaskVerification::Mismatch
        );
    }

    #[test]
    fn recognized_task_classifier_preserves_full_snapshot_fail_closed_contract() {
        let mut snapshot = exact_task_snapshot();

        snapshot.settings.allow_start_on_demand = false;

        assert_eq!(
            recognize_snapshot_for_test(&snapshot, USER_SID),
            RecognizedBrokerTaskVerification::Mismatch
        );

        let snapshot = exact_task_snapshot();

        assert_eq!(
            recognize_snapshot_for_test(&snapshot, "S-1-bad",),
            RecognizedBrokerTaskVerification::Mismatch
        );
    }

    #[test]
    fn typed_snapshot_exact_contract_matches() {
        assert!(snapshot_matches_contract(
            &exact_task_snapshot(),
            &contract()
        ));
    }

    #[test]
    fn typed_snapshot_fails_closed_on_principal_trigger_settings_action_and_acl_drift() {
        let contract = contract();

        let mut snapshot = exact_task_snapshot();
        snapshot.user_sid = "S-1-5-21-1-2-3-9999".to_owned();
        assert!(!snapshot_matches_contract(&snapshot, &contract));

        let mut snapshot = exact_task_snapshot();
        snapshot.run_level = 0;
        assert!(!snapshot_matches_contract(&snapshot, &contract));

        let mut snapshot = exact_task_snapshot();
        snapshot.trigger_count = 1;
        assert!(!snapshot_matches_contract(&snapshot, &contract));

        let mut snapshot = exact_task_snapshot();
        snapshot.settings.allow_start_on_demand = false;
        assert!(!snapshot_matches_contract(&snapshot, &contract));

        let mut snapshot = exact_task_snapshot();
        snapshot
            .actions
            .push(BrokerTaskActionSnapshot::Other { action_type: 999 });
        assert!(!snapshot_matches_contract(&snapshot, &contract));

        let mut snapshot = exact_task_snapshot();
        snapshot.actions = vec![BrokerTaskActionSnapshot::Exec {
            path: r"C:\untrusted\broker.exe".to_owned(),
            arguments: BROKER_TASK_ARGUMENTS.to_owned(),
            working_directory: String::new(),
        }];
        assert!(!snapshot_matches_contract(&snapshot, &contract));

        let mut snapshot = exact_task_snapshot();
        snapshot.security_descriptor = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{})", USER_SID);
        assert!(!snapshot_matches_contract(&snapshot, &contract));
    }

    #[test]
    fn delete_projection_accepts_already_absent_and_preserves_other_errors() {
        let absent = WindowsError::from_hresult(HRESULT(HRESULT_FILE_NOT_FOUND));

        assert!(normalize_delete_task_result(Err(absent)).is_ok());

        let access_denied = HRESULT(0x8007_0005_u32 as i32);

        let error = normalize_delete_task_result(Err(WindowsError::from_hresult(access_denied)))
            .expect_err("non-absence deletion failure must remain visible");

        assert_eq!(error.code(), access_denied);
    }
}
