//! Shared client assembly for the CLI and the desktop shell.
//!
//! One [`Workspace`] owns the catalog, the operation journal and the platform
//! binding surface. Clients send typed [`Request`] values and receive one
//! [`Envelope`]; no storage, native or UI type crosses the wire.
//!
//! Operations that would need a qualified native effect port are refused with
//! a typed unavailable reason. They never report simulated success. Recovery is
//! reachable because a crash can leave recorded pending work that must be
//! reconciled.

use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};

mod selection;

pub use selection::{Purpose, Selection, SelectionId};
use selection::{SelectionStore, Source};

use marduk_core::deployment::{
    DeployError, OperationId, OperationKind, OperationStatus, Phase, PlanId, PreState, StepEvent,
};
use marduk_core::ids::{MemberId, ProfileId, ProfileRevision};
use marduk_core::import::NamedProfile;
use marduk_core::import::{
    ArchiveError, BeginImport, ImportError, ImportId, ImportTarget, LimitKind, PreparedImport,
};
use marduk_core::recovery::{BackupRef, BackupSlot, Journal, StepDirection};
use marduk_core::root_identity::{NativeIdentity, RootError, RootIdentity, UnsupportedReason};
use marduk_games::skyrim_se;
use marduk_platform::{DeploymentOwner, InstallationBinding};
use marduk_storage::Catalog;
use marduk_storage::library::{QuarantineReason, ReconcileReport};
use marduk_storage::native_effects::NativeEffects;
use marduk_storage::operation_journal::OperationJournal;
use marduk_storage::operation_journal::SelectedInstallation;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

/// Bundle identifier that fixes the desktop and CLI data root.
///
/// Phase B must configure the Tauri identifier to this exact value so
/// `app.path().app_data_dir()` resolves to the same catalog as the CLI.
pub const APP_IDENTIFIER: &str = "com.marduk.desktop";

/// Join the OS data directory with [`APP_IDENTIFIER`]. Pure so tests can
/// check the frozen layout without touching the process environment.
pub fn app_data_root_in(data_dir: &Path) -> PathBuf {
    data_dir.join(APP_IDENTIFIER)
}

/// The one application data root used by every client.
///
/// This is exactly Tauri 2's `app_data_dir()` resolution for
/// [`APP_IDENTIFIER`]: `dirs::data_dir()` plus the bundle identifier. It uses
/// the Windows known-folder roaming data directory (not the `APPDATA`
/// environment variable), and on Linux it ignores a relative `XDG_DATA_HOME` in
/// favour of `$HOME/.local/share`. No client may derive a different location.
pub fn app_data_root() -> Result<PathBuf, WireError> {
    dirs::data_dir()
        .map(|base| app_data_root_in(&base))
        .ok_or_else(|| WireError::new(Code::Storage))
}

/// The single M1 profile. Setup returns or initializes it.
fn profile() -> ProfileId {
    ProfileId::new(1)
}

/// Request operation names. These are internal wire names, not CLI verbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Setup,
    Status,
    ImportPreview,
    ImportApply,
    Order,
    DeployPreview,
    DeployApply,
    Restore,
    Recover,
    Operation,
}

/// One client request: `{op,args}`. Each handler decodes only its own args.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub op: Op,
    #[serde(default = "empty_args")]
    pub args: Value,
}

impl Request {
    pub fn new(op: Op, args: Value) -> Self {
        Self { op, args }
    }
}

fn empty_args() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Stable wire result. Exactly one envelope per invocation.
#[derive(Clone, Debug)]
pub enum Envelope<T> {
    Ok { result: T },
    Error { error: WireError },
}

impl<T: Serialize> Serialize for Envelope<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            Self::Ok { result } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("result", result)?;
            }
            Self::Error { error } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("error", error)?;
            }
        }
        map.end()
    }
}

impl<T> Envelope<T> {
    pub fn ok(result: T) -> Self {
        Self::Ok { result }
    }

    pub fn error(error: WireError) -> Self {
        Self::Error { error }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

/// Stable client error codes. The strings are part of the client contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Code {
    InvalidRequest,
    InvalidName,
    InvalidOrder,
    StalePlan,
    MemberNotFound,
    Busy,
    UnknownImport,
    ImportNotPrepared,
    StageChanged,
    StageUnavailable,
    Storage,
    InvalidContainer,
    UnsupportedContainer,
    DuplicateRawName,
    UnsupportedEntry,
    UnsafeName,
    ConflictingNames,
    LimitExceeded,
    CorruptEntry,
    MissingEntry,
    InvalidPath,
    TargetUnavailable,
    UnsupportedTarget,
    ActiveDeployment,
    RecoveryConflict,
    UnknownOperation,
    NoActiveDeployment,
    Io,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "InvalidRequest",
            Self::InvalidName => "InvalidName",
            Self::InvalidOrder => "InvalidOrder",
            Self::StalePlan => "StalePlan",
            Self::MemberNotFound => "MemberNotFound",
            Self::Busy => "Busy",
            Self::UnknownImport => "UnknownImport",
            Self::ImportNotPrepared => "ImportNotPrepared",
            Self::StageChanged => "StageChanged",
            Self::StageUnavailable => "StageUnavailable",
            Self::Storage => "Storage",
            Self::InvalidContainer => "InvalidContainer",
            Self::UnsupportedContainer => "UnsupportedContainer",
            Self::DuplicateRawName => "DuplicateRawName",
            Self::UnsupportedEntry => "UnsupportedEntry",
            Self::UnsafeName => "UnsafeName",
            Self::ConflictingNames => "ConflictingNames",
            Self::LimitExceeded => "LimitExceeded",
            Self::CorruptEntry => "CorruptEntry",
            Self::MissingEntry => "MissingEntry",
            Self::InvalidPath => "InvalidPath",
            Self::TargetUnavailable => "TargetUnavailable",
            Self::UnsupportedTarget => "UnsupportedTarget",
            Self::ActiveDeployment => "ActiveDeployment",
            Self::RecoveryConflict => "RecoveryConflict",
            Self::UnknownOperation => "UnknownOperation",
            Self::NoActiveDeployment => "NoActiveDeployment",
            Self::Io => "Io",
        }
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The only detail shapes the client contract permits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Detail {
    Index { index: usize },
    Pair { first: usize, second: usize },
    Kind { kind: LimitKindDto },
    Reason { reason: ReasonDto },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LimitKindDto {
    ArchiveBytes,
    DirectoryBytes,
    Entries,
    FileBytes,
    TotalBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasonDto {
    MissingParent,
    AmbiguousName,
    OutsideBoundary,
    ObjectKind,
    Metadata,
    Sharing,
    Configuration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: Code,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
    /// Human-readable context for the typed refusal, such as the owning
    /// catalog root of a foreign deployment marker. Clients show it beside
    /// the stable code; they never branch on its wording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl WireError {
    pub fn new(code: Code) -> Self {
        Self {
            code,
            detail: None,
            message: None,
        }
    }

    pub fn with_detail(code: Code, detail: Detail) -> Self {
        Self {
            code,
            detail: Some(detail),
            message: None,
        }
    }

    pub fn with_message(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            detail: None,
            message: Some(message.into()),
        }
    }
}

pub type Blocker = WireError;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub installed_id: String,
    pub enabled: bool,
    pub display_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub revision: String,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Availability {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<Blocker>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retained {
    pub operation_id: String,
    pub ordinal: u32,
    pub direction: Direction,
    pub slot: Slot,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Forward,
    Undo,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Original,
    Step,
    Displaced,
    /// Detection-only location positively observed during recovery. It is a
    /// report location, never a publication destination.
    Alternate,
    Stage,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deploy,
    Restore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    pub kind: Kind,
    pub phase: String,
    pub generation_id: Option<String>,
    pub blocker: Option<Blocker>,
    pub retained: Vec<Retained>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailabilitySet {
    pub target: Availability,
    pub import: Availability,
    pub deploy: Availability,
    pub restore: Availability,
}

/// What the one startup reconciliation did. It is computed when the
/// [`Workspace`] opens and never rerun per status load.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reconciliation {
    /// Imports whose abandoned private snapshot was reclaimed at this open.
    pub reclaimed: Vec<String>,
    /// Imports left untouched because the catalog could not prove them.
    pub quarantined: Vec<QuarantinedLeftover>,
    /// Number of entries in [`Self::quarantined`], for a count-only display.
    pub quarantined_count: usize,
}

/// One leftover import that reconciliation left in place. `reason` is a stable
/// token; clients may display it but never parse its prose.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantinedLeftover {
    pub id: String,
    pub reason: String,
}

/// The catalog root a readable deployment marker records.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentInfo {
    pub owner: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    pub installation: Option<Installation>,
    pub profile: Snapshot,
    pub availability: AvailabilitySet,
    pub pending: Option<Operation>,
    pub active_generation_id: Option<String>,
    /// The owner of a readable marker, or `null` if no owner could be read.
    pub deployment: Option<DeploymentInfo>,
    pub reconciliation: Reconciliation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub role: Role,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Data,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub id: String,
    pub profile_id: String,
    pub expected_revision: String,
    pub archive_sha256: String,
    pub files: Vec<Target>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFile {
    pub target: Target,
    pub winner: String,
    pub candidates: Vec<String>,
    pub needs_backup: bool,
}

/// Deployment preview shape. Phase B must produce it from the qualified effect
/// port and round-trip it through this DTO before the desktop shows Review.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub id: Option<String>,
    pub profile_id: String,
    pub revision: String,
    pub installation_id: String,
    pub label: String,
    pub files: Vec<PreviewFile>,
    pub blockers: Vec<Blocker>,
}

/// Restore outcome shape. Phase B must produce both variants and cover their
/// round trip; phase A only returns the typed unsupported blocker.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum RestoreResult {
    Operation { operation: Operation },
    NoActiveDeployment { code: Code },
}

/// Ordered progress hint sent on the per-operation Tauri channel.
///
/// It is never authoritative: clients refresh `status`/`operation` on a gap,
/// duplicate or disconnect and never claim completion from this value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub operation_id: String,
    pub sequence: String,
    pub phase: String,
}

/// The one wire token for a core phase, or nothing when the phase has no
/// ordered-hint meaning on the client.
fn phase_token(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Preparing => Some("preparing"),
        Phase::Applying => Some("applying"),
        Phase::Restoring => Some("restoring"),
        Phase::Completed => Some("completed"),
        Phase::RolledBack => Some("rolled_back"),
        Phase::Prepared | Phase::RollingBack | Phase::Cancelled => None,
    }
}

/// Number the engine's transitions for one write.
///
/// The first operation the engine reports is this call's operation; an event
/// for any other operation is not part of this channel and is dropped. The
/// sink cannot influence the operation: [`Self::emit`] returns nothing.
struct ProgressFeed<S: FnMut(Progress)> {
    operation: Option<OperationId>,
    sequence: u64,
    sink: S,
}

impl<S: FnMut(Progress)> ProgressFeed<S> {
    fn new(sink: S) -> Self {
        Self {
            operation: None,
            sequence: 0,
            sink,
        }
    }

    fn emit(&mut self, event: StepEvent) {
        let operation = *self.operation.get_or_insert(event.operation);
        if operation != event.operation {
            return;
        }
        let Some(phase) = phase_token(event.phase) else {
            return;
        };
        self.sequence += 1;
        (self.sink)(Progress {
            operation_id: decimal(operation.get()),
            sequence: decimal(self.sequence),
            phase: phase.to_owned(),
        });
    }
}

/// One shared client service over the authoritative catalog and journal.
pub struct Workspace {
    /// Absolute catalog root. Also the owner recorded in the installation's
    /// deployment marker.
    app_data: PathBuf,
    catalog: Catalog,
    journal: OperationJournal,
    selections: SelectionStore,
    reconciliation: Reconciliation,
}

impl Workspace {
    /// Open the shared state under the application data root.
    ///
    /// M1 is one client and one catalog per installation, so nothing outside
    /// this root participates in ownership; the installation marker names the
    /// catalog that deployed there.
    pub fn open(app_data: &Path) -> Result<Self, WireError> {
        let app_data =
            std::path::absolute(app_data).map_err(|_| WireError::new(Code::InvalidPath))?;
        let mut catalog = Catalog::open(&app_data).map_err(import_error)?;
        // Reclaim catalog-office leftovers exactly once per open, before any
        // command is served. A failure is the open's failure: a client must
        // never get a workspace whose abandoned imports are unaccounted for,
        // and this path never deletes anything the catalog refused to reclaim.
        let reconciliation = reconciliation(catalog.reconcile().map_err(import_error)?);
        let journal = OperationJournal::open(&app_data).map_err(deploy_error)?;
        Ok(Self {
            app_data,
            catalog,
            journal,
            selections: SelectionStore::new(),
            reconciliation,
        })
    }

    /// Record one native picker result and mint the only handle the renderer
    /// may see. Archive inputs are opened now, and an installation binds its
    /// verified identity pair now, so a later command consumes the exact object
    /// that was picked instead of rebinding a pathname.
    pub fn remember_selection(
        &mut self,
        purpose: Purpose,
        path: PathBuf,
    ) -> Result<SelectionId, WireError> {
        match purpose {
            Purpose::Archive => self.selections.insert_archive(path),
            Purpose::Installation => {
                // Make the locator absolute but never resolve its links: the
                // native binder must see and refuse a redirected picker root,
                // and consumption re-binds this same spelling.
                let resolved =
                    std::path::absolute(&path).map_err(|_| WireError::new(Code::InvalidPath))?;
                let binding = marduk_platform::bind_installation(&resolved, &skyrim_se::LAYOUT)
                    .map_err(root_error)?;
                let (installation, data) = binding.identities();
                self.selections
                    .insert_installation(resolved, installation.clone(), data.clone())
            }
        }
    }

    /// Dispatch one request. One result per invocation.
    pub fn dispatch(&mut self, request: Request) -> Envelope<Value> {
        match self.handle(request) {
            Ok(result) => Envelope::ok(result),
            Err(error) => Envelope::error(error),
        }
    }

    /// Dispatch one write request with an ordered progress sink.
    ///
    /// Only apply and restore stream, and only they reach this entry; any other
    /// request is refused so a client cannot wait for hints that never arrive.
    pub fn dispatch_streaming(
        &mut self,
        request: Request,
        progress: impl FnMut(Progress),
    ) -> Envelope<Value> {
        let result = match request.op {
            Op::DeployApply => decode::<DeployApplyArgs>(request.args).and_then(|args| {
                let plan = PlanId::try_from_u64(parse_id(&args.plan_id)?)
                    .map_err(|_| WireError::new(Code::InvalidRequest))?;
                json(self.deploy_apply_with_progress(plan, progress)?)
            }),
            Op::Restore => decode::<NoArgs>(request.args)
                .and_then(|_| json(self.restore_with_progress(progress)?)),
            _ => Err(WireError::new(Code::InvalidRequest)),
        };
        match result {
            Ok(result) => Envelope::ok(result),
            Err(error) => Envelope::error(error),
        }
    }

    fn handle(&mut self, request: Request) -> Result<Value, WireError> {
        match request.op {
            Op::Setup => {
                let args: SetupArgs = decode(request.args)?;
                let selected = self.setup(args.selection()?)?;
                Ok(json(self.status_from(Some(selected))?)?)
            }
            Op::Status => {
                let _: NoArgs = decode(request.args)?;
                Ok(json(self.status()?)?)
            }
            Op::ImportPreview => {
                let args: ImportPreviewArgs = decode(request.args)?;
                Ok(json(self.import_preview(
                    args.selection()?,
                    args.name,
                    args.at_revision,
                )?)?)
            }
            Op::ImportApply => {
                let args: ImportApplyArgs = decode(request.args)?;
                Ok(json(self.import_apply(args)?)?)
            }
            Op::Order => {
                let args: OrderArgs = decode(request.args)?;
                Ok(json(self.order(args)?)?)
            }
            Op::DeployPreview => {
                let args: DeployPreviewArgs = decode(request.args)?;
                let revision = parse_revision(&args.at_revision)?;
                Ok(json(self.deploy_preview(revision)?)?)
            }
            Op::DeployApply => {
                let args: DeployApplyArgs = decode(request.args)?;
                let plan = PlanId::try_from_u64(parse_id(&args.plan_id)?)
                    .map_err(|_| WireError::new(Code::InvalidRequest))?;
                Ok(json(self.deploy_apply(plan)?)?)
            }
            Op::Restore => {
                let _: NoArgs = decode(request.args)?;
                Ok(json(self.restore_generation()?)?)
            }
            Op::Recover => {
                let _: NoArgs = decode(request.args)?;
                Ok(json(self.recover_pending()?)?)
            }
            Op::Operation => {
                let args: OperationArgs = decode(request.args)?;
                Ok(json(self.operation(args)?)?)
            }
        }
    }

    fn setup(&mut self, selection: Selection) -> Result<SelectedInstallation, WireError> {
        let (path, picked) = match selection {
            Selection::CliPath(path) => (path, None),
            Selection::Desktop(id) => match self.selections.take(id, Purpose::Installation)? {
                Source::Installation(picked) => (picked.path.clone(), Some(picked)),
                Source::Archive(_) => return Err(WireError::new(Code::InvalidRequest)),
            },
        };
        if path.as_os_str().is_empty() {
            return Err(WireError::new(Code::InvalidRequest));
        }
        // The locator is durable, so resolve it once and use that exact path
        // for both the bind and the journal record. A relative path must never
        // become the stored location.
        let selection =
            std::path::absolute(&path).map_err(|_| WireError::new(Code::InvalidPath))?;
        let binding = marduk_platform::bind_installation(&selection, &skyrim_se::LAYOUT)
            .map_err(root_error)?;
        let (installation, data) = binding.identities();
        if picked
            .as_ref()
            .is_some_and(|picked| picked.installation != *installation || picked.data != *data)
        {
            return Err(WireError::new(Code::TargetUnavailable));
        }
        self.journal
            .select_verified_installation(&selection, installation, data)
            .map_err(deploy_error)
    }

    fn status(&mut self) -> Result<WorkspaceStatus, WireError> {
        let selected = self.journal.selected_installation().map_err(deploy_error)?;
        self.status_from(selected)
    }

    fn status_from(
        &mut self,
        selected: Option<SelectedInstallation>,
    ) -> Result<WorkspaceStatus, WireError> {
        let named = self
            .catalog
            .named_profile(profile())
            .map_err(import_error)?;

        let mut installation = None;
        let mut target = Availability {
            available: false,
            blocker: Some(WireError::new(Code::TargetUnavailable)),
        };
        let mut pending = None;
        let mut active_generation_id = None;
        let mut deployment = None;
        // Client deploy/restore is available only where the recorded pair still
        // reopens, both roots sit on a volume the record proves, and the app and
        // game roots share a rename domain. The locator's volume class alone is
        // not evidence. No bound installation has no proven pair.
        let mut effects = Availability {
            available: false,
            blocker: Some(unsupported_effect()),
        };

        if let Some(selected) = selected {
            match self.reopen(&selected) {
                Ok(binding) => {
                    target = Availability {
                        available: true,
                        blocker: None,
                    };
                    (effects, deployment) = self.deployment_state(&selected, &binding);
                }
                Err(error) => {
                    target.blocker = Some(error.clone());
                    effects.blocker = Some(error);
                }
            }
            let SelectedInstallation {
                id,
                locator,
                pending: pending_status,
                active_generation,
                ..
            } = selected;

            installation = Some(Installation {
                id: decimal(id),
                label: label(&locator),
            });
            active_generation_id = active_generation.map(decimal);
            pending = pending_status.map(operation);
        }

        let import = if self.catalog.stage_available() {
            Availability {
                available: true,
                blocker: None,
            }
        } else {
            Availability {
                available: false,
                blocker: Some(WireError::new(Code::StageUnavailable)),
            }
        };

        Ok(WorkspaceStatus {
            installation,
            profile: snapshot_dto(named),
            availability: AvailabilitySet {
                target,
                import,
                deploy: effects.clone(),
                restore: effects,
            },
            pending,
            active_generation_id,
            deployment,
            reconciliation: self.reconciliation.clone(),
        })
    }

    fn import_preview(
        &mut self,
        selection: Selection,
        name: String,
        at_revision: String,
    ) -> Result<Prepared, WireError> {
        let revision = parse_revision(&at_revision)?;
        let input = match selection {
            Selection::CliPath(path) => File::open(&path).map_err(|_| WireError::new(Code::Io))?,
            Selection::Desktop(id) => match self.selections.take(id, Purpose::Archive)? {
                Source::Archive(file) => file,
                Source::Installation(_) => return Err(WireError::new(Code::InvalidRequest)),
            },
        };
        let plan = BeginImport {
            profile: profile(),
            expected_revision: revision,
            target: ImportTarget::Append { display_name: name },
        };
        let prepared = self
            .catalog
            .prepare_import(input, plan)
            .map_err(import_error)?;
        Ok(prepared_dto(prepared))
    }

    fn import_apply(&mut self, args: ImportApplyArgs) -> Result<Snapshot, WireError> {
        let id = ImportId::try_from_u64(parse_id(&args.import_id)?)
            .map_err(|_| WireError::new(Code::InvalidRequest))?;
        // Publication stays inside the app-owned library: the adapter re-observes
        // the staged bytes before the catalog records visible membership.
        let snapshot = self.catalog.publish_import(id).map_err(import_error)?;
        let names = self
            .catalog
            .named_profile(profile())
            .map_err(import_error)?
            .names;
        Ok(snapshot_dto(NamedProfile { snapshot, names }))
    }

    fn order(&mut self, args: OrderArgs) -> Result<Snapshot, WireError> {
        let revision = parse_revision(&args.at_revision)?;
        let members = args
            .member_ids
            .iter()
            .map(|value| parse_id(value).map(MemberId::new))
            .collect::<Result<Vec<_>, _>>()?;
        let named = self
            .catalog
            .reorder_named(profile(), revision, &members)
            .map_err(edit_error)?;
        Ok(snapshot_dto(named))
    }

    fn deploy_preview(&mut self, revision: ProfileRevision) -> Result<Preview, WireError> {
        let selected = self.selected_pair()?;
        let binding = self.reopen(&selected)?;
        self.marker_owner(&binding)?;
        self.require_native_effects(&selected, &binding)?;
        let mut port = self.native_effects(&selected)?;
        let named = self
            .catalog
            .named_profile(profile())
            .map_err(import_error)?;
        if named.snapshot.revision != revision {
            return Err(WireError::new(Code::StalePlan));
        }
        let inputs = self
            .catalog
            .resolved_inputs(profile())
            .map_err(import_error)?;
        let plan = marduk_core::deployment::preview_deployment(
            &mut self.journal,
            &mut port,
            &named.snapshot,
            &inputs,
            &selected.installation,
            &selected.data,
        )
        .map_err(deploy_error)?;
        // Rebuild the candidate list from the same resolver inputs. The plan
        // carries only the winning source, so this is where losing candidates
        // (including two members that share one installed revision) stay
        // visible in the preview.
        let resolved = marduk_core::resolve::resolve_files(&named.snapshot, &inputs)
            .map_err(|_| WireError::new(Code::StalePlan))?;
        let files = plan
            .intents
            .iter()
            .map(|intent| {
                let entry = resolved
                    .entries
                    .iter()
                    .find(|entry| {
                        entry.target.relative.as_str() == intent.target.target.relative.as_str()
                    })
                    .ok_or_else(|| WireError::new(Code::StalePlan))?;
                let winner = entry
                    .candidates
                    .last()
                    .ok_or_else(|| WireError::new(Code::StalePlan))?;
                if winner.installed != intent.source.installed {
                    return Err(WireError::new(Code::StalePlan));
                }
                Ok(PreviewFile {
                    target: Target {
                        role: Role::Data,
                        path: intent.target.target.relative.as_str().to_owned(),
                    },
                    winner: decimal(winner.member.get()),
                    candidates: entry
                        .candidates
                        .iter()
                        .map(|candidate| decimal(candidate.member.get()))
                        .collect(),
                    needs_backup: matches!(intent.original, PreState::Regular(_)),
                })
            })
            .collect::<Result<Vec<_>, WireError>>()?;
        Ok(Preview {
            id: Some(decimal(plan.id.get())),
            profile_id: decimal(plan.profile.get()),
            revision: decimal(plan.revision.get()),
            installation_id: decimal(selected.id),
            label: label(&selected.locator),
            files,
            blockers: Vec::new(),
        })
    }

    fn deploy_apply(&mut self, plan: PlanId) -> Result<Operation, WireError> {
        self.deploy_apply_with_progress(plan, |_| {})
    }

    fn deploy_apply_with_progress(
        &mut self,
        plan: PlanId,
        progress: impl FnMut(Progress),
    ) -> Result<Operation, WireError> {
        let mut feed = ProgressFeed::new(progress);
        // A recorded claim is a durable answer, independent of the live root.
        if let Some(status) = self.journal.claimed(plan).map_err(deploy_error)? {
            if let Some(event) = StepEvent::terminal(&status) {
                feed.emit(event);
            }
            return Ok(operation(status));
        }
        let selected = self.selected_pair()?;
        let binding = self.reopen(&selected)?;
        self.marker_owner(&binding)?;
        self.require_native_effects(&selected, &binding)?;
        let mut port = self.native_effects(&selected)?;
        // The marker is claimed after the plan exists and names this
        // installation, but before the first public effect.
        let stored = self
            .journal
            .plan(plan)
            .map_err(deploy_error)?
            .ok_or_else(|| WireError::new(Code::StalePlan))?;
        if stored.installation != selected.installation || stored.data != selected.data {
            return Err(WireError::new(Code::TargetUnavailable));
        }
        if let DeploymentOwner::Foreign(owner) = binding
            .claim_deployment(self.catalog.id(), &self.app_data)
            .map_err(deploy_error)?
        {
            return Err(foreign_deployment(&owner));
        }
        let committed = marduk_core::deployment::commit_deployment_with_progress(
            &mut self.journal,
            &mut port,
            plan,
            || false,
            |event| feed.emit(event),
        );
        self.clear_marker_if_idle(&binding, &selected)?;
        let status = committed.map_err(deploy_error)?;
        Ok(operation(status))
    }

    fn restore_generation(&mut self) -> Result<RestoreResult, WireError> {
        self.restore_with_progress(|_| {})
    }

    fn restore_with_progress(
        &mut self,
        progress: impl FnMut(Progress),
    ) -> Result<RestoreResult, WireError> {
        let mut feed = ProgressFeed::new(progress);
        let selected = self.selected_pair()?;
        let binding = self.reopen(&selected);
        let owner = binding
            .as_ref()
            .ok()
            .map(|binding| self.marker_owner(binding))
            .transpose()?;
        // Answer the no-work and pending cases from the journal before the
        // effects gate. A clean root is `NoActiveDeployment`, not an unavailable
        // volume, and a pending Restore reports its tracked status.
        let state = self
            .journal
            .root_state(&selected.data)
            .map_err(deploy_error)?;
        if let Some(op) = state.pending {
            self.journal
                .verify_pending_pair(op, &selected.installation, &selected.data)
                .map_err(deploy_error)?;
            let status = self
                .journal
                .status(op)
                .map_err(deploy_error)?
                .ok_or_else(|| deploy_error(DeployError::UnknownOperation))?;
            if status.kind == OperationKind::Restore {
                if let Some(event) = StepEvent::terminal(&status) {
                    feed.emit(event);
                }
                return Ok(RestoreResult::Operation {
                    operation: operation(status),
                });
            }
            return Err(deploy_error(DeployError::Busy));
        }
        if state.active.is_none() {
            if let Ok(binding) = &binding
                && matches!(owner, Some(DeploymentOwner::Owned(_)))
            {
                self.clear_marker(binding)?;
            }
            return Ok(RestoreResult::NoActiveDeployment {
                code: Code::NoActiveDeployment,
            });
        }
        let binding = binding?;
        self.require_native_effects(&selected, &binding)?;
        let mut port = self.native_effects(&selected)?;
        match marduk_core::deployment::restore_with_progress(
            &mut self.journal,
            &mut port,
            &selected.installation,
            &selected.data,
            |event| {
                if event.phase != Phase::Completed {
                    feed.emit(event);
                }
            },
        ) {
            Ok(status) => {
                if status.phase == Phase::Completed {
                    self.clear_marker(&binding)?;
                    feed.emit(StepEvent::terminal(&status).expect("completed restore"));
                }
                Ok(RestoreResult::Operation {
                    operation: operation(status),
                })
            }
            Err(DeployError::NoActiveDeployment) => Ok(RestoreResult::NoActiveDeployment {
                code: Code::NoActiveDeployment,
            }),
            Err(error) => Err(deploy_error(error)),
        }
    }

    fn selected_pair(&mut self) -> Result<SelectedInstallation, WireError> {
        self.journal
            .selected_installation()
            .map_err(deploy_error)?
            .ok_or_else(|| WireError::new(Code::TargetUnavailable))
    }

    /// The effects availability and the marker owner for one installation.
    ///
    /// A marker owned by another catalog is the dominant fact: this client may
    /// neither deploy nor restore, and the blocker names the owner.
    fn deployment_state(
        &self,
        selected: &SelectedInstallation,
        binding: &InstallationBinding,
    ) -> (Availability, Option<DeploymentInfo>) {
        match binding
            .deployment_owner(self.catalog.id())
            .map_err(deploy_error)
        {
            Ok(DeploymentOwner::Foreign(owner)) => (
                Availability {
                    available: false,
                    blocker: Some(foreign_deployment(&owner)),
                },
                Some(deployment_info(&owner)),
            ),
            Ok(owner) => {
                let blocker = self.require_native_effects(selected, binding).err();
                (
                    Availability {
                        available: blocker.is_none(),
                        blocker,
                    },
                    marker_owner(owner),
                )
            }
            Err(error) => (
                Availability {
                    available: false,
                    blocker: Some(error),
                },
                None,
            ),
        }
    }

    fn marker_owner(&self, binding: &InstallationBinding) -> Result<DeploymentOwner, WireError> {
        match binding
            .deployment_owner(self.catalog.id())
            .map_err(deploy_error)?
        {
            DeploymentOwner::Foreign(owner) => Err(foreign_deployment(&owner)),
            owner => Ok(owner),
        }
    }

    fn clear_marker(&self, binding: &InstallationBinding) -> Result<(), WireError> {
        binding
            .clear_deployment(self.catalog.id())
            .map_err(|error| {
                WireError::with_message(
                    deploy_error(error).code,
                    "could not clear the deployment marker; retry recovery",
                )
            })
    }

    fn clear_marker_if_idle(
        &mut self,
        binding: &InstallationBinding,
        selected: &SelectedInstallation,
    ) -> Result<(), WireError> {
        let state = self
            .journal
            .root_state(&selected.data)
            .map_err(deploy_error)?;
        if state.pending.is_none() && state.active.is_none() {
            self.clear_marker(binding)?;
        }
        Ok(())
    }

    /// One decision point for a new client-visible deploy or restore. A
    /// locator's volume class alone is not evidence: the recorded pair must
    /// reopen with its supported layout, both roots must sit on a proven
    /// volume, and the app and game roots must share a rename domain or the
    /// staged object cannot be renamed onto the target.
    fn require_native_effects(
        &self,
        selected: &SelectedInstallation,
        binding: &InstallationBinding,
    ) -> Result<(), WireError> {
        if !marduk_platform::deploy_volume_qualified(&self.app_data)
            || !marduk_platform::deploy_volume_qualified(&selected.locator)
        {
            return Err(unsupported_effect());
        }
        let app =
            marduk_platform::effects::directory_identity(&self.app_data).map_err(root_error)?;
        if !same_rename_domain(&app, binding.identities().0) {
            return Err(unsupported_effect());
        }
        Ok(())
    }

    fn reopen(&self, selected: &SelectedInstallation) -> Result<InstallationBinding, WireError> {
        marduk_platform::reopen_installation(
            &selected.locator,
            &skyrim_se::LAYOUT,
            &selected.installation,
            &selected.data,
        )
        .map_err(root_error)
    }

    fn native_effects(&self, selected: &SelectedInstallation) -> Result<NativeEffects, WireError> {
        NativeEffects::open(&self.app_data, &selected.locator, skyrim_se::LAYOUT)
            .map_err(deploy_error)
    }

    /// Proof-lane preview that bypasses the client availability gate.
    ///
    /// Client deploy and restore are available where the record proves the
    /// composition, but witnesses also need the same catalog, journal and
    /// adapter composition on an unqualified root to check its typed refusals
    /// and recorded answers. The adapter itself refuses an unqualified volume
    /// with a typed error, so this lane cannot simulate a write.
    #[doc(hidden)]
    pub fn proof_preview_deployment(
        &mut self,
        revision: ProfileRevision,
    ) -> Result<Preview, WireError> {
        self.deploy_preview(revision)
    }

    /// Synthetic Apply, see [`Self::proof_preview_deployment`].
    #[doc(hidden)]
    pub fn proof_apply_deployment(&mut self, plan: PlanId) -> Result<Operation, WireError> {
        self.deploy_apply(plan)
    }

    /// Synthetic Restore, see [`Self::proof_preview_deployment`].
    #[doc(hidden)]
    pub fn proof_restore(&mut self) -> Result<RestoreResult, WireError> {
        self.restore_generation()
    }

    /// Restart reconciliation behind the [`Op::Recover`] wire route.
    ///
    /// `Recover` settles an already-recorded pending operation and creates no
    /// work; the availability rule is owned by `docs/contracts/m1-clients.md`.
    fn recover_pending(&mut self) -> Result<Option<Operation>, WireError> {
        let selected = self.selected_pair()?;
        let binding = self.reopen(&selected)?;
        let owner = self.marker_owner(&binding)?;
        let state = self
            .journal
            .root_state(&selected.data)
            .map_err(deploy_error)?;
        if state.pending.is_none() {
            if state.active.is_none() && matches!(owner, DeploymentOwner::Owned(_)) {
                self.clear_marker(&binding)?;
            }
            return Ok(None);
        }
        self.require_native_effects(&selected, &binding)?;
        let mut port = self.native_effects(&selected)?;
        let recovered = marduk_core::recovery::recover_pending(
            &mut self.journal,
            &mut port,
            &selected.installation,
            &selected.data,
        );
        match recovered {
            Ok(status) => {
                // Completed recovery may have rolled the deployment back, so an
                // idle journal now means nothing is deployed and the marker is
                // this catalog's leftover.
                if matches!(owner, DeploymentOwner::Owned(_)) {
                    self.clear_marker_if_idle(&binding, &selected)?;
                }
                Ok(status.map(operation))
            }
            Err(error) => Err(deploy_error(error)),
        }
    }

    /// Synthetic restart reconciliation, see [`Self::proof_preview_deployment`].
    #[doc(hidden)]
    pub fn proof_recover_pending(&mut self) -> Result<Option<Operation>, WireError> {
        self.recover_pending()
    }

    fn operation(&mut self, args: OperationArgs) -> Result<Operation, WireError> {
        let id = marduk_core::deployment::OperationId::try_from_u64(parse_id(&args.operation_id)?)
            .map_err(|_| WireError::new(Code::InvalidRequest))?;
        let status = Journal::status(&mut self.journal, id)
            .map_err(deploy_error)?
            .ok_or_else(|| WireError::new(Code::UnknownOperation))?;
        Ok(operation(status))
    }
}

/// CLI names a path, the desktop shell names a handle. Requiring exactly one
/// keeps a renderer-supplied path out of the desktop request entirely.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetupArgs {
    #[serde(default)]
    game_dir: Option<String>,
    #[serde(default)]
    selection_id: Option<String>,
}

impl SetupArgs {
    fn selection(&self) -> Result<Selection, WireError> {
        match (&self.game_dir, &self.selection_id) {
            (Some(path), None) if !path.is_empty() => Ok(Selection::CliPath(PathBuf::from(path))),
            (None, Some(id)) => Ok(Selection::Desktop(SelectionId::parse(id)?)),
            _ => Err(WireError::new(Code::InvalidRequest)),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportPreviewArgs {
    #[serde(default)]
    zip: Option<String>,
    #[serde(default)]
    selection_id: Option<String>,
    name: String,
    at_revision: String,
}

impl ImportPreviewArgs {
    fn selection(&self) -> Result<Selection, WireError> {
        match (&self.zip, &self.selection_id) {
            (Some(path), None) if !path.is_empty() => Ok(Selection::CliPath(PathBuf::from(path))),
            (None, Some(id)) => Ok(Selection::Desktop(SelectionId::parse(id)?)),
            _ => Err(WireError::new(Code::InvalidRequest)),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportApplyArgs {
    import_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrderArgs {
    at_revision: String,
    member_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeployPreviewArgs {
    at_revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeployApplyArgs {
    plan_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationArgs {
    operation_id: String,
}

fn decode<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, WireError> {
    serde_json::from_value(args).map_err(|_| WireError::new(Code::InvalidRequest))
}

fn json<T: Serialize>(value: T) -> Result<Value, WireError> {
    serde_json::to_value(value).map_err(|_| WireError::new(Code::Storage))
}

fn decimal(value: u64) -> String {
    value.to_string()
}

fn parse_revision(value: &str) -> Result<ProfileRevision, WireError> {
    parse_decimal(value, true).map(ProfileRevision::new)
}

fn parse_id(value: &str) -> Result<u64, WireError> {
    parse_decimal(value, false)
}

fn parse_decimal(value: &str, allow_zero: bool) -> Result<u64, WireError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(WireError::new(Code::InvalidRequest));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(WireError::new(Code::InvalidRequest));
    }
    let parsed: u64 = value
        .parse()
        .map_err(|_| WireError::new(Code::InvalidRequest))?;
    if (!allow_zero && parsed == 0) || parsed > i64::MAX as u64 {
        return Err(WireError::new(Code::InvalidRequest));
    }
    Ok(parsed)
}

fn label(locator: &Path) -> String {
    locator
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("installation")
        .to_owned()
}

fn snapshot_dto(named: NamedProfile) -> Snapshot {
    let NamedProfile { snapshot, names } = named;
    let members = snapshot
        .members
        .into_iter()
        .map(|member| {
            let display_name = names
                .get(&member.id)
                .cloned()
                .unwrap_or_else(|| decimal(member.id.get()));
            Member {
                id: decimal(member.id.get()),
                installed_id: decimal(member.installed.get()),
                enabled: member.enabled,
                display_name,
            }
        })
        .collect();
    Snapshot {
        id: decimal(snapshot.id.get()),
        revision: decimal(snapshot.revision.get()),
        members,
    }
}

fn prepared_dto(prepared: PreparedImport) -> Prepared {
    Prepared {
        id: decimal(prepared.id.get()),
        profile_id: decimal(prepared.profile.get()),
        expected_revision: decimal(prepared.expected_revision.get()),
        archive_sha256: hex(&prepared.archive_sha256),
        files: prepared
            .files
            .into_iter()
            .map(|file| Target {
                role: Role::Data,
                path: file.relative.as_str().to_owned(),
            })
            .collect(),
    }
}

fn reconciliation(report: ReconcileReport) -> Reconciliation {
    let quarantined: Vec<QuarantinedLeftover> = report
        .quarantined
        .into_iter()
        .map(|entry| QuarantinedLeftover {
            id: decimal(entry.id.get()),
            reason: quarantine_reason(entry.reason).to_owned(),
        })
        .collect();
    Reconciliation {
        quarantined_count: quarantined.len(),
        reclaimed: report
            .reclaimed
            .into_iter()
            .map(|id| decimal(id.get()))
            .collect(),
        quarantined,
    }
}

/// Stable reason tokens for the client contract.
fn quarantine_reason(reason: QuarantineReason) -> &'static str {
    match reason {
        QuarantineReason::OwnershipMissing => "ownership_missing",
        QuarantineReason::StageChanged => "stage_changed",
        QuarantineReason::StageUnavailable => "stage_unavailable",
        QuarantineReason::Published => "published",
    }
}

fn operation(status: OperationStatus) -> Operation {
    Operation {
        id: decimal(status.id.get()),
        kind: match status.kind {
            OperationKind::Deploy => Kind::Deploy,
            OperationKind::Restore => Kind::Restore,
        },
        phase: phase(status.phase).to_owned(),
        generation_id: status.generation.map(|value| decimal(value.get())),
        blocker: status.blocker.map(|error| {
            let error = deploy_error(error);
            Blocker {
                code: error.code,
                detail: error.detail,
                message: error.message,
            }
        }),
        retained: status.retained.into_iter().filter_map(retained).collect(),
    }
}

fn retained(retained: BackupRef) -> Option<Retained> {
    let slot = match retained.slot() {
        BackupSlot::Original => Slot::Original,
        BackupSlot::Step => Slot::Step,
        BackupSlot::Displaced => Slot::Displaced,
        BackupSlot::AlternateDisplaced => Slot::Alternate,
        BackupSlot::StageDisplaced => Slot::Stage,
    };
    Some(Retained {
        operation_id: decimal(retained.operation().get()),
        ordinal: retained.ordinal(),
        direction: match retained.direction() {
            StepDirection::Forward => Direction::Forward,
            StepDirection::Undo => Direction::Undo,
        },
        slot,
    })
}

fn phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Preparing => "preparing",
        Phase::Prepared => "prepared",
        Phase::Applying => "applying",
        Phase::RollingBack => "rolling_back",
        Phase::Restoring => "restoring",
        Phase::Completed => "completed",
        Phase::RolledBack => "rolled_back",
        Phase::Cancelled => "cancelled",
    }
}

/// The same rename-domain comparison the effect adapter applies to its stage
/// and target parents: matching filesystem magic and id, plus the Btrfs tree
/// when present. Windows compares the volume serial. The mount view is a
/// session property and is not part of the comparison.
fn same_rename_domain(app: &RootIdentity, installation: &RootIdentity) -> bool {
    match (app.evidence(), installation.evidence()) {
        (
            NativeIdentity::Linux {
                fs_magic: app_magic,
                fs_id: app_id,
                tree_id: app_tree,
                ..
            },
            NativeIdentity::Linux {
                fs_magic: installation_magic,
                fs_id: installation_id,
                tree_id: installation_tree,
                ..
            },
        ) => {
            app_magic == installation_magic
                && app_id == installation_id
                && app_tree == installation_tree
        }
        (
            NativeIdentity::Windows {
                volume_serial: app_volume,
                ..
            },
            NativeIdentity::Windows {
                volume_serial: installation_volume,
                ..
            },
        ) => app_volume == installation_volume,
        _ => false,
    }
}

/// No proven client deploy route exists for this configuration, so no plan can
/// be bound, no game file can be observed and no restore can run through
/// `dispatch`.
fn unsupported_effect() -> WireError {
    WireError::with_detail(
        Code::UnsupportedTarget,
        Detail::Reason {
            reason: ReasonDto::Configuration,
        },
    )
}

/// The typed refusal for a marker owned by another catalog root. The message
/// names the owner so a client can say "deployed by another configuration"
/// instead of showing a bare Busy.
fn foreign_deployment(owner: &Path) -> WireError {
    WireError::with_message(
        Code::ActiveDeployment,
        format!(
            "this installation is deployed by another configuration: {}",
            owner.display()
        ),
    )
}

fn deployment_info(owner: &Path) -> DeploymentInfo {
    DeploymentInfo {
        owner: owner.display().to_string(),
    }
}

fn marker_owner(owner: DeploymentOwner) -> Option<DeploymentInfo> {
    match owner {
        DeploymentOwner::Vacant => None,
        DeploymentOwner::Owned(owner) | DeploymentOwner::Foreign(owner) => {
            Some(deployment_info(&owner))
        }
    }
}

fn import_error(error: ImportError) -> WireError {
    match error {
        ImportError::InvalidName => WireError::new(Code::InvalidName),
        ImportError::StalePlan => WireError::new(Code::StalePlan),
        ImportError::MemberNotFound => WireError::new(Code::MemberNotFound),
        ImportError::Busy => WireError::new(Code::Busy),
        ImportError::UnknownImport => WireError::new(Code::UnknownImport),
        ImportError::ImportNotPrepared => WireError::new(Code::ImportNotPrepared),
        ImportError::StageChanged => WireError::new(Code::StageChanged),
        ImportError::StageUnavailable => WireError::new(Code::StageUnavailable),
        ImportError::Archive(error) => archive_error(error),
        ImportError::Storage => WireError::new(Code::Storage),
    }
}

fn edit_error(error: marduk_core::import::ProfileEditError) -> WireError {
    use marduk_core::import::ProfileEditError;
    match error {
        ProfileEditError::StalePlan => WireError::new(Code::StalePlan),
        ProfileEditError::InvalidOrder => WireError::new(Code::InvalidOrder),
        ProfileEditError::Busy => WireError::new(Code::Busy),
        ProfileEditError::Storage => WireError::new(Code::Storage),
    }
}

fn deploy_error(error: DeployError) -> WireError {
    match error {
        DeployError::StalePlan => WireError::new(Code::StalePlan),
        DeployError::ActiveDeployment => WireError::new(Code::ActiveDeployment),
        DeployError::Busy => WireError::new(Code::Busy),
        DeployError::TargetUnavailable => WireError::new(Code::TargetUnavailable),
        DeployError::UnsupportedTarget => WireError::new(Code::UnsupportedTarget),
        DeployError::InvalidPath => WireError::new(Code::InvalidPath),
        DeployError::RecoveryConflict => WireError::new(Code::RecoveryConflict),
        DeployError::UnknownOperation => WireError::new(Code::UnknownOperation),
        DeployError::NoActiveDeployment => WireError::new(Code::NoActiveDeployment),
        DeployError::UnsupportedEvidenceVersion => WireError::new(Code::Storage),
        DeployError::Io => WireError::new(Code::Io),
    }
}

fn root_error(error: RootError) -> WireError {
    match error {
        RootError::TargetUnavailable => WireError::new(Code::TargetUnavailable),
        RootError::MissingParent => WireError::with_detail(
            Code::InvalidPath,
            Detail::Reason {
                reason: ReasonDto::MissingParent,
            },
        ),
        RootError::AmbiguousName => WireError::with_detail(
            Code::InvalidPath,
            Detail::Reason {
                reason: ReasonDto::AmbiguousName,
            },
        ),
        RootError::OutsideBoundary => WireError::with_detail(
            Code::InvalidPath,
            Detail::Reason {
                reason: ReasonDto::OutsideBoundary,
            },
        ),
        RootError::UnsupportedTarget(reason) => WireError::with_detail(
            Code::UnsupportedTarget,
            Detail::Reason {
                reason: reason_dto(reason),
            },
        ),
        RootError::Io => WireError::new(Code::Io),
    }
}

fn reason_dto(reason: UnsupportedReason) -> ReasonDto {
    match reason {
        UnsupportedReason::ObjectKind => ReasonDto::ObjectKind,
        UnsupportedReason::Metadata => ReasonDto::Metadata,
        UnsupportedReason::Sharing => ReasonDto::Sharing,
        UnsupportedReason::Configuration => ReasonDto::Configuration,
    }
}

fn archive_error(error: ArchiveError) -> WireError {
    match error {
        ArchiveError::InvalidContainer => WireError::new(Code::InvalidContainer),
        ArchiveError::UnsupportedContainer => WireError::new(Code::UnsupportedContainer),
        ArchiveError::DuplicateRawName => WireError::new(Code::DuplicateRawName),
        ArchiveError::UnsupportedEntry { index } => {
            WireError::with_detail(Code::UnsupportedEntry, Detail::Index { index })
        }
        ArchiveError::UnsafeName { index } => {
            WireError::with_detail(Code::UnsafeName, Detail::Index { index })
        }
        ArchiveError::ConflictingNames { first, second } => {
            WireError::with_detail(Code::ConflictingNames, Detail::Pair { first, second })
        }
        ArchiveError::LimitExceeded { kind } => WireError::with_detail(
            Code::LimitExceeded,
            Detail::Kind {
                kind: match kind {
                    LimitKind::ArchiveBytes => LimitKindDto::ArchiveBytes,
                    LimitKind::DirectoryBytes => LimitKindDto::DirectoryBytes,
                    LimitKind::Entries => LimitKindDto::Entries,
                    LimitKind::FileBytes => LimitKindDto::FileBytes,
                    LimitKind::TotalBytes => LimitKindDto::TotalBytes,
                },
            },
        ),
        ArchiveError::CorruptEntry { index } => {
            WireError::with_detail(Code::CorruptEntry, Detail::Index { index })
        }
        ArchiveError::MissingEntry { index } => {
            WireError::with_detail(Code::MissingEntry, Detail::Index { index })
        }
        ArchiveError::Io => WireError::new(Code::Io),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(slot: BackupSlot) -> BackupRef {
        BackupRef::from_record(
            marduk_core::deployment::OperationId::try_from_u64(7).unwrap(),
            2,
            StepDirection::Undo,
            slot,
        )
    }

    fn event(operation: OperationId, phase: Phase, completed: u64, total: u64) -> StepEvent {
        StepEvent {
            operation,
            phase,
            completed,
            total,
        }
    }

    #[test]
    fn progress_feed_numbers_one_operation_and_drops_foreign_events() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let operation = OperationId::try_from_u64(9).unwrap();
        let foreign = OperationId::try_from_u64(10).unwrap();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let mut feed = ProgressFeed::new(move |progress| sink.borrow_mut().push(progress));

        for event in [
            event(operation, Phase::Preparing, 0, 2),
            event(foreign, Phase::Applying, 1, 2),
            event(operation, Phase::Applying, 1, 2),
            event(operation, Phase::Applying, 2, 2),
            event(operation, Phase::Completed, 2, 2),
        ] {
            feed.emit(event);
        }

        let seen = seen.borrow();
        assert_eq!(
            seen.iter()
                .map(|progress| progress.operation_id.as_str())
                .collect::<Vec<_>>(),
            ["9", "9", "9", "9"],
            "every hint names this call's operation"
        );
        assert_eq!(
            seen.iter()
                .map(|progress| progress.sequence.as_str())
                .collect::<Vec<_>>(),
            ["1", "2", "3", "4"],
            "sequence is monotonic from 1 and a foreign event consumes none"
        );
        assert_eq!(
            seen.iter()
                .map(|progress| progress.phase.as_str())
                .collect::<Vec<_>>(),
            ["preparing", "applying", "applying", "completed"]
        );
    }

    #[test]
    fn a_sink_that_stops_accepting_cannot_change_what_the_feed_reports() {
        use std::cell::Cell;
        use std::rc::Rc;

        // A client whose channel is gone stops accepting after the first hint.
        let calls = Rc::new(Cell::new(0usize));
        let recorded = Rc::new(Cell::new(0usize));
        let sink_calls = Rc::clone(&calls);
        let sink_recorded = Rc::clone(&recorded);
        let mut feed = ProgressFeed::new(move |_| {
            sink_calls.set(sink_calls.get() + 1);
            if sink_calls.get() == 1 {
                sink_recorded.set(1);
            }
            // Later sends fail in the desktop adapter; the feed has nothing to
            // propagate and must keep reporting the operation's transitions.
        });
        let operation = OperationId::try_from_u64(11).unwrap();

        feed.emit(event(operation, Phase::Preparing, 0, 2));
        feed.emit(event(operation, Phase::Applying, 1, 2));
        feed.emit(event(operation, Phase::Completed, 2, 2));

        // Every transition still reached the sink; the sink's own refusal to
        // record one is not an operation failure and is never retried.
        assert_eq!(calls.get(), 3);
        assert_eq!(recorded.get(), 1);
    }

    #[test]
    fn code_strings_serialize_through_one_table() {
        let codes = [
            Code::InvalidRequest,
            Code::InvalidName,
            Code::InvalidOrder,
            Code::StalePlan,
            Code::MemberNotFound,
            Code::Busy,
            Code::UnknownImport,
            Code::ImportNotPrepared,
            Code::StageChanged,
            Code::StageUnavailable,
            Code::Storage,
            Code::InvalidContainer,
            Code::UnsupportedContainer,
            Code::DuplicateRawName,
            Code::UnsupportedEntry,
            Code::UnsafeName,
            Code::ConflictingNames,
            Code::LimitExceeded,
            Code::CorruptEntry,
            Code::MissingEntry,
            Code::InvalidPath,
            Code::TargetUnavailable,
            Code::UnsupportedTarget,
            Code::ActiveDeployment,
            Code::RecoveryConflict,
            Code::UnknownOperation,
            Code::NoActiveDeployment,
            Code::Io,
        ];
        for code in codes {
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                Value::String(code.as_str().to_owned())
            );
            assert_eq!(
                serde_json::from_value::<Code>(serde_json::to_value(code).unwrap()).unwrap(),
                code
            );
        }
    }

    #[test]
    fn envelope_ok_is_derived_from_the_variant() {
        assert_eq!(
            serde_json::to_value(Envelope::ok(7)).unwrap(),
            serde_json::json!({ "ok": true, "result": 7 })
        );
        assert_eq!(
            serde_json::to_value(Envelope::<()>::error(WireError::new(Code::StalePlan))).unwrap(),
            serde_json::json!({ "ok": false, "error": { "code": "StalePlan" } })
        );
    }

    #[test]
    fn order_failures_keep_their_stable_codes() {
        use marduk_core::import::ProfileEditError;
        assert_eq!(
            edit_error(ProfileEditError::InvalidOrder),
            WireError::new(Code::InvalidOrder)
        );
        assert_eq!(
            edit_error(ProfileEditError::StalePlan),
            WireError::new(Code::StalePlan)
        );
    }

    #[test]
    fn reconciliation_projects_ids_and_stable_reason_tokens() {
        use marduk_storage::library::QuarantinedImport;
        let id = |value| ImportId::try_from_u64(value).unwrap();
        let report = ReconcileReport {
            prepared: Vec::new(),
            reclaimed: vec![id(7)],
            quarantined: vec![
                QuarantinedImport {
                    id: id(8),
                    reason: QuarantineReason::OwnershipMissing,
                },
                QuarantinedImport {
                    id: id(9),
                    reason: QuarantineReason::StageChanged,
                },
                QuarantinedImport {
                    id: id(10),
                    reason: QuarantineReason::StageUnavailable,
                },
                QuarantinedImport {
                    id: id(11),
                    reason: QuarantineReason::Published,
                },
            ],
        };

        let dto = reconciliation(report);
        assert_eq!(dto.reclaimed, ["7"]);
        assert_eq!(dto.quarantined_count, 4);
        assert_eq!(
            dto.quarantined
                .iter()
                .map(|entry| (entry.id.as_str(), entry.reason.as_str()))
                .collect::<Vec<_>>(),
            [
                ("8", "ownership_missing"),
                ("9", "stage_changed"),
                ("10", "stage_unavailable"),
                ("11", "published"),
            ]
        );
    }

    #[test]
    fn alternate_displacement_is_a_distinct_reported_location() {
        let dto = retained(backup(BackupSlot::AlternateDisplaced)).unwrap();
        assert_eq!(
            serde_json::to_value(dto).unwrap()["slot"],
            serde_json::Value::String("alternate".into())
        );
        for (slot, expected) in [
            (BackupSlot::Original, Slot::Original),
            (BackupSlot::Step, Slot::Step),
            (BackupSlot::Displaced, Slot::Displaced),
        ] {
            let dto = retained(backup(slot)).unwrap();
            assert_eq!(
                serde_json::to_value(dto).unwrap()["slot"],
                serde_json::to_value(expected).unwrap()
            );
        }
    }

    #[test]
    fn stage_displacement_is_reported_as_stage() {
        let dto = retained(backup(BackupSlot::StageDisplaced)).unwrap();
        assert_eq!(serde_json::to_value(dto).unwrap()["slot"], "stage");
    }

    #[test]
    fn app_data_root_is_the_identifier_under_the_os_data_directory() {
        let root = app_data_root_in(Path::new("/var/lib/user-data"));
        assert_eq!(root, Path::new("/var/lib/user-data").join(APP_IDENTIFIER));
        assert_eq!(root.file_name().unwrap(), APP_IDENTIFIER);
    }
}

/// Rust-generated client wire fixtures.
///
/// The desktop transport tests parse these files, so the committed JSON is
/// produced by the same serialization the commands return. Regenerate with
/// `MDUK_WRITE_WIRE_FIXTURES=1 cargo test -p marduk-workspace wire_fixture`
/// after changing a DTO, then rerun without the variable to verify the check.
#[cfg(test)]
mod wire_fixture {
    use super::*;

    const LARGE_A: &str = "9007199254740993";
    const LARGE_B: &str = "9007199254740995";
    const LARGE_C: &str = "9007199254740997";
    const LARGE_D: &str = "9007199254740999";
    const LARGE_E: &str = "9007199254741001";
    const LARGE_F: &str = "9007199254741003";
    const REVISION: &str = "9223372036854775807";

    fn fixtures() -> Vec<(&'static str, Value)> {
        vec![
            ("status.json", ok(status_fixture())),
            ("snapshot.json", ok(snapshot_fixture())),
            ("operation.json", ok(operation_fixture())),
            ("prepared.json", ok(prepared_fixture())),
            ("preview.json", ok(preview_fixture())),
            (
                "progress.json",
                serde_json::to_value(progress_fixture()).unwrap(),
            ),
            (
                "restore-no-active.json",
                ok(RestoreResult::NoActiveDeployment {
                    code: Code::NoActiveDeployment,
                }),
            ),
            (
                "error-stale-plan.json",
                serde_json::to_value(Envelope::<()>::error(WireError::new(Code::StalePlan)))
                    .unwrap(),
            ),
            (
                "error-invalid-path.json",
                serde_json::to_value(Envelope::<()>::error(WireError::with_detail(
                    Code::InvalidPath,
                    Detail::Reason {
                        reason: ReasonDto::MissingParent,
                    },
                )))
                .unwrap(),
            ),
            (
                "error-limit.json",
                serde_json::to_value(Envelope::<()>::error(WireError::with_detail(
                    Code::LimitExceeded,
                    Detail::Kind {
                        kind: LimitKindDto::Entries,
                    },
                )))
                .unwrap(),
            ),
        ]
    }

    fn ok<T: Serialize>(result: T) -> Value {
        serde_json::to_value(Envelope::ok(result)).unwrap()
    }

    fn snapshot_fixture() -> Snapshot {
        Snapshot {
            id: LARGE_B.to_owned(),
            revision: REVISION.to_owned(),
            members: vec![
                Member {
                    id: LARGE_C.to_owned(),
                    installed_id: LARGE_D.to_owned(),
                    enabled: true,
                    display_name: "Base textures".to_owned(),
                },
                Member {
                    id: LARGE_E.to_owned(),
                    installed_id: LARGE_F.to_owned(),
                    enabled: false,
                    display_name: "Weather overhaul".to_owned(),
                },
            ],
        }
    }

    fn operation_fixture() -> Operation {
        Operation {
            id: "9223372036854775801".to_owned(),
            kind: Kind::Restore,
            phase: "completed".to_owned(),
            generation_id: Some("9223372036854775802".to_owned()),
            blocker: None,
            retained: vec![
                Retained {
                    operation_id: "9223372036854775701".to_owned(),
                    ordinal: 3,
                    direction: Direction::Undo,
                    slot: Slot::Displaced,
                },
                Retained {
                    operation_id: "9223372036854775702".to_owned(),
                    ordinal: 4,
                    direction: Direction::Forward,
                    slot: Slot::Alternate,
                },
            ],
        }
    }

    fn status_fixture() -> WorkspaceStatus {
        WorkspaceStatus {
            installation: Some(Installation {
                id: LARGE_A.to_owned(),
                label: "Skyrim Special Edition".to_owned(),
            }),
            profile: snapshot_fixture(),
            availability: AvailabilitySet {
                target: Availability {
                    available: true,
                    blocker: None,
                },
                import: Availability {
                    available: false,
                    blocker: Some(WireError::new(Code::StageUnavailable)),
                },
                deploy: Availability {
                    available: false,
                    blocker: Some(WireError::with_detail(
                        Code::UnsupportedTarget,
                        Detail::Reason {
                            reason: ReasonDto::Configuration,
                        },
                    )),
                },
                restore: Availability {
                    available: false,
                    blocker: Some(WireError::with_detail(
                        Code::UnsupportedTarget,
                        Detail::Reason {
                            reason: ReasonDto::Configuration,
                        },
                    )),
                },
            },
            pending: Some(Operation {
                id: "9223372036854775803".to_owned(),
                kind: Kind::Deploy,
                phase: "applying".to_owned(),
                generation_id: Some("9223372036854775804".to_owned()),
                blocker: Some(WireError::new(Code::RecoveryConflict)),
                retained: vec![Retained {
                    operation_id: "9223372036854775805".to_owned(),
                    ordinal: 2,
                    direction: Direction::Forward,
                    slot: Slot::Original,
                }],
            }),
            active_generation_id: Some("9223372036854775804".to_owned()),
            deployment: Some(DeploymentInfo {
                owner: "/home/user/.local/share/com.marduk.desktop".to_owned(),
            }),
            reconciliation: Reconciliation {
                reclaimed: vec![LARGE_B.to_owned()],
                quarantined: vec![QuarantinedLeftover {
                    id: LARGE_C.to_owned(),
                    reason: "stage_changed".to_owned(),
                }],
                quarantined_count: 1,
            },
        }
    }

    fn prepared_fixture() -> Prepared {
        Prepared {
            id: "9223372036854775702".to_owned(),
            profile_id: "1".to_owned(),
            expected_revision: "42".to_owned(),
            archive_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_owned(),
            files: vec![
                Target {
                    role: Role::Data,
                    path: "textures/winner.dds".to_owned(),
                },
                Target {
                    role: Role::Data,
                    path: "meshes/only-a.nif".to_owned(),
                },
            ],
        }
    }

    fn progress_fixture() -> Progress {
        Progress {
            operation_id: "9223372036854775801".to_owned(),
            sequence: "9007199254741009".to_owned(),
            phase: "applying".to_owned(),
        }
    }

    fn preview_fixture() -> Preview {
        Preview {
            id: Some("9223372036854775703".to_owned()),
            profile_id: "1".to_owned(),
            revision: "42".to_owned(),
            installation_id: LARGE_A.to_owned(),
            label: "Skyrim Special Edition".to_owned(),
            files: vec![PreviewFile {
                target: Target {
                    role: Role::Data,
                    path: "textures/winner.dds".to_owned(),
                },
                winner: LARGE_E.to_owned(),
                candidates: vec![LARGE_C.to_owned(), LARGE_E.to_owned()],
                needs_backup: true,
            }],
            blockers: vec![
                WireError::with_detail(
                    Code::InvalidPath,
                    Detail::Reason {
                        reason: ReasonDto::MissingParent,
                    },
                ),
                WireError::new(Code::TargetUnavailable),
            ],
        }
    }

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src/transport/fixtures")
    }

    #[test]
    fn committed_wire_fixtures_are_rust_serialized() {
        let dir = fixture_dir();
        let write = std::env::var_os("MDUK_WRITE_WIRE_FIXTURES").is_some();
        if write {
            std::fs::create_dir_all(&dir).unwrap();
        }
        for (name, value) in fixtures() {
            let path = dir.join(name);
            if write {
                let text = serde_json::to_string_pretty(&value).unwrap();
                std::fs::write(&path, format!("{text}\n")).unwrap();
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!(
                    "missing {name}; run MDUK_WRITE_WIRE_FIXTURES=1 \
                     cargo test -p marduk-workspace committed_wire_fixtures_are_rust_serialized"
                )
            });
            let committed: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(committed, value, "{name} drifted from the Rust DTOs");
        }
    }

    #[test]
    fn preview_fixture_names_members_not_installed_revisions() {
        let dir = fixture_dir();
        let read = |name: &str| -> Value {
            let text = std::fs::read_to_string(dir.join(name)).unwrap();
            serde_json::from_str::<Value>(&text).unwrap()["result"].clone()
        };
        let preview: Preview = serde_json::from_value(read("preview.json")).unwrap();
        let snapshot: Snapshot = serde_json::from_value(read("snapshot.json")).unwrap();
        let members: std::collections::BTreeSet<&str> = snapshot
            .members
            .iter()
            .map(|member| member.id.as_str())
            .collect();
        let installed: std::collections::BTreeSet<&str> = snapshot
            .members
            .iter()
            .map(|member| member.installed_id.as_str())
            .collect();
        for file in &preview.files {
            assert!(
                members.contains(file.winner.as_str()),
                "{} is not a member id",
                file.winner
            );
            for candidate in &file.candidates {
                assert!(
                    members.contains(candidate.as_str()),
                    "{candidate} is not a member id"
                );
                assert!(
                    !installed.contains(candidate.as_str()),
                    "{candidate} is an installed revision id"
                );
            }
        }
    }

    #[test]
    fn fixture_results_deserialize_into_client_dtos() {
        let result = |value: Value| value["result"].clone();

        let status: WorkspaceStatus = serde_json::from_value(result(ok(status_fixture()))).unwrap();
        assert_eq!(status.profile.revision, REVISION);
        assert_eq!(status.profile.members[0].id, LARGE_C);
        assert_eq!(status.pending.unwrap().retained[0].ordinal, 2);

        let _: Snapshot = serde_json::from_value(result(ok(snapshot_fixture()))).unwrap();
        let _: Operation = serde_json::from_value(result(ok(operation_fixture()))).unwrap();
        let _: Prepared = serde_json::from_value(result(ok(prepared_fixture()))).unwrap();
        let _: Preview = serde_json::from_value(result(ok(preview_fixture()))).unwrap();
        let progress: Progress = serde_json::from_value(result(ok(progress_fixture()))).unwrap();
        assert_eq!(progress.sequence, "9007199254741009");
        let restore: RestoreResult =
            serde_json::from_value(result(ok(RestoreResult::NoActiveDeployment {
                code: Code::NoActiveDeployment,
            })))
            .unwrap();
        assert!(matches!(restore, RestoreResult::NoActiveDeployment { .. }));
    }
}
