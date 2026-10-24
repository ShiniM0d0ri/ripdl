# qBittorrent options inventory (clean-room)

Purpose: enumerate qBittorrent's user-facing Preferences and torrent actions so
ripdl can decide what a download manager should adopt. This is a feature map,
not a port plan.

- Snapshot: **qBittorrent release-5.2.4** (2026-09-28; latest stable at time of
  writing). Master (`d494d1b`, 2026-10-18) differences are noted separately.
- Written: 2026-10-19.
- Clean-room note: qBittorrent is GPL-2.0+; ripdl is MIT OR Apache-2.0. Only
  public docs and Qt UI/source definitions were read, to learn **which options
  exist and what they do**. No code, XML, stylesheets, icons, or long strings
  were copied. All summaries below are original wording.

## Sources

| Source | Used for |
|---|---|
| https://github.com/qbittorrent/qBittorrent/wiki/Explanation-of-Options-in-qBittorrent | official option explanations |
| https://github.com/qbittorrent/qBittorrent/blob/release-5.2.4/src/gui/optionsdialog.ui | tab list and 191 form controls |
| https://github.com/qbittorrent/qBittorrent/blob/release-5.2.4/src/gui/advancedsettings.cpp | 84 Advanced-tab rows |
| https://github.com/qbittorrent/qBittorrent/blob/release-5.2.4/src/gui/addnewtorrentdialog.ui | add-torrent defaults |
| https://github.com/qbittorrent/qBittorrent/blob/release-5.2.4/src/gui/torrentoptionsdialog.ui | per-torrent overrides |
| https://github.com/qbittorrent/qBittorrent/blob/release-5.2.4/src/gui/torrentsharelimitswidget.ui | ratio/time/action limits |
| https://github.com/qbittorrent/qBittorrent/blob/release-5.2.4/src/gui/transferlistwidget.cpp | row context menu actions |
| https://github.com/qbittorrent/qBittorrent/releases/tag/release-5.2.4 | version/date |

## 1. Total counts

**9 Preferences pages. 275 user-settable controls in 5.2.4.** Master adds 12
more (287).

| Preferences tab | 5.2.4 controls | master | Notes |
|---|---:|---:|---|
| Behavior | 43 | 43 | UI, list, tray, logging, power, status bar |
| Downloads | 41 | 42 | add defaults, save paths, watched folders, email, hooks |
| Connection | 29 | 30 | ports, peer limits, proxy, IP filter, I2P |
| Speed | 11 | 11 | global/alternative limits, schedule |
| BitTorrent | 26 | 28 | privacy, queue, seeding limits, trackers |
| Search | 3 | 4 | tab persistence only; plugins are separate |
| RSS | 7 | 7 | reader + auto-download enablement |
| WebUI | 31 | 32 | auth, HTTPS, API key, security |
| Advanced | 84 | 90 | searchable list of app/libtorrent tunables |
| **Total** | **275** | **287** | |

Counting method:

- 1 control = one checkbox, checkable group-box master switch, radio button,
  combo box, spin/double-spin/time box, line edit, path edit, or multiline text
  box.
- A master switch and its dependent fields count separately (e.g. "Use another
  path for incomplete torrents" + the path box = 2).
- Excluded: static labels, non-checkable group frames, list/tree views, and
  action buttons such as Browse, Random, Test email, Register, Copy/Rotate API
  key, Edit RSS rules, Add/Remove watched folder. There are roughly 30 such
  actions, plus separate dialogs (RSS rules, IP ban list, subnet whitelist).
- Platform-conditional controls present in the `.ui` are counted once. Some
  only appear on Windows/macOS.
- Advanced is a single searchable table rather than hand-placed widgets; each
  row is one setting. If you merge obvious pairs (upload/download limit, min/
  max port) the "human concept" count is roughly 230, but the raw control count
  is 275.

Master delta: +1 Downloads choice (default-category path action), +1 Connection
checkbox (I2P PeX), +2 BitTorrent radios (share limits match any/all), +1 Search
checkbox, +1 WebUI number (session count limit), +6 Advanced rows. The 5.2.4
numbers are the reference below.

## 2. Preferences inventory

Types: `check`, `choice`, `number`, `text`, `path`, `multiline`, `action`.
Actions are listed for completeness but are **not** in the 275 count.

### Behavior (43)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Language | choice | Selects the UI language. |
| 2 | UI style | choice | Qt widget style, or system default. |
| 3 | Color scheme | choice | Light, dark, or follow system. |
| 4 | Use custom UI theme | check | Master switch for a theme file. |
| 5 | Theme file | path | Path to a custom `.qbtheme`. |
| 6 | Use system icons | check | Inherits the OS icon theme instead of bundled icons. |
| 7 | Confirm when deleting torrents | check | Ask before removing a torrent. |
| 8 | Alternating row colors | check | Zebra-striped transfer list. |
| 9 | Colors by torrent state | check | Tints row text according to state. |
| 10 | Progress bars follow text color | check | Ties bar color to the state text color. |
| 11 | Hide zero and infinity values | check | Blanks 0 and ∞ cells. |
| 12 | Hide-zero scope | choice | Apply to all rows, or stopped torrents only. |
| 13 | Double-click action, downloading | choice | Start/stop, open folder, preview, torrent options, or nothing. |
| 14 | Double-click action, finished | choice | Same action set for completed rows. |
| 15 | Auto-hide zero status filters | check | Hides empty sidebar filters. |
| 16 | Separate tracker-status filter | check | Adds a tracker-state filter entry. |
| 17 | Drag content from qBittorrent | check | Allows dragging files out of the content view. |
| 18 | Start on Windows login | check | Registers autostart. |
| 19 | Splash screen | check | Shows the launch splash. |
| 20 | Startup window state | choice | Normal, minimized, or hidden. |
| 21 | Exit confirmation | check | Warn when active torrents exist. |
| 22 | Auto-exit confirmation | check | Confirm when the auto-exit condition fires. |
| 23 | Show tray icon | check | Master switch for the notification-area icon. |
| 24 | Minimize to tray | check | Minimize hides to the tray. |
| 25 | Close to tray | check | Close hides to the tray. |
| 26 | Tray icon style | choice | Normal or monochrome for dark/light themes. |
| 27 | Associate `.torrent` files | check | Registers qBittorrent as the handler. |
| 28 | Associate magnet links | check | Registers qBittorrent as the handler. |
| 29 | Check for program updates | check | Queries GitHub releases. |
| 30 | Show speed in Dock | check | macOS Dock speed readout. |
| 31 | Show menu bar icon | check | macOS status-bar item. |
| 32 | Inhibit sleep while downloading | check | Keeps the system awake during downloads. |
| 33 | Inhibit sleep while seeding | check | Keeps the system awake while seeding. |
| 34 | Enable log files | check | Master switch for file logging. |
| 35 | Log directory | path | Where log files are written. |
| 36 | Backup log after size | check | Enables size-based log rotation. |
| 37 | Log size threshold | number | Rotation size. |
| 38 | Delete old backups | check | Enables age-based cleanup. |
| 39 | Backup age | number | Age threshold. |
| 40 | Age unit | choice | Days, months, or years. |
| 41 | Log performance warnings | check | Records UI-thread stall warnings. |
| 42 | Show free disk space | check | Status-bar disk readout. |
| 43 | Show external IP | check | Status-bar external-address readout. |

Actions excluded: Customize UI Theme, Open default-apps settings.

### Downloads (41)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Show add-torrent dialog | check | Master switch for the content/options dialog. |
| 2 | Bring dialog to front | check | Focuses the dialog when it opens. |
| 3 | Content layout | choice | Original, create subfolder, or no subfolder. |
| 4 | Add to top of queue | check | New torrents take priority. |
| 5 | Do not start automatically | check | New torrents start paused. |
| 6 | Stop condition | choice | None, metadata received, or files checked. |
| 7 | Merge trackers on duplicate | check | Adds new trackers to an existing torrent. |
| 8 | Ask before merging trackers | check | Confirmation for manual adds. |
| 9 | Delete `.torrent` afterwards | check | Master switch for add-file cleanup. |
| 10 | Also when addition is cancelled | check | Cleanup even on cancel. |
| 11 | Pre-allocate all files | check | Reserves disk space up front. |
| 12 | Append `.!qB` to incomplete files | check | Marks partial files in the filesystem. |
| 13 | Keep unselected files in `.unwanted` | check | Preserves unselected content in a side folder. |
| 14 | Recursive download dialog | check | Offers recursive selection for multi-file adds. |
| 15 | Saving mode | choice | Manual or automatic path management. |
| 16 | Category change action | choice | Relocate torrents or switch them to manual. |
| 17 | Default category path change action | choice | Same choice for the default category. |
| 18 | Category path change action | choice | Same choice for other categories. |
| 19 | Use category paths in manual mode | check | Category path still applies in manual mode. |
| 20 | Default save path | path | Main download directory. |
| 21 | Use separate incomplete path | check | Master switch for a temp download area. |
| 22 | Incomplete path | path | Directory for incomplete content. |
| 23 | Copy `.torrent` files to | check | Keeps a copy of added metadata. |
| 24 | Export directory | path | Destination for added `.torrent` copies. |
| 25 | Copy finished `.torrent` files to | check | Keeps metadata after completion. |
| 26 | Finished export directory | path | Destination for finished metadata. |
| 27 | Excluded file names | check | Enables the exclusion list. |
| 28 | Excluded names | multiline | Names/patterns to skip on add. |
| 29 | Email notification | check | Master switch for completion mail. |
| 30 | Sender address | text | From address. |
| 31 | Recipient address | text | To address. |
| 32 | SMTP server | text | Mail relay host. |
| 33 | SMTP SSL | check | Use a secure connection. |
| 34 | SMTP authentication | check | Master switch for mail auth. |
| 35 | SMTP username | text | Mail login. |
| 36 | SMTP password | text | Mail secret. |
| 37 | Run on torrent added | check | Enables an external command on add. |
| 38 | Added command | text | Command line with torrent placeholders. |
| 39 | Run on torrent finished | check | Enables a command on completion. |
| 40 | Finished command | text | Command line with torrent placeholders. |
| 41 | Show console window | check | Keeps the child console visible. |

Not counted: watched-folder tree and its Add/Options/Remove buttons, Test email,
Browse actions. Watched folders themselves are a feature: auto-add `.torrent`
files dropped into configured directories.

### Connection (29)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Protocol | choice | TCP and µTP, TCP only, or µTP only. |
| 2 | Listening port | number | Incoming peer port. |
| 3 | UPnP / NAT-PMP | check | Asks the router to forward the port. |
| 4 | Global max connections | check | Enables the global cap. |
| 5 | Global connection value | number | Maximum peer connections overall. |
| 6 | Per-torrent max connections | check | Enables the per-torrent cap. |
| 7 | Per-torrent value | number | Maximum peers per torrent. |
| 8 | Global max upload slots | check | Enables the global upload-slot cap. |
| 9 | Global slot value | number | Maximum unchoked peers overall. |
| 10 | Per-torrent max upload slots | check | Enables the per-torrent cap. |
| 11 | Per-torrent slot value | number | Maximum unchoked peers per torrent. |
| 12 | Enable I2P | check | Turns on the I2P SAM bridge integration. |
| 13 | I2P host | text | SAM bridge host. |
| 14 | I2P port | number | SAM bridge port. |
| 15 | I2P mixed mode | check | Allows both I2P and clearnet peers. |
| 16 | Proxy type | choice | None, SOCKS4, SOCKS5, or HTTP. |
| 17 | Proxy host | text | Proxy address. |
| 18 | Proxy port | number | Proxy port. |
| 19 | Resolve hostnames via proxy | check | Sends DNS through the proxy. |
| 20 | Proxy authentication | check | Master switch for proxy credentials. |
| 21 | Proxy username | text | Proxy login. |
| 22 | Proxy password | text | Proxy secret. |
| 23 | Use proxy for BitTorrent | check | Master switch for the BT proxy scope. |
| 24 | Proxy peer connections | check | Routes peer traffic through the proxy. |
| 25 | Proxy RSS | check | Routes feed fetches through the proxy. |
| 26 | Proxy general purposes | check | Routes other app traffic through the proxy. |
| 27 | Enable IP filtering | check | Loads a filter list. |
| 28 | Filter file | path | `.dat`, `.p2p`, or `.p2b` list. |
| 29 | Apply filter to trackers | check | Also filters tracker connections. |

Actions excluded: Random port, Refresh filter, Manually banned IP addresses.

### Speed (11)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Global upload limit | number | Total upload cap (KiB/s). |
| 2 | Global download limit | number | Total download cap. |
| 3 | Alternative upload limit | number | Second cap for scheduled hours. |
| 4 | Alternative download limit | number | Second download cap. |
| 5 | Schedule alternative limits | check | Enables the time window. |
| 6 | From | time | Window start. |
| 7 | To | time | Window end. |
| 8 | Days | choice | Every day, weekdays, or weekends. |
| 9 | Limit µTP | check | Counts µTP traffic against the cap. |
| 10 | Limit transport overhead | check | Counts protocol overhead. |
| 11 | Limit LAN peers | check | Applies caps to local peers. |

### BitTorrent (26)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | DHT | check | Trackerless peer discovery. |
| 2 | PeX | check | Peer exchange with connected peers. |
| 3 | LSD | check | Local peer discovery on the LAN. |
| 4 | Encryption | choice | Allow, require, or disable peer encryption. |
| 5 | Anonymous mode | check | Hides client identity from peers/trackers where possible. |
| 6 | Max active checking torrents | number | Limits concurrent rechecks. |
| 7 | Torrent queueing | check | Master switch for active-torrent limits. |
| 8 | Max active downloads | number | Concurrent downloading torrents. |
| 9 | Max active uploads | number | Concurrent seeding torrents. |
| 10 | Max active torrents | number | Combined active cap. |
| 11 | Ignore slow torrents | check | Excludes slow torrents from queue limits. |
| 12 | Download slow threshold | number | Download rate below which a torrent is "slow". |
| 13 | Upload slow threshold | number | Upload rate threshold. |
| 14 | Slow inactivity timer | number | How long a slow torrent must stay slow. |
| 15 | Max ratio | check | Enables a ratio-based stop. |
| 16 | Ratio value | number | Target share ratio. |
| 17 | Max seeding time | check | Enables a total-seed-time stop. |
| 18 | Seeding minutes | number | Time limit. |
| 19 | Max inactive seeding time | check | Enables an idle-seed stop. |
| 20 | Inactive minutes | number | Idle time limit. |
| 21 | Share-limit action | choice | Stop, remove, remove with files, or enable super seeding. |
| 22 | Append trackers | check | Master switch for a default tracker list. |
| 23 | Tracker list | multiline | Trackers added to every new torrent. |
| 24 | Append trackers from URL | check | Master switch for a remote list. |
| 25 | Tracker list URL | text | Remote list address. |
| 26 | Fetched tracker list | multiline | Cached/edited copy of the remote list. |

Master adds two radios: "any of the above" vs "all of the above" for the three
seeding limits. 5.2.4 applies the limits independently.

### Search (3)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Store opened tabs | check | Restores search tabs on restart. |
| 2 | Also store results | check | Persists result lists too. |
| 3 | History length | number | How many searches are remembered. |

Search plugins and the search engine itself are not Preferences options; they
are a separate subsystem.

### RSS (7)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Enable fetching RSS feeds | check | Turns on the feed reader. |
| 2 | Refresh interval | number | Minutes between feed fetches. |
| 3 | Fetch delay | number | Delay between individual feeds. |
| 4 | Max articles per feed | number | Trims stored items. |
| 5 | Enable auto-downloading | check | Master switch for RSS rules. |
| 6 | Download revised uploads | check | Smart filter accepts updated releases. |
| 7 | Smart episode filters | multiline | Additional episode-filter patterns. |

RSS rules (separate dialog, ~8 fields per rule): must contain / must not
contain, regex mode, episode filter, smart episode filter, ignore period,
priority, and torrent parameters (save path, category, tags, add paused). Rules
can also be assigned per feed. `Edit auto downloading rules...` is an action,
not counted.

### WebUI (31)

| # | Option | Type | Behavior |
|---:|---|---|---|
| 1 | Enable Web UI | check | Starts the HTTP remote-control server. |
| 2 | Address | text | Bind address. |
| 3 | Port | number | HTTP port. |
| 4 | UPnP / NAT-PMP | check | Requests a router port mapping. |
| 5 | Use HTTPS | check | Enables TLS. |
| 6 | Certificate | path | PEM certificate. |
| 7 | Private key | path | PEM key. |
| 8 | Username | text | Login name. |
| 9 | Password | text | Login secret. |
| 10 | API key | text | Static key for the Web API. |
| 11 | Bypass auth on localhost | check | Trusts local clients. |
| 12 | Bypass auth for subnets | check | Trusts listed subnets. |
| 13 | Max failed logins | number | Ban threshold. |
| 14 | Ban duration | number | Ban length. |
| 15 | Session timeout | number | Idle logout. |
| 16 | Use alternative WebUI | check | Serves a custom root. |
| 17 | Alternative root folder | path | Custom WebUI directory. |
| 18 | Clickjacking protection | check | Sends frame-ancestors policy. |
| 19 | CSRF protection | check | Requires same-origin tokens. |
| 20 | Secure cookie flag | check | Marks cookies Secure. |
| 21 | Host header validation | check | Rejects unknown Host headers. |
| 22 | Allowed domains | text | Host allow-list. |
| 23 | Add custom HTTP headers | check | Enables header injection. |
| 24 | Custom headers | multiline | Header lines sent by the server. |
| 25 | Reverse proxy support | check | Trusts forwarded headers. |
| 26 | Trusted proxies | text | Allowed proxy addresses. |
| 27 | Update dynamic DNS | check | Master switch for DDNS. |
| 28 | DDNS service | choice | DynDNS or No-IP. |
| 29 | Domain | text | Hostname to update. |
| 30 | DDNS username | text | Service login. |
| 31 | DDNS password | text | Service secret. |

Actions excluded: Copy/Rotate/Delete API key, subnet whitelist, Register.

The Web API itself is a separate surface: login via cookie or API key, then
REST endpoints for torrents, transfer info, sync, and app preferences. ripdl
does not plan server parity; its daemon RPC is the automation surface.

### Advanced (84)

A single searchable table. Type is the editor shown in the Value column.

| # | Setting | Type | Behavior |
|---:|---|---|---|
| 1 | Resume data storage type | choice | Fastresume files or experimental SQLite store. |
| 2 | Torrent content removing mode | choice | Permanent delete or move to trash. |
| 3 | Physical RAM usage limit | number | Cap qBittorrent's working set. |
| 4 | Process memory priority | choice | OS memory-pressure hint. |
| 5 | Bdecode depth limit | number | Maximum bencode nesting. |
| 6 | Bdecode token limit | number | Maximum bencode tokens. |
| 7 | Asynchronous I/O threads | number | Disk I/O worker count. |
| 8 | Hashing threads | number | Hashing/recheck worker count. |
| 9 | File pool size | number | Open file-handle cache. |
| 10 | Outstanding memory when checking | number | Memory allowed during rechecks. |
| 11 | Disk cache | number | libtorrent disk-cache size. |
| 12 | Disk cache expiry | number | Cache entry lifetime. |
| 13 | Disk queue size | number | Maximum queued disk bytes. |
| 14 | Disk I/O type | choice | libtorrent disk backend. |
| 15 | Disk I/O read mode | choice | OS read-cache behavior. |
| 16 | Disk I/O write mode | choice | Write-through/coalesced behavior. |
| 17 | Coalesce reads and writes | check | Merges adjacent disk operations. |
| 18 | Piece extent affinity | check | Groups piece reads for locality. |
| 19 | Send upload piece suggestions | check | Suggests pieces to peers. |
| 20 | Send buffer watermark | number | Target send-buffer size. |
| 21 | Send buffer low watermark | number | Minimum send-buffer size. |
| 22 | Send buffer watermark factor | number | Adaptive buffer scaling. |
| 23 | Outgoing connections per second | number | Connect-rate cap. |
| 24 | Socket send buffer size | number | 0 uses the OS default. |
| 25 | Socket receive buffer size | number | 0 uses the OS default. |
| 26 | Socket backlog size | number | Listen queue length. |
| 27 | Save resume data interval | number | Fastresume flush period; 0 disables. |
| 28 | Save statistics interval | number | Statistics flush period. |
| 29 | `.torrent` file size limit | number | Rejects oversized metadata. |
| 30 | Outgoing ports (min) | number | Lower bound for outgoing connections. |
| 31 | Outgoing ports (max) | number | Upper bound. |
| 32 | UPnP lease duration | number | Router mapping lifetime. |
| 33 | DSCP for peer connections | number | QoS marking on peer traffic. |
| 34 | µTP-TCP mixed mode | choice | Preference algorithm between the two transports. |
| 35 | Hostname resolver cache TTL | number | DNS cache lifetime. |
| 36 | Support IDN | check | Allows internationalized tracker/webseed names. |
| 37 | Allow multiple connections per IP | check | Permits several peers behind one address. |
| 38 | Validate HTTPS tracker certificates | check | Enforces TLS validation for trackers/webseeds. |
| 39 | SSRF mitigation | check | Restricts tracker/webseed access to local networks. |
| 40 | Disallow peers on privileged ports | check | Refuses peers on ports below 1024. |
| 41 | Recheck torrents on completion | check | Verifies data after download. |
| 42 | Custom application instance name | text | Changes the single-instance identity. |
| 43 | Refresh interval | number | UI refresh period. |
| 44 | Resolve peer countries | check | GeoIP lookup for peer flags. |
| 45 | Resolve peer host names | check | Reverse DNS for peers. |
| 46 | Network interface | choice | Bind outgoing traffic to an interface. |
| 47 | Optional IP to bind | choice | Bind to a specific local address. |
| 48 | IP reported to trackers | text | Overrides the announce address. |
| 49 | Port reported to trackers | number | Overrides the announce port. |
| 50 | Max concurrent HTTP announces | number | Tracker request concurrency. |
| 51 | Stop tracker timeout | number | Time allowed for a stop announce. |
| 52 | Display notifications | check | Enables desktop notifications. |
| 53 | Notifications for added torrents | check | Noisy add notifications. |
| 54 | Notification timeout | number | How long notifications stay. |
| 55 | Reannounce on IP/port change | check | Forces a tracker update. |
| 56 | Download tracker favicon | check | Fetches tracker icons. |
| 57 | Save path history length | number | Number of recent paths kept. |
| 58 | Enable speed graphs | check | Shows the speed plot. |
| 59 | Enable icons in menus | check | Icon decoration in menus. |
| 60 | Attach add-torrent dialog | check | Embeds it in the main window. |
| 61 | Enable embedded tracker | check | Runs a local tracker. |
| 62 | Embedded tracker port | number | Local tracker listen port. |
| 63 | Port forwarding for embedded tracker | check | Attempts a router mapping. |
| 64 | Mark of the Web (Windows) | check | Tags downloaded files with MOTW. |
| 65 | Ignore SSL errors | check | Disables certificate validation app-wide. |
| 66 | Python executable path | path | Interpreter used by search plugins. |
| 67 | Start session paused | check | All torrents start stopped. |
| 68 | Session shutdown timeout | number | Grace period on exit. |
| 69 | Upload slots behavior | choice | Fixed slots or upload-rate based. |
| 70 | Upload choking algorithm | choice | Fastest, round-robin, or anti-leech. |
| 71 | Confirm torrent recheck | check | Ask before rechecking. |
| 72 | Confirm removing all tags | check | Ask before bulk tag removal. |
| 73 | Confirm tracker removal from all | check | Ask before bulk tracker removal. |
| 74 | Announce to all trackers in tier | check | Ignores normal tier backoff. |
| 75 | Announce to all tiers | check | Announces to every tier at once. |
| 76 | Peer turnover disconnect % | number | Churn threshold for dropping peers. |
| 77 | Peer turnover threshold % | number | Minimum eligible peer percentage. |
| 78 | Peer turnover interval | number | Seconds between turnover checks. |
| 79 | Max outstanding requests per peer | number | Request-queue depth. |
| 80 | DHT bootstrap nodes | text | Custom bootstrap list. |
| 81 | I2P inbound quantity | number | I2P tunnel quantity. |
| 82 | I2P outbound quantity | number | I2P tunnel quantity. |
| 83 | I2P inbound length | number | I2P tunnel length. |
| 84 | I2P outbound length | number | I2P tunnel length. |

Deprecated options exist in the wiki but are not in the 5.2.4 dialog and are not
counted.

## 3. Add-torrent dialog (not Preferences, but defaults live here)

| Option | Type | Behavior |
|---|---|---|
| Save at | path | Per-add destination. |
| Use another path for incomplete torrent | check + path | Per-add temp area. |
| Remember last used save path | check | Sticky destination. |
| Category | choice | Assigns a category. |
| Set as default category | check | Makes the choice sticky. |
| Tags | text | Comma-separated tags. |
| Start torrent | check | Start immediately vs add paused. |
| Stop condition | choice | None, metadata, or files checked. |
| Add to top of queue | check | Priority insert. |
| Skip hash check | check | Trusts existing data. |
| Download in sequential order | check | Piece order for streaming. |
| Download first and last pieces first | check | Speeds up container headers/tails. |
| Do not delete `.torrent` file | check | Keeps the source metadata. |
| Content layout | choice | Original, subfolder, or flat. |
| File tree | tree | Per-file include/exclude and priority. |
| Save as `.torrent` | action | Exports the metainfo. |

The Preferences "Downloads" page supplies the defaults for many of these:
dialog visibility, content layout, start-paused, queue top, and stop condition.

## 4. Torrent task context menu / row actions

Evidence: row menu construction in `transferlistwidget.cpp`. Actions marked
"dialog" live in the per-torrent options dialog rather than the right-click
menu.

| Action | Group | Behavior |
|---|---|---|
| Start / Resume | transfer | Starts or resumes the torrent. |
| Stop / Pause | transfer | Pauses the torrent. |
| Force start | transfer | Starts even when queue limits would block it. |
| Remove | transfer | Removes the torrent; optional file deletion. |
| Set location | storage | Moves content and updates the save path. |
| Rename | metadata | Renames the torrent/root folder. |
| Automatic Torrent Management | storage | Tri-state: let the category choose paths. |
| Torrent options | dialog | Per-torrent save path, speed limits, share limits, DHT/PeX/LSD, sequential, first/last. |
| Upload speed limit | dialog | Per-torrent cap, bounded by the global cap. |
| Download speed limit | dialog | Per-torrent cap. |
| Share ratio / seeding time / action | dialog | Per-torrent override, including remove or super-seed. |
| Super seeding mode | seeding | Tri-state; pretends to be a new leecher. |
| Sequential download | transfer | Tri-state; downloads pieces in order. |
| First/last pieces first | transfer | Tri-state; prioritizes the ends. |
| Force recheck | integrity | Re-hashes all data. |
| Force reannounce | tracker | Sends an immediate announce. |
| Queue: move to top | queue | Highest queue priority. |
| Queue: move up | queue | One step up. |
| Queue: move down | queue | One step down. |
| Queue: move to bottom | queue | Lowest priority. |
| Category submenu | organize | New/reset/assign category. |
| Tags submenu | organize | Add tags / remove all. |
| Edit trackers | tracker | Add, edit, remove tracker URLs. |
| Open destination folder | storage | Opens the save path in the file manager. |
| Preview file | content | Opens a media preview when possible. |
| Copy name | clipboard | Torrent display name. |
| Copy info hash v1 | clipboard | v1 hash. |
| Copy info hash v2 | clipboard | v2 hash. |
| Copy magnet link | clipboard | Magnet URI. |
| Copy torrent ID | clipboard | Internal ID. |
| Copy comment | clipboard | Torrent comment. |
| Copy content path | clipboard | On-disk content path. |
| Export `.torrent` | metadata | Writes the current metainfo to a file. |

Related non-menu columns/toolbar: progress, status, seeds/peers, download and
upload speed, ETA, ratio, category, tags, save path, added-on, completed-on,
amounts, session amounts. Double-click behavior is configurable in Preferences.
Per-torrent speed limits are reached through the torrent-options dialog, not a
right-click entry.

## 5. ripdl gap matrix

Status reflects ripdl after the 2026-10-19 seeding pass: HTTP engine + daemon
queue, librqbit torrents with pre-add file selection, sequential mode, details
tabs, NIC bonding, persistent seeding (default ratio 1.0), torrents restored
into the session at boot, and live upload/ratio/peer stats in the row and
details summary.

| Feature | qBittorrent behavior | ripdl status | Priority | Rationale |
|---|---|---|---|---|
| Add magnet / `.torrent` / URL | All three, plus clipboard/watch folder | have | — | Core torrent intake already works. |
| Pre-add file selection | File tree with include/exclude | have | — | GUI checklist and `--only` exist. |
| Per-file priority levels | Skip/normal/high per file | partial | P2 | Selection exists; priority levels do not. |
| Add paused / start paused default | "Do not start automatically" + add-dialog toggle | missing | P1 | Basic expected control; avoids surprise traffic. |
| Skip hash check on add | Add-dialog checkbox | missing | P2 | Useful for trusted re-adds; librqbit may always verify. |
| Sequential download | Toggle per torrent and default | have | — | Already exposed. |
| First/last pieces first | Per-torrent tri-state | missing | P2 | Cheap streaming win on media torrents. |
| Persistent seeding after completion | Stays seeding until limits | have | — | Daemon watcher serves finished torrents; boot restore re-attaches them. |
| Live upload speed / uploaded / ratio / peers | Row and details | have | — | Row shows ↑ rate; details summary shows ratio/peers. |
| Start/stop seeding action | Row action | have | — | Row toggle + context menu; stop keeps the torrent inspectable. |
| Ratio/time seeding limits | Ratio, total time, inactive time + action | partial | P1 | Global ratio/time stop implemented; per-torrent overrides and remove/super-seed actions missing. |
| Super seeding | Tri-state per torrent | missing | backlog | Niche; only after normal seeding is solid. |
| Max active downloads/seeds/total | Three queue limits | missing | P1 | Beyond `max_concurrent`; lets seeding coexist with downloads. |
| Slow-torrent exclusion | Rate thresholds + timer | missing | P2 | Reduces queue stalls on dead torrents. |
| Queue reorder / priority | Top/up/down/bottom + add-to-top | missing | P1 | FIFO alone starves urgent downloads. |
| Force start (bypass queue) | Per-torrent | missing | P2 | Needed once queue limits exist. |
| Per-torrent speed limits | Upload/download in torrent options | missing | P1 | Most-requested torrent control after pause. |
| Global + alternative speed limits | Global caps, scheduled alt caps | partial | P1 | Shared limiter/config exists; UI and schedule windows are limited. |
| Global/per-torrent connection limits | Connections and upload slots | missing | P2 | librqbit defaults may suffice initially. |
| Encryption mode | Allow / require / disable | missing | P2 | Policy control; no UI today. |
| DHT / PeX / LSD toggles | Three privacy switches | missing | P2 | Users expect to disable discovery. |
| Anonymous mode | Hides client identity | deliberately not planned | — | Low value for a download manager; complexity/privacy theatre. |
| Proxy (SOCKS/HTTP) | Global + per-scope | missing | P2 | Valuable but a large surface; HTTP proxy may come first. |
| IP filter / peer ban list | File list + manual bans | missing | backlog | Niche for a DM. |
| Port forwarding / UPnP / NAT-PMP | Listening port mapping | missing | backlog | librqbit uses ephemeral ports; adding NAT is invasive. |
| Listening port / protocol choice | TCP+µTP, fixed port | missing | backlog | Same. |
| Force recheck | Row action | missing | P1 | Trust/recovery action after crashes or moved files. |
| Force reannounce | Row action | missing | P2 | Tracker recovery; cheap via librqbit. |
| Tracker editing | Add/edit/remove trackers | missing | P2 | Needed for dead/private trackers. |
| Export `.torrent` | Row action | missing | P2 | Backup/migration; easy metadata write. |
| Copy magnet / hashes / name | Copy submenu | missing | P2 | Low effort, high convenience. |
| Rename torrent | Row action | missing | P2 | Fixes bad root folder names. |
| Set location / move content | Per-torrent | partial | P2 | GUI "Save to" sets a destination; moving existing content is not proven. |
| Category | Category tree + auto paths | missing | P2 | Useful organization; tags may cover most needs. |
| Tags | Multi-tag assignment | missing | P2 | Cross-category labelling; simpler than categories. |
| Per-torrent share-ratio override | Torrent options dialog | missing | backlog | Only after global seeding rules land. |
| Torrent details: files/peers/trackers | Three tabs, live refresh | have | — | Already implemented with 2 s refresh. |
| Peer banning | Manual ban by IP | missing | backlog | Niche. |
| Watched folders | Auto-add from directories | missing | backlog | Convenience; daemon-side watcher. |
| RSS reader + rules | Feeds, filters, auto-add | deliberately not planned | — | Explicitly deferred unless requested. |
| Search plugins | External engines | deliberately not planned | — | Out of scope for a DM. |
| Email notification | SMTP on completion | missing | backlog | Low demand; OS notifications cover most. |
| Run external program hooks | On add/finish with placeholders | partial | P2 | yt-dlp sidecar exists; generic hooks do not. |
| Web UI + API key/auth | Full remote UI and REST API | deliberately not planned | — | ripdl has local RPC/GUI; exposing HTTP is a security surface. |
| Global scheduler windows | Time/day rate schedule | partial | P2 | HTTP has `--at`/`--after`; no global window. |
| Pre-allocation | Reserve disk up front | missing | backlog | librqbit/storage may handle sparse files. |
| Incomplete-file marker | `.!qB` suffix | missing | backlog | Cosmetic/packaging concern. |
| Excluded file names | Skip patterns on add | missing | backlog | Niche. |
| Speed graphs / long-term stats | Speed widget and statistics | missing | backlog | Nice-to-have UI polish. |
| Tray, single instance, update check | Desktop integration | have | — | Already implemented in ripdl. |
| File associations | `.torrent` + magnet | missing | backlog | Installer concern, not engine. |

Notes on deliberate non-parity:

- ripdl's queue is a **download manager queue**, not a seedbox scheduler. It
  should adopt qB's limits only where they protect interactive downloads.
- WebUI/API parity is rejected because ripdl's threat model is a local daemon,
  and its RPC already covers automation.
- Anonymous mode, search plugins, RSS, and broad discovery presets are out of
  scope; they add surface without serving the core "fast, private downloader"
  goal.

## 6. Recommended next 5

1. **Per-torrent and global bandwidth limits** (upload/download, with the
   existing shared limiter and an optional scheduled alternative cap). qB's
   Speed page is small but heavily used; a download manager that cannot cap a
   torrent or the whole client feels unfinished.
2. **Queue policy** (max active downloads, seeds, total; slow-torrent exclusion;
   move top/up/down/bottom; force start). This lets seeding run alongside new
   downloads without starving them, and prevents the current
   `max_concurrent`-only behavior from becoming a bottleneck.
3. **Integrity and recovery actions** (force recheck, force reannounce, export
   `.torrent`, copy magnet/hash/name). These are cheap librqbit calls with high
   trust value after crashes, moved files, or dead trackers.
4. **Add-torrent defaults and per-torrent overrides** (start paused, skip hash
   check, first/last pieces, sequential, per-torrent speed and share limits).
   This matches qB's add workflow and makes the existing file-checklist dialog
   feel complete without adopting categories, RSS, or a Web UI.
5. **Tracker editing + peer visibility polish** (add/edit/remove trackers,
   seed counts if librqbit exposes them). Torrents live and die by trackers;
   the details dialog already has the tab.

Deliberately not in the next five: categories/tags, proxy, IP filtering, NAT
traversal, RSS, WebUI parity, super seeding, peer banning. Those are either
niche for a download manager or disproportionately large.
