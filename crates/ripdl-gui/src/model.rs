//! Desktop GUI view-model types, sorting/filtering, validation, and onboarding state.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ripdl_core::categories::categorize;
use ripdl_core::engine::UplinkNic;
use ripdl_core::lan::LanPeer;
use ripdl_core::rpc::{
    DaemonSettings, DuplicatePolicy, ExtensionStatus, LanInfo, LanSendInfo, TaskDetailsSnapshot,
    TaskInfo, MAX_CONCURRENT_LIMIT, MAX_SEGMENTS_LIMIT,
};
use ripdl_core::task::{TaskKind, TaskState};
use ripdl_core::torrent::{TorrentDetailsSnapshot, TorrentPreviewSnapshot};
use ripdl_core::update::UpdateInfo;

use crate::ext_host::BrowserChoice;

pub const RATE_HISTORY_CAP: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarFilter {
    All,
    Active,
    PausedOrExpired,
    Completed,
    Torrents,
    Bundles,
    Streams,
    Category(ripdl_core::categories::Category),
}

impl SidebarFilter {
    pub fn label(self) -> &'static str {
        use ripdl_core::categories::Category;
        match self {
            Self::All => "All Downloads",
            Self::Active => "Active",
            Self::PausedOrExpired => "Paused & Expired",
            Self::Completed => "Completed",
            Self::Torrents => "Torrents",
            Self::Bundles => "Bundles",
            Self::Streams => "Video Streams",
            Self::Category(c) => match c {
                Category::Video => "Video",
                Category::Audio => "Music",
                Category::Image => "Images",
                Category::Document => "Documents",
                Category::Archive => "Archives",
                Category::Program => "Programs",
                Category::Other => "Other",
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Kind,
    Size,
    Progress,
    Status,
    Rate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub kind: String,
    pub source: String,
    pub dest_dir: Option<PathBuf>,
    pub dest_file: Option<PathBuf>,
    pub raw_state: TaskState,
    pub state: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percent: Option<f64>,
    pub speed_bps: Option<f64>,
    pub uploaded: u64,
    pub upload_bps: Option<f64>,
    pub seeding: bool,
    pub verifying: bool,
    pub peers: u32,
    pub start_at: Option<i64>,
    pub parts_done: u32,
    pub parts_total: u32,
    pub parts_missing: Vec<u32>,
    pub stale_reason: Option<String>,
    pub error: Option<String>,
    pub category: ripdl_core::categories::Category,
}

impl TaskRow {
    pub fn from_rpc(t: TaskInfo) -> Self {
        let now_s = now_unix();
        let name = t
            .dest_file
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .or_else(|| fallback_name_from_url(&t.url))
            .unwrap_or_else(|| t.url.clone());
        let category = categorize(&name);
        let kind = match t.kind {
            TaskKind::Http => "http",
            TaskKind::Video => "video",
            TaskKind::Torrent => "torrent",
            TaskKind::Bundle => "bundle",
        }
        .to_string();
        let state = derive_state_label(&t, now_s);
        let speed_bps = if t.state == TaskState::Running && !t.verifying {
            t.speed_bps
        } else {
            None
        };
        let upload_bps = if t.seeding { t.upload_bps } else { None };
        Self {
            id: t.id,
            name,
            source: host_of(&t.url),
            url: t.url,
            kind,
            dest_dir: t.dest_dir,
            dest_file: t.dest_file,
            raw_state: t.state,
            state,
            downloaded: t.downloaded,
            total: t.total,
            percent: t.percent,
            speed_bps,
            uploaded: t.uploaded,
            upload_bps,
            seeding: t.seeding,
            verifying: t.verifying,
            peers: t.peers,
            start_at: t.start_at,
            parts_done: t.parts_done,
            parts_total: t.parts_total,
            parts_missing: t.parts_missing,
            stale_reason: t.stale_reason,
            error: t.error,
            category,
        }
    }

    pub fn ratio(&self) -> f32 {
        if let Some(pct) = self.percent {
            return (pct as f32 / 100.0).clamp(0.0, 1.0);
        }
        if self.raw_state == TaskState::Completed && self.total.is_none() {
            return 1.0;
        }
        match self.total {
            Some(total) if total > 0 => (self.downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.raw_state, TaskState::Running | TaskState::Queued) || self.seeding
    }

    pub fn is_paused_or_expired(&self) -> bool {
        matches!(
            self.raw_state,
            TaskState::Paused | TaskState::Stale | TaskState::Failed | TaskState::Cancelled
        )
    }

    pub fn can_pause_or_stop(&self) -> bool {
        matches!(self.raw_state, TaskState::Running | TaskState::Queued) || self.seeding
    }

    pub fn can_resume(&self) -> bool {
        matches!(
            self.raw_state,
            TaskState::Paused | TaskState::Failed | TaskState::Stale | TaskState::Cancelled
        ) || (self.kind == "torrent" && self.raw_state == TaskState::Completed && !self.seeding)
    }

    pub fn can_refresh_url(&self) -> bool {
        (self.kind == "http" || self.kind == "bundle") && self.raw_state != TaskState::Completed
    }
}

fn derive_state_label(t: &TaskInfo, now_s: i64) -> String {
    if t.state == TaskState::Queued && t.start_at.is_some_and(|s| s > now_s) {
        return "timed".to_string();
    }
    if t.state == TaskState::Completed && t.seeding {
        return "seeding".to_string();
    }
    if t.state == TaskState::Running && t.kind == TaskKind::Video {
        return "hunt".to_string();
    }
    if t.state == TaskState::Running && t.verifying {
        return "verifying".to_string();
    }
    if t.state == TaskState::Running && t.kind == TaskKind::Torrent && t.total.is_none() {
        return "metadata".to_string();
    }
    if t.state == TaskState::Running && t.probing {
        return "connecting".to_string();
    }
    if t.state == TaskState::Stale {
        return "expired".to_string();
    }
    format!("{:?}", t.state).to_lowercase()
}

#[derive(Debug, Clone)]
pub struct LinkRow {
    pub name: String,
    pub ip: String,
    pub default_route: bool,
    pub rate_bps: u64,
    pub capacity_bps: u64,
    pub weight: u32,
    pub in_use: u32,
    pub fails: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub connected: bool,
    pub daemon_label: String,
    pub daemon_version: String,
    pub bundles_supported: bool,
    pub unified_traffic: bool,
    pub tasks: Vec<TaskRow>,
    pub links: Vec<LinkRow>,
    pub detected_nics: Vec<UplinkNic>,
    pub total_bps: f64,
    pub rate_history: VecDeque<f64>,
    pub global_limit: Option<u64>,
    pub settings: Option<DaemonSettings>,
    pub torrent_session: bool,
    pub torrent_bind: Vec<String>,
    pub torrent_rates: Vec<(String, f64)>,
    pub torrent_unreachable: Vec<String>,
    pub extension: Option<ExtensionStatus>,
    pub update: Option<UpdateInfo>,
    pub update_checking: bool,
    pub update_install_status: Option<String>,
    pub lan: Option<LanInfo>,
    pub lan_peers: Vec<LanPeer>,
    pub lan_sends: Vec<LanSendInfo>,
    pub lan_scanning: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMode {
    AutoOrHttp,
    Video,
    Torrent,
    Bundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    Immediate,
    AfterDelay,
    AtTime,
}

#[derive(Debug, Clone)]
pub struct PendingAddRequest {
    pub url: String,
    pub dest_dir: Option<PathBuf>,
    pub headers: HashMap<String, String>,
    pub start_at: Option<i64>,
    pub kind: TaskKind,
    pub sequential: bool,
    pub only_files: Option<Vec<usize>>,
}

#[derive(Debug, Clone)]
pub struct DuplicatePrompt {
    pub request: PendingAddRequest,
    pub existing: TaskRow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickTarget {
    AddTorrentOpen,
    AddTorrentPayload,
    AddDestDir,
    OnboardingDownloadDir,
    SettingsDownloadDir,
    SettingsYtDlp,
    SettingsExtensionDir,
    SettingsLanDir,
    SettingsPluginScript,
    LanSendFile,
}

#[derive(Debug, Clone)]
pub struct AddDraft {
    pub open: bool,
    pub mode: AddMode,
    pub url_text: String,
    pub bundle_name: String,
    pub dest_dir: String,
    pub category: String,
    pub headers_text: String,
    pub advanced_open: bool,
    pub duplicate: DuplicatePolicy,
    pub schedule_kind: ScheduleKind,
    pub after_text: String,
    pub at_text: String,
    pub sequential: bool,
    pub torrent_preview_for: String,
    pub torrent_preview_busy: bool,
    pub torrent_preview: Option<TorrentPreviewSnapshot>,
    pub torrent_selected: HashSet<usize>,
    pub torrent_preview_error: Option<String>,
    pub bundle_preview_busy: bool,
    pub bundle_preview: Option<Vec<ripdl_core::bundle::Group>>,
    pub bundle_preview_error: Option<String>,
    pub submitting: bool,
    pub error: Option<String>,
}

impl Default for AddDraft {
    fn default() -> Self {
        Self {
            open: false,
            mode: AddMode::AutoOrHttp,
            url_text: String::new(),
            bundle_name: String::new(),
            dest_dir: String::new(),
            category: "auto".to_string(),
            headers_text: String::new(),
            advanced_open: false,
            duplicate: DuplicatePolicy::Share,
            schedule_kind: ScheduleKind::Immediate,
            after_text: "30m".to_string(),
            at_text: "23:00".to_string(),
            sequential: false,
            torrent_preview_for: String::new(),
            torrent_preview_busy: false,
            torrent_preview: None,
            torrent_selected: HashSet::new(),
            torrent_preview_error: None,
            bundle_preview_busy: false,
            bundle_preview: None,
            bundle_preview_error: None,
            submitting: false,
            error: None,
        }
    }
}

impl AddDraft {
    pub fn reset_for_open(&mut self) {
        *self = Self {
            open: true,
            ..Default::default()
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsTab {
    Overview,
    Files,
    Peers,
    Trackers,
    Segments,
    Parts,
}

#[derive(Debug, Clone, Default)]
pub struct CachedDetails {
    pub task_id: String,
    pub loading: bool,
    pub http: Option<TaskDetailsSnapshot>,
    pub torrent: Option<TorrentDetailsSnapshot>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DetailsDraft {
    pub task_id: Option<String>,
    pub tab: DetailsTab,
    pub selected_files: HashSet<usize>,
    pub synced_selection_for: String,
    pub add_parts_open: bool,
    pub add_parts_text: String,
    pub add_parts_error: Option<String>,
}

impl Default for DetailsDraft {
    fn default() -> Self {
        Self {
            task_id: None,
            tab: DetailsTab::Overview,
            selected_files: HashSet::new(),
            synced_selection_for: String::new(),
            add_parts_open: false,
            add_parts_text: String::new(),
            add_parts_error: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RefreshDraft {
    pub open: bool,
    pub task_id: String,
    pub task_name: String,
    pub part_seq: Option<u32>,
    pub url: String,
    pub last_probed_url: String,
    pub probing: bool,
    pub verdict_ok: bool,
    pub verdict_text: String,
    pub submitting: bool,
    pub error: Option<String>,
    pub resume_after: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Connection,
    Bonding,
    Torrents,
    Browser,
    Lan,
    Updates,
}

impl SettingsTab {
    pub fn all() -> &'static [Self] {
        &[
            Self::General,
            Self::Connection,
            Self::Bonding,
            Self::Torrents,
            Self::Browser,
            Self::Lan,
            Self::Updates,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Connection => "Speed & Limits",
            Self::Bonding => "Bonding",
            Self::Torrents => "Torrents",
            Self::Browser => "Browser",
            Self::Lan => "LAN Drop Box",
            Self::Updates => "Updates",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedUnit {
    Unlimited,
    KiB,
    MiB,
}

#[derive(Debug, Clone)]
pub struct SettingsDraft {
    pub open: bool,
    pub tab: SettingsTab,
    pub loaded: bool,
    pub dirty: bool,
    pub base: DaemonSettings,
    pub default_download_dir: String,
    pub hide_to_tray: bool,
    pub max_concurrent: usize,
    pub max_segments: usize,
    pub min_segment_text: String,
    pub speed_unit: SpeedUnit,
    pub speed_value: String,
    pub ytdlp_path: String,
    pub bond_interfaces: bool,
    pub bond_excluded: HashSet<String>,
    pub seed_enabled: bool,
    pub seed_ratio_text: String,
    pub seed_time_min: u32,
    pub extension_dir: String,
    pub browser_choice: BrowserChoice,
    pub update_check: bool,
    pub update_auto_install: bool,
    pub lan_enabled: bool,
    pub lan_port_text: String,
    pub lan_dir: String,
    pub lan_open: bool,
    pub lan_code: String,
    pub error: Option<String>,
}

impl Default for SettingsDraft {
    fn default() -> Self {
        Self {
            open: false,
            tab: SettingsTab::General,
            loaded: false,
            dirty: false,
            base: DaemonSettings::default(),
            default_download_dir: String::new(),
            hide_to_tray: true,
            max_concurrent: 3,
            max_segments: 8,
            min_segment_text: "1MiB".to_string(),
            speed_unit: SpeedUnit::Unlimited,
            speed_value: String::new(),
            ytdlp_path: String::new(),
            bond_interfaces: false,
            bond_excluded: HashSet::new(),
            seed_enabled: true,
            seed_ratio_text: "1".to_string(),
            seed_time_min: 0,
            extension_dir: String::new(),
            browser_choice: BrowserChoice::Auto,
            update_check: true,
            update_auto_install: false,
            lan_enabled: false,
            lan_port_text: "8463".to_string(),
            lan_dir: String::new(),
            lan_open: false,
            lan_code: String::new(),
            error: None,
        }
    }
}

impl SettingsDraft {
    pub fn load_from(&mut self, s: &DaemonSettings) {
        self.base = s.clone();
        self.default_download_dir = s
            .default_download_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.hide_to_tray = s.hide_to_tray.unwrap_or(true);
        self.max_concurrent = s.max_concurrent();
        self.max_segments = s.max_segments();
        self.min_segment_text = format_bytes_edit(s.min_segment_size());
        match s.max_speed() {
            None => {
                self.speed_unit = SpeedUnit::Unlimited;
                self.speed_value.clear();
            }
            Some(bps) if bps >= 1024 * 1024 && bps % (1024 * 1024) == 0 => {
                self.speed_unit = SpeedUnit::MiB;
                self.speed_value = format!("{}", bps / (1024 * 1024));
            }
            Some(bps) if bps % 1024 == 0 => {
                self.speed_unit = SpeedUnit::KiB;
                self.speed_value = format!("{}", bps / 1024);
            }
            Some(bps) => {
                self.speed_unit = SpeedUnit::KiB;
                self.speed_value = format!("{:.1}", bps as f64 / 1024.0);
            }
        }
        self.ytdlp_path = s.ytdlp_path.clone().unwrap_or_default();
        self.bond_interfaces = s.bond_interfaces();
        self.bond_excluded = s.bond_excludes().into_iter().collect();
        self.seed_enabled = s.seed_enabled();
        self.seed_ratio_text = format!("{}", s.seed_ratio());
        self.seed_time_min = s.seed_time_min();
        self.extension_dir = s
            .extension_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.update_check = s.update_check();
        self.update_auto_install = s.update_auto_install();
        self.lan_enabled = s.lan_enabled();
        self.lan_port_text = s.lan_port().to_string();
        self.lan_dir = s
            .lan_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.lan_open = s.lan_open();
        self.lan_code = s.lan_code.clone().unwrap_or_default();
        self.loaded = true;
        self.dirty = false;
        self.error = None;
    }

    pub fn build_payload(&self) -> Result<DaemonSettings, String> {
        if !(1..=MAX_CONCURRENT_LIMIT).contains(&self.max_concurrent) {
            return Err("Concurrent downloads must be between 1 and 64.".to_string());
        }
        if !(1..=MAX_SEGMENTS_LIMIT).contains(&self.max_segments) {
            return Err("Connections per download must be between 1 and 64.".to_string());
        }
        let min_seg = parse_bytes_loose(&self.min_segment_text)?.unwrap_or(1024 * 1024);
        if min_seg < 1 {
            return Err("Minimum chunk size must be at least 1 byte.".to_string());
        }
        let max_speed_bytes = parse_speed_limit(self.speed_unit, &self.speed_value)?;
        let ratio_str = self.seed_ratio_text.trim();
        let seed_ratio = if ratio_str.is_empty() {
            1.0
        } else {
            let r: f64 = ratio_str
                .parse()
                .map_err(|_| "Seed ratio must be 0 or a positive number.".to_string())?;
            if !r.is_finite() || r < 0.0 {
                return Err("Seed ratio must be 0 or a positive number.".to_string());
            }
            r
        };
        let port: u16 = self
            .lan_port_text
            .trim()
            .parse()
            .map_err(|_| "LAN port must be a valid port 1-65535.".to_string())?;
        if port == 0 {
            return Err("LAN port must be 1-65535.".to_string());
        }
        let code_trimmed = self.lan_code.trim();
        let lan_code = if code_trimmed.is_empty() {
            None
        } else if !ripdl_core::rpc::is_valid_lan_code(code_trimmed) {
            return Err("LAN pairing code must be at least 6 ASCII alphanumeric characters.".into());
        } else {
            Some(code_trimmed.to_string())
        };

        let opt_path = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| PathBuf::from(t))
        };
        let opt_str = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };

        let mut out = self.base.clone();
        out.max_concurrent = Some(self.max_concurrent);
        out.max_segments = Some(self.max_segments);
        out.min_segment_size = Some(min_seg);
        out.max_speed_bytes = max_speed_bytes;
        out.default_download_dir = opt_path(&self.default_download_dir);
        out.hide_to_tray = Some(self.hide_to_tray);
        out.ytdlp_path = opt_str(&self.ytdlp_path);
        out.bond_interfaces = Some(self.bond_interfaces);
        out.bond_exclude = if self.bond_excluded.is_empty() {
            None
        } else {
            let mut v: Vec<String> = self.bond_excluded.iter().cloned().collect();
            v.sort();
            Some(v)
        };
        out.seed_enabled = Some(self.seed_enabled);
        out.seed_ratio = Some(seed_ratio);
        out.seed_time_min = Some(self.seed_time_min);
        out.extension_dir = opt_path(&self.extension_dir);
        out.update_check = Some(self.update_check);
        out.update_auto_install = Some(self.update_auto_install);
        out.lan_enabled = Some(self.lan_enabled);
        out.lan_port = Some(port);
        out.lan_dir = opt_path(&self.lan_dir);
        out.lan_open = Some(self.lan_open);
        out.lan_code = lan_code;
        Ok(out)
    }
}

pub fn parse_speed_limit(unit: SpeedUnit, value: &str) -> Result<Option<u64>, String> {
    match unit {
        SpeedUnit::Unlimited => Ok(Some(0)),
        SpeedUnit::KiB | SpeedUnit::MiB => {
            let raw = value.trim();
            if raw.is_empty() {
                Ok(Some(0))
            } else {
                let n: f64 = raw
                    .parse()
                    .map_err(|_| "Speed limit must be a positive number.".to_string())?;
                if !n.is_finite() || n <= 0.0 {
                    return Err("Speed limit must be greater than zero.".to_string());
                }
                let mult = if unit == SpeedUnit::MiB {
                    1024.0 * 1024.0
                } else {
                    1024.0
                };
                let bps = (n * mult).round() as u64;
                if bps < 1024 {
                    return Err("Minimum speed limit is 1 KiB/s.".to_string());
                }
                Ok(Some(bps))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    Folder,
    Browser,
    Network,
}

#[derive(Debug, Clone)]
pub struct OnboardingDraft {
    pub open: bool,
    pub step: OnboardingStep,
    pub download_dir: String,
    pub browser_choice: BrowserChoice,
    pub host_status: Option<(bool, String)>,
    pub bond_interfaces: bool,
    pub speed_unit: SpeedUnit,
    pub speed_value: String,
    pub error: Option<String>,
}

impl Default for OnboardingDraft {
    fn default() -> Self {
        Self {
            open: false,
            step: OnboardingStep::Folder,
            download_dir: default_downloads_fallback().display().to_string(),
            browser_choice: BrowserChoice::Auto,
            host_status: None,
            bond_interfaces: false,
            speed_unit: SpeedUnit::Unlimited,
            speed_value: String::new(),
            error: None,
        }
    }
}

impl OnboardingDraft {
    pub fn reset_from(&mut self, settings: Option<&DaemonSettings>) {
        self.open = true;
        self.step = OnboardingStep::Folder;
        self.error = None;
        self.host_status = None;
        if let Some(s) = settings {
            self.download_dir = s
                .default_download_dir
                .clone()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(default_downloads_fallback)
                .display()
                .to_string();
            self.bond_interfaces = s.bond_interfaces();
            match s.max_speed() {
                None => {
                    self.speed_unit = SpeedUnit::Unlimited;
                    self.speed_value.clear();
                }
                Some(bps) if bps >= 1024 * 1024 && bps % (1024 * 1024) == 0 => {
                    self.speed_unit = SpeedUnit::MiB;
                    self.speed_value = format!("{}", bps / (1024 * 1024));
                }
                Some(bps) => {
                    self.speed_unit = SpeedUnit::KiB;
                    self.speed_value = format!("{}", (bps / 1024).max(1));
                }
            }
        } else {
            self.download_dir = default_downloads_fallback().display().to_string();
            self.bond_interfaces = false;
            self.speed_unit = SpeedUnit::Unlimited;
            self.speed_value.clear();
        }
    }

    pub fn build_commit_settings(&self, base: Option<&DaemonSettings>) -> Result<DaemonSettings, String> {
        let mut out = base.cloned().unwrap_or_default();
        let trimmed_dir = self.download_dir.trim();
        if !trimmed_dir.is_empty() {
            out.default_download_dir = Some(PathBuf::from(trimmed_dir));
        }
        out.bond_interfaces = Some(self.bond_interfaces);
        out.max_speed_bytes = parse_speed_limit(self.speed_unit, &self.speed_value)?;
        out.onboarding_done = Some(true);
        Ok(out)
    }
}

/// First-run onboarding opens automatically ONLY on genuinely new installs:
/// an existing <=0.4.10 user (with tasks, prior extension heartbeat, or an
/// existing config/queue file) is never interrupted on upgrade.
pub fn should_auto_open_onboarding(
    settings: &DaemonSettings,
    tasks: &[TaskRow],
    extension: Option<&ExtensionStatus>,
    has_existing_profile_files: bool,
) -> bool {
    match settings.onboarding_done {
        Some(done) => !done,
        None => {
            tasks.is_empty()
                && extension.is_none_or(|e| e.last_seen <= 0)
                && !has_existing_profile_files
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LanDraft {
    pub open: bool,
    pub send_to: String,
    pub send_code: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeleteConfirm {
    pub ids: Vec<String>,
    pub label: String,
    pub delete_files: bool,
}

#[derive(Debug, Clone)]
pub struct UiNotice {
    pub text: String,
    pub is_error: bool,
    pub until: Instant,
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn default_downloads_fallback() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn resolved_add_dir(
    custom_dir: &str,
    settings: Option<&DaemonSettings>,
    category: &str,
    url_or_payload: &str,
) -> PathBuf {
    let trimmed = custom_dir.trim();
    if !trimmed.is_empty() {
        return PathBuf::from(trimmed);
    }
    let mut base = settings
        .and_then(|s| s.default_download_dir.clone())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(default_downloads_fallback);
    let sub = match category {
        "auto" => {
            let name = fallback_name_from_url(url_or_payload).unwrap_or_default();
            categorize(&name).subdir()
        }
        "video" => Some("Videos"),
        "music" => Some("Music"),
        "images" => Some("Images"),
        "documents" => Some("Documents"),
        "archives" => Some("Archives"),
        "programs" => Some("Programs"),
        _ => None,
    };
    if let Some(sub) = sub {
        base.push(sub);
    }
    base
}

pub fn humans_bin(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut n = bytes as f64;
    let mut i = 0;
    while n >= 1024.0 && i + 1 < UNITS.len() {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 || n >= 100.0 {
        format!("{n:.0} {}", UNITS[i])
    } else {
        format!("{n:.1} {}", UNITS[i])
    }
}

pub fn format_bytes_edit(bytes: u64) -> String {
    if bytes == 0 {
        return "0".to_string();
    }
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes;
    let mut idx = 0;
    while idx + 1 < UNITS.len() && v >= 1024 && v % 1024 == 0 {
        v /= 1024;
        idx += 1;
    }
    if idx == 0 && bytes >= 1024 {
        bytes.to_string()
    } else {
        format!("{v}{}", UNITS[idx])
    }
}

pub fn parse_bytes_loose(raw: &str) -> Result<Option<u64>, String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Ok(None);
    }
    let cut = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = s[cut.len()..].trim();
    let mult: u64 = match suffix {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        "t" | "tb" | "tib" => 1024 * 1024 * 1024 * 1024,
        other => return Err(format!("unknown unit '{other}' (use KiB, MiB, GiB)")),
    };
    let val: f64 = cut
        .trim()
        .parse()
        .map_err(|_| format!("invalid size '{raw}'"))?;
    if !val.is_finite() || val < 0.0 {
        return Err("size must not be negative".to_string());
    }
    Ok(Some((val * mult as f64).round() as u64))
}

pub fn parse_after_delay(raw: &str) -> Result<u64, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("enter a delay like 30s, 15m, or 1h30m".to_string());
    }
    let mut total = 0u64;
    let mut num_start: Option<usize> = None;
    for (i, ch) in s.char_indices() {
        if ch.is_ascii_digit() {
            if num_start.is_none() {
                num_start = Some(i);
            }
        } else {
            let start = num_start.take().ok_or_else(|| format!("bad delay '{s}'"))?;
            let n: u64 = s[start..i].parse().map_err(|_| format!("bad number in '{s}'"))?;
            let mult: u64 = match ch.to_ascii_lowercase() {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86_400,
                other => return Err(format!("bad delay unit '{other}' (use s/m/h/d)")),
            };
            total = total
                .checked_add(n.checked_mul(mult).ok_or_else(|| "delay overflow".to_string())?)
                .ok_or_else(|| "delay overflow".to_string())?;
        }
    }
    if num_start.is_some() {
        return Err(format!("missing unit at end of '{s}' (use s/m/h/d)"));
    }
    if total == 0 {
        return Err("delay must be positive".to_string());
    }
    Ok(total)
}

pub fn parse_at_hhmm(raw: &str) -> Result<i64, String> {
    let s = raw.trim();
    let (h_str, m_str) = s
        .split_once(':')
        .ok_or_else(|| "time must be HH:MM (24-hour)".to_string())?;
    let h: u32 = h_str.parse().map_err(|_| "invalid hour".to_string())?;
    let m: u32 = m_str.parse().map_err(|_| "invalid minute".to_string())?;
    if h > 23 || m > 59 {
        return Err("time must be 00:00..23:59".to_string());
    }
    let now = now_unix();
    let today_secs = now.rem_euclid(86_400);
    let target_secs = (h as i64) * 3600 + (m as i64) * 60;
    let delta = if target_secs > today_secs {
        target_secs - today_secs
    } else {
        target_secs + 86_400 - today_secs
    };
    Ok(now + delta)
}

pub fn eta_text(t: &TaskRow) -> String {
    if t.raw_state != TaskState::Running || t.verifying {
        return "-".to_string();
    }
    let Some(total) = t.total else {
        return "-".to_string();
    };
    if t.downloaded >= total {
        return "-".to_string();
    }
    let Some(spd) = t.speed_bps else {
        return "-".to_string();
    };
    if spd < 1.0 {
        return "-".to_string();
    }
    let secs = ((total - t.downloaded) as f64 / spd).ceil() as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub fn host_of(url: &str) -> String {
    if url.starts_with("magnet:") {
        return "magnet / DHT".to_string();
    }
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|h| !h.is_empty())
        .unwrap_or("local")
        .to_string()
}

pub fn fallback_name_from_url(url: &str) -> Option<String> {
    if url.starts_with("magnet:") {
        for kv in url.split(['?', '&']) {
            if let Some(dn) = kv.strip_prefix("dn=") {
                if !dn.is_empty() {
                    return Some(dn.replace('+', " "));
                }
            }
        }
        return Some("Magnet torrent".to_string());
    }
    let no_q = url.split(['?', '#']).next().unwrap_or(url);
    no_q.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn count_filter(tasks: &[TaskRow], filter: SidebarFilter) -> usize {
    tasks.iter().filter(|t| matches_filter(t, filter, "")).count()
}

pub fn matches_filter(t: &TaskRow, filter: SidebarFilter, search: &str) -> bool {
    let cat_ok = match filter {
        SidebarFilter::All => true,
        SidebarFilter::Active => t.is_active(),
        SidebarFilter::PausedOrExpired => t.is_paused_or_expired(),
        SidebarFilter::Completed => t.raw_state == TaskState::Completed,
        SidebarFilter::Torrents => t.kind == "torrent",
        SidebarFilter::Bundles => t.kind == "bundle",
        SidebarFilter::Streams => t.kind == "video",
        SidebarFilter::Category(cat) => t.category == cat,
    };
    if !cat_ok {
        return false;
    }
    let q = search.trim();
    if q.is_empty() {
        return true;
    }
    let q = q.to_ascii_lowercase();
    t.name.to_ascii_lowercase().contains(&q)
        || t.source.to_ascii_lowercase().contains(&q)
        || t.state.to_ascii_lowercase().contains(&q)
        || t.kind.to_ascii_lowercase().contains(&q)
}

pub fn sort_tasks(rows: &mut [&TaskRow], key: SortKey, dir: SortDir) {
    rows.sort_by(|a, b| {
        let ord = match key {
            SortKey::Name => a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()),
            SortKey::Kind => a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)),
            SortKey::Size => a
                .total
                .unwrap_or(a.downloaded)
                .cmp(&b.total.unwrap_or(b.downloaded)),
            SortKey::Progress => a
                .ratio()
                .partial_cmp(&b.ratio())
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Status => a.state.cmp(&b.state).then_with(|| a.name.cmp(&b.name)),
            SortKey::Rate => {
                let ra = a.speed_bps.or(a.upload_bps).unwrap_or(-1.0);
                let rb = b.speed_bps.or(b.upload_bps).unwrap_or(-1.0);
                ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
            }
        };
        if dir == SortDir::Desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

pub fn parse_headers_block(raw: &str) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (k, v) = trimmed
            .split_once(':')
            .ok_or_else(|| format!("invalid header line '{trimmed}' (expected Name: value)"))?;
        let key = k.trim();
        if key.is_empty() {
            return Err("header name cannot be empty".to_string());
        }
        out.insert(key.to_string(), v.trim().to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(raw_state: TaskState, speed_bps: Option<f64>) -> TaskRow {
        TaskRow {
            id: "t1".into(),
            name: "sample.bin".into(),
            url: "https://cdn.example.org/sample.bin".into(),
            kind: "http".into(),
            source: "cdn.example.org".into(),
            dest_dir: None,
            dest_file: None,
            raw_state,
            state: format!("{raw_state:?}").to_lowercase(),
            downloaded: 500,
            total: Some(1000),
            percent: None,
            speed_bps,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            verifying: false,
            peers: 0,
            start_at: None,
            parts_done: 0,
            parts_total: 0,
            parts_missing: Vec::new(),
            stale_reason: None,
            error: None,
            category: ripdl_core::categories::Category::Other,
        }
    }

    #[test]
    fn binary_edit_roundtrips_without_drift() {
        for bytes in [1024u64, 1_048_576, 4 * 1024 * 1024, 524_288, 123_456] {
            let formatted = format_bytes_edit(bytes);
            let parsed = parse_bytes_loose(&formatted).unwrap().unwrap();
            assert_eq!(parsed, bytes, "{formatted} drifted on round-trip");
        }
    }

    #[test]
    fn eta_only_shows_for_active_nonzero_running_downloads() {
        assert_ne!(eta_text(&sample_row(TaskState::Running, Some(100.0))), "-");
        assert_eq!(eta_text(&sample_row(TaskState::Paused, Some(100.0))), "-");
        assert_eq!(eta_text(&sample_row(TaskState::Stale, Some(100.0))), "-");
        assert_eq!(eta_text(&sample_row(TaskState::Completed, Some(100.0))), "-");
    }

    #[test]
    fn queued_rows_never_carry_live_speed() {
        let info = TaskInfo {
            id: "q1".into(),
            url: "https://example.org/a.bin".into(),
            dest_dir: None,
            dest_file: None,
            state: TaskState::Queued,
            kind: TaskKind::Http,
            downloaded: 0,
            total: Some(1000),
            percent: None,
            speed_bps: Some(999.0),
            probing: false,
            error: None,
            stale_reason: None,
            start_at: None,
            uploaded: 0,
            upload_bps: None,
            seeding: false,
            seeding_stopped: false,
            verifying: false,
            peers: 0,
            info_hash: None,
            name: None,
            sequential: false,
            only_files: None,
            parts_done: 0,
            parts_total: 0,
            parts_missing: Vec::new(),
        };
        let row = TaskRow::from_rpc(info);
        assert_eq!(row.speed_bps, None);
    }

    #[test]
    fn resolved_add_dir_honors_daemon_default_dir_and_category() {
        let mut s = DaemonSettings::default();
        s.default_download_dir = Some(PathBuf::from("E:/CustomDL"));
        let p = resolved_add_dir("", Some(&s), "auto", "https://example.com/archive.zip");
        assert_eq!(p, PathBuf::from("E:/CustomDL").join("Archives"));
        let explicit = resolved_add_dir("F:/Other", Some(&s), "auto", "https://example.com/a.zip");
        assert_eq!(explicit, PathBuf::from("F:/Other"));
    }

    #[test]
    fn settings_payload_validates_bounds_and_preserves_zero_speed_as_uncapped() {
        let mut draft = SettingsDraft::default();
        draft.max_concurrent = 65;
        assert!(draft.build_payload().is_err());
        draft.max_concurrent = 4;
        draft.max_segments = 0;
        assert!(draft.build_payload().is_err());
        draft.max_segments = 8;
        draft.speed_unit = SpeedUnit::KiB;
        draft.speed_value = "512".into();
        let built = draft.build_payload().unwrap();
        assert_eq!(built.max_speed(), Some(512 * 1024));
    }

    #[test]
    fn onboarding_never_hijacks_existing_v0_4_users() {
        let upgrade_default = DaemonSettings::default();
        assert!(
            !should_auto_open_onboarding(&upgrade_default, &[], None, true),
            "existing profile files must suppress automatic onboarding"
        );
        assert!(
            !should_auto_open_onboarding(
                &upgrade_default,
                &[sample_row(TaskState::Completed, None)],
                None,
                false
            ),
            "existing tasks must suppress automatic onboarding"
        );
        let done_true = DaemonSettings {
            onboarding_done: Some(true),
            ..Default::default()
        };
        assert!(!should_auto_open_onboarding(&done_true, &[], None, false));
        assert!(should_auto_open_onboarding(
            &upgrade_default,
            &[],
            None,
            false
        ));
    }

    #[test]
    fn onboarding_commit_preserves_other_settings() {
        let existing = DaemonSettings {
            max_segments: Some(12),
            seed_ratio: Some(0.5),
            lan_code: Some("ABCDEF".into()),
            ..Default::default()
        };
        let mut ob = OnboardingDraft::default();
        ob.reset_from(Some(&existing));
        ob.download_dir = "D:/MyDownloads".into();
        let committed = ob.build_commit_settings(Some(&existing)).unwrap();
        assert_eq!(committed.max_segments, Some(12));
        assert_eq!(committed.seed_ratio, Some(0.5));
        assert_eq!(committed.lan_code.as_deref(), Some("ABCDEF"));
        assert_eq!(committed.onboarding_done, Some(true));
        assert_eq!(committed.default_download_dir, Some(PathBuf::from("D:/MyDownloads")));
    }
}
