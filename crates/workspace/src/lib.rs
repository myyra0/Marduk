//! Shared client assembly for the CLI and the desktop shell.
//!
//! One [`Workspace`] owns the catalog, the operation journal and the platform
//! binding surface. Clients send typed [`Request`] values and receive one
//! [`Envelope`]; no storage, native or UI type crosses the wire.
//!
//! Operations that would need a qualified native effect port are refused with
//! a typed unavailable reason. They never report simulated success.

use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};

mod selection;

pub use selection::{Purpose, Selection, SelectionId};
use selection::{SelectionStore, Source};

use marduk_core::deployment::{DeployError, OperationKind, OperationStatus, Phase};
use marduk_core::ids::{MemberId, ProfileId, ProfileRevision};
use marduk_core::import::NamedProfile;
use marduk_core::import::{
    ArchiveError, BeginImport, ImportError, ImportId, ImportTarget, LimitKind, PreparedImport,
};
use marduk_core::recovery::{BackupRef, BackupSlot, Journal, StepDirection};
use marduk_core::root_identity::{RootError, UnsupportedReason};
use marduk_games::skyrim_se;
use marduk_storage::Catalog;
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
}

impl WireError {
    pub fn new(code: Code) -> Self {
        Self { code, detail: None }
    }

    pub fn with_detail(code: Code, detail: Detail) -> Self {
        Self {
            code,
            detail: Some(detail),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    pub installation: Option<Installation>,
    pub profile: Snapshot,
    pub availability: AvailabilitySet,
    pub pending: Option<Operation>,
    pub active_generation_id: Option<String>,
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

/// One shared client service over the authoritative catalog and journal.
pub struct Workspace {
    catalog: Catalog,
    journal: OperationJournal,
    selections: SelectionStore,
}

impl Workspace {
    /// Open the shared state under the application data root.
    pub fn open(app_data: &Path) -> Result<Self, WireError> {
        let catalog = Catalog::open(app_data).map_err(import_error)?;
        let journal = OperationJournal::open(app_data).map_err(deploy_error)?;
        Ok(Self {
            catalog,
            journal,
            selections: SelectionStore::new(),
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
                let resolved =
                    std::fs::canonicalize(&path).map_err(|_| WireError::new(Code::InvalidPath))?;
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
                let _ = parse_revision(&args.at_revision)?;
                Err(unsupported_effect())
            }
            Op::DeployApply => {
                let args: DeployApplyArgs = decode(request.args)?;
                let _ = parse_id(&args.plan_id)?;
                Err(unsupported_effect())
            }
            Op::Restore => {
                let _: NoArgs = decode(request.args)?;
                Err(unsupported_effect())
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

        if let Some(selected) = selected {
            let SelectedInstallation {
                id,
                locator,
                installation: expected_installation,
                data,
                pending: pending_status,
                active_generation,
            } = selected;

            installation = Some(Installation {
                id: decimal(id),
                label: label(&locator),
            });
            active_generation_id = active_generation.map(decimal);
            pending = pending_status.map(operation);
            target = match marduk_platform::reopen_installation(
                &locator,
                &skyrim_se::LAYOUT,
                &expected_installation,
                &data,
            ) {
                Ok(_) => Availability {
                    available: true,
                    blocker: None,
                },
                Err(error) => Availability {
                    available: false,
                    blocker: Some(root_error(error)),
                },
            };
        }

        Ok(WorkspaceStatus {
            installation,
            profile: snapshot_dto(named),
            availability: AvailabilitySet {
                target,
                import: Availability {
                    available: false,
                    blocker: Some(WireError::new(Code::StageUnavailable)),
                },
                deploy: effect_availability(),
                restore: effect_availability(),
            },
            pending,
            active_generation_id,
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
        let _ = ImportId::try_from_u64(parse_id(&args.import_id)?)
            .map_err(|_| WireError::new(Code::InvalidRequest))?;
        // No import can have been prepared while the native stage is closed.
        Err(WireError::new(Code::StageUnavailable))
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

fn effect_availability() -> Availability {
    Availability {
        available: false,
        blocker: Some(WireError::with_detail(
            Code::UnsupportedTarget,
            Detail::Reason {
                reason: ReasonDto::Configuration,
            },
        )),
    }
}

/// No native effect port is connected, so no plan can be bound, no game file
/// can be observed and no restore can run. The request is typed unavailable.
fn unsupported_effect() -> WireError {
    WireError::with_detail(
        Code::UnsupportedTarget,
        Detail::Reason {
            reason: ReasonDto::Configuration,
        },
    )
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
