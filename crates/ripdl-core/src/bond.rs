//! The bond authority: the machine's live, weight-managed internet uplinks.
//!
//! One instance (usually one per daemon) is shared by the HTTP engine and
//! the torrent session. It owns:
//!
//! * the **link set** — what discovery last found (with reachability run by
//!   the caller), swapped in whole (`set_links`), so NIC events apply at the
//!   next tick instead of the next daemon restart;
//! * **metering** — every byte each stack reads through a bound connection
//!   is attributed to its link (`record`); no probe traffic exists;
//! * **weighting** — `sample()` folds the bytes since the last sample into
//!   per-link EMA rates and derives a *preference* (1..~10). This is never
//!   a ban: a working link always keeps weight >= 1, because maximizing
//!   throughput is the caller's job (adaptive chunks + stealing), not the
//!   filter's. Only a link whose *dial fails* twice in a row is excluded,
//!   and even that is re-probed every explore window;
//! * **picking** — `pick()` hands out the next link by smooth weighted
//!   round-robin (3:1 preferences put 3 of 4 dials on the fast link). A
//!   link that failed its dial twice in a row is skipped on the spot; it
//!   does not wait for the sampler to notice.
//!
//! The weights deliberately carry NO tuned correctness: they are a soft
//! ordering of candidates. The real balancing lives in the caller: claim
//! chunks sized by the link's own measured rate (`chunks_for`), and steal
//! the largest remaining cell when a worker frees up. Any bug in the
//! delays the spread instead of capping it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// One discovered uplink as the caller (discovery pass) sees it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkInfo {
    /// The bindable source address. The default-route link also has one;
    /// callers that don't bind know they can still use it unbound.
    pub ip: IpAddr,
    /// Display name ("Wi-Fi", "Ethernet 2", interface name on Unix).
    pub name: String,
    /// The interface the OS routing table already prefers.
    pub default_route: bool,
}

/// Meters + weights for one link. The hot path (`record`) touches only the
/// atomics; `sample`/`pick` take its short mutex.
pub struct Link {
    pub info: LinkInfo,
    delta: AtomicU64,
    /// Connections (segments/peers) currently dialing or streaming here.
    in_use: AtomicU32,
    inner: Mutex<LinkInner>,
}

#[derive(Debug, Default, Clone)]
struct LinkInner {
    /// EMA of measured bytes/second (0 until the first sample with bytes).
    rate: f64,
    /// Relative share of new work (0 = quarantined).
    weight: u32,
    /// Consecutive dial failures on this link (0 = fine).
    fails: u32,
}

/// Serializable snapshot for RPC/UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkStatus {
    pub name: String,
    pub ip: IpAddr,
    pub default_route: bool,
    /// Smoothed measured rate, bytes/second (idle links keep their last EMA).
    pub rate_bps: u64,
    /// Current relative weight (0 = quarantined / skipped).
    pub weight: u32,
    /// Connections currently using this link.
    pub in_use: u32,
    /// Consecutive dial failures (0 = healthy).
    pub fails: u32,
}

impl serde::Serialize for Link {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.status().serialize(serializer)
    }
}

impl Link {
    pub fn status(&self) -> LinkStatus {
        let inner = self.inner.lock().unwrap();
        LinkStatus {
            name: self.info.name.clone(),
            ip: self.info.ip,
            default_route: self.info.default_route,
            rate_bps: inner.rate as u64,
            weight: inner.weight,
            in_use: self.in_use.load(Ordering::Relaxed),
            fails: inner.fails,
        }
    }
    /// Bytes since the last sample, then zeroed.
    fn take_delta(&self) -> u64 {
        self.delta.swap(0, Ordering::Relaxed)
    }
    fn note_fail(&self, skip_after: u32) {
        let mut inner = self.inner.lock().unwrap();
        inner.fails = inner.fails.saturating_add(1);
        // The OS-route link is fail-counted but never skippable: someone
        // must always be able to dial even if every bound link dies.
        if !self.info.default_route && inner.fails >= skip_after {
            inner.weight = 0;
        }
    }
    fn note_ok(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.fails = 0;
        // A successful dial outranks any skip state: restore at least the
        // floor preference right away (the sampler will re-scale later).
        if inner.weight == 0 {
            inner.weight = 1;
        }
    }
    fn weight(&self) -> u32 {
        self.inner.lock().unwrap().weight
    }
}

/// A link whose dial fails this many times in a row is skipped for the
/// explore window (re-probed with weight 1 next window). Working links are
/// never excluded by any measured-speed rule — slowness throttles them
/// through shrinking chunk sizes instead (`chunks_for`).
const DIAL_FAIL_SKIP: u32 = 2;

/// How often (in samples) skip-listed links get weight 1 again for one
/// window (a probe dial), and how often preferences re-normalize.
pub const EXPLORE_EVERY: Duration = Duration::from_secs(60);

/// Claim-size bounds: a dial must be worth its TLS handshake, and one
/// claimant must never park a whole multi-GB cell invisibly.
pub const MIN_CHUNK: u64 = 256 * 1024;
pub const MAX_CHUNK: u64 = 64 * 1024 * 1024;

/// Choose the size of the next chunk to claim on a link. `file_remaining`
/// caps the claim. The claim is proportional to the link's own measured
/// rate — no cross-link ratio is computed, so nothing is tuned: a 300 Mbps
/// link takes ~8x the bite of a 100 Mbps link because *it measured itself*.
pub fn chunks_for(rate_bps: f64, file_remaining: u64) -> u64 {
    const CHUNK_SECS: f64 = 2.0;
    if rate_bps <= 0.0 {
        // No measurement yet (or idle forever): small probe-sized claim so
        // the link gets to meter without hogging a range.
        return file_remaining.min(MIN_CHUNK);
    }
    let want = (rate_bps * CHUNK_SECS) as u64;
    want.clamp(MIN_CHUNK, MAX_CHUNK).min(file_remaining).max(1)
}

impl std::fmt::Debug for Bond {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Bond")
            .field(&self.links.read().unwrap().len())
            .finish()
    }
}

#[derive(Default)]
pub struct Bond {
    links: RwLock<Vec<Arc<Link>>>,
    /// Smooth weighted-RR credit per pick, keyed by link address.
    credit: Mutex<HashMap<IpAddr, i64>>,
    last_explore: Mutex<std::option::Option<Instant>>,
}

impl Bond {
    pub fn new() -> Self {
        Self::default()
    }

    /// Swap the discovered link set in whole. History (rate EMA, weights)
    /// follows the address when it is still present — a Wi-Fi roaming to a
    /// new address starts as weight 1, a returning tether keeps its old
    /// learned rate. Departed links vanish; vanished credit entries are
    /// cleaned on the next pick.
    pub fn set_links(&self, links: &[LinkInfo]) {
        let mut next: Vec<Arc<Link>> = Vec::with_capacity(links.len());
        let current = self.links.read().unwrap();
        for info in links {
            if let Some(old) = current.iter().find(|l| l.info == *info) {
                next.push(Arc::clone(old));
                continue;
            }
            // Same address, new name/default flag: keep the history.
            if let Some(old) = current.iter().find(|l| l.info.ip == info.ip) {
                let inner = old.inner.lock().unwrap().clone();
                next.push(Arc::new(Link {
                    info: info.clone(),
                    delta: AtomicU64::new(old.delta.load(Ordering::Relaxed)),
                    in_use: AtomicU32::new(old.in_use.load(Ordering::Relaxed)),
                    inner: Mutex::new(inner),
                }));
                continue;
            }
            next.push(Arc::new(Link {
                info: info.clone(),
                delta: AtomicU64::new(0),
                in_use: AtomicU32::new(0),
                inner: Mutex::new(LinkInner {
                    rate: 0.0,
                    // A brand-new link starts at weight 1: it must get a
                    // chance to meter before any quarantine logic sees it.
                    weight: 1,
                    fails: 0,
                }),
            }));
        }
        drop(current);
        *self.links.write().unwrap() = next;
    }

    /// Current links (a clone of the Arcs) for the caller's own pass.
    pub fn links(&self) -> Vec<Arc<Link>> {
        self.links.read().unwrap().clone()
    }

    pub fn status(&self) -> Vec<LinkStatus> {
        self.links
            .read()
            .unwrap()
            .iter()
            .map(|l| l.status())
            .collect()
    }

    /// Attribute `bytes` to the link a connection is bound to.
    pub fn record(&self, link: &Link, bytes: u64) {
        link.delta.fetch_add(bytes, Ordering::Relaxed);
    }

    /// A dial on this link is in flight.
    pub fn begin(&self, link: &Link) {
        link.in_use.fetch_add(1, Ordering::Relaxed);
    }
    /// The dial is done (success or failure).
    pub fn end(&self, link: &Link) {
        link.in_use.fetch_sub(1, Ordering::Relaxed);
    }

    /// A dial through this link failed (counted for every link; only
    /// non-default links are ever skipped by the sampler).
    pub fn note_fail(&self, link: &Link) {
        link.note_fail(DIAL_FAIL_SKIP);
    }
    /// A dial through this link succeeded.
    pub fn note_ok(&self, link: &Link) {
        link.note_ok();
    }

    /// Smooth weighted round-robin over healthy links. Weight 0 links are
    /// skipped entirely; the returned link is the one the caller should bind
    /// (or treat as OS-routed when `default_route`). `None` = no healthy
    /// link: the caller falls back to the plain mode.
    pub fn pick(&self) -> Option<Arc<Link>> {
        let candidates = self.links.read().unwrap().clone();
        let mut credit = self.credit.lock().unwrap();
        credit.retain(|ip, _| candidates.iter().any(|l| l.info.ip == *ip));
        let mut total = 0i64;
        let mut best: Option<(Arc<Link>, i64)> = None;
        for link in &candidates {
            let weight = link.weight() as i64;
            if weight <= 0 {
                credit.insert(link.info.ip, 0);
                continue;
            }
            total += weight;
            let entry = credit.entry(link.info.ip).or_insert(0);
            *entry += weight;
            if best.as_ref().map(|(_, b)| *entry > *b).unwrap_or(true) {
                best = Some((Arc::clone(link), *entry));
            }
        }
        let (pick, _) = best?;
        if let Some(c) = credit.get_mut(&pick.info.ip) {
            *c -= total;
        }
        Some(pick)
    }

    /// Fold the metered bytes since the last call into the per-link EMA and
    /// re-derive weights. `dt` is the seconds since the previous sample.
    /// Returns the status snapshot for RPC/UI.
    pub fn sample(&self, dt: f64) -> Vec<LinkStatus> {
        let links = self.links.read().unwrap().clone();
        let dt = dt.max(0.1);

        // EMA update first.
        for link in &links {
            let bytes = link.take_delta();
            let mut inner = link.inner.lock().unwrap();
            let sample = bytes as f64 / dt;
            inner.rate = if inner.rate == 0.0 {
                sample
            } else {
                inner.rate * 0.6 + sample * 0.4
            };
        }

        // Explore window: everyone gets weight 1 for this round. The very
        // first sample only arms the clock - there is nothing learned yet
        // that a probe could wash out.
        let explore = {
            let mut last = self.last_explore.lock().unwrap();
            let hit = match *last {
                None => {
                    *last = Some(Instant::now());
                    false
                }
                Some(t) => {
                    if t.elapsed() >= EXPLORE_EVERY {
                        *last = Some(Instant::now());
                        true
                    } else {
                        false
                    }
                }
            };
            hit
        };

        let max_rate = links
            .iter()
            .map(|l| l.inner.lock().unwrap().rate)
            .fold(0.0f64, f64::max);

        for link in &links {
            let mut inner = link.inner.lock().unwrap();
            // Dead dials outrank any rate math: skip list, unless this
            // window is the re-probe. The OS-route link is exempt: someone
            // must always be able to dial.
            let skip_eligible = !link.info.default_route;
            if skip_eligible && inner.fails >= DIAL_FAIL_SKIP {
                inner.weight = if explore { 1 } else { 0 };
                continue;
            }
            if explore {
                // Probe window: hand everyone (skip-listed links included,
                // handled above) the floor weight so a recovered link can
                // prove itself without waiting for its EMA.
                inner.weight = 1;
                continue;
            }
            // No comparative traffic yet: keep the current preference (a
            // fresh link defaults to 1; an idle link keeps what it earned).
            if max_rate <= 0.0 || inner.rate == 0.0 {
                continue;
            }
            // Soft preference 1..10: never 0, never a ban. The measured
            // rate only orders candidates; caller-side chunk sizing and
            // stealing do the actual balancing.
            let w = (inner.rate / max_rate * 10.0).round() as u32;
            inner.weight = w.clamp(1, 10);
        }

        self.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn addr(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    fn link_info(n: u8, name: &str, default_route: bool) -> LinkInfo {
        LinkInfo {
            ip: addr(n),
            name: name.to_string(),
            default_route,
        }
    }

    fn link(b: &Bond, n: u8) -> Arc<Link> {
        b.links()
            .into_iter()
            .find(|l| l.info.ip == addr(n))
            .unwrap()
    }

    #[test]
    fn pick_respects_smooth_weights() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "fast", true), link_info(2, "slow", false)]);
        let fast = link(&b, 1);
        let slow = link(&b, 2);
        fast.inner.lock().unwrap().weight = 3;
        slow.inner.lock().unwrap().weight = 1;
        let picks: Vec<u8> = (0..4)
            .map(|_| {
                let p = b.pick().unwrap();
                match p.info.ip {
                    IpAddr::V4(v) => v.octets()[3],
                    _ => unreachable!(),
                }
            })
            .collect();
        assert_eq!(picks.iter().filter(|p| **p == 1).count(), 3, "3 fast of 4");
        assert_eq!(picks.iter().filter(|p| **p == 2).count(), 1, "1 slow of 4");
        // Two slow picks can never be adjacent in a smooth RR.
        assert!(
            picks.windows(2).any(|w| w == &[1, 2]) || picks.windows(2).any(|w| w == &[2, 1]),
            "the lone slow pick sits next to fast ones: {:?}",
            picks
        );
    }

    #[test]
    fn pick_skips_weight_zero() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "a", true), link_info(2, "b", false)]);
        link(&b, 2).inner.lock().unwrap().weight = 0;
        for _ in 0..5 {
            assert_eq!(b.pick().unwrap().info.ip, addr(1));
        }
    }

    #[test]
    fn empty_and_quarantined_bond_picks_none() {
        let b = Bond::new();
        assert!(b.pick().is_none());
        b.set_links(&[link_info(1, "a", false)]);
        link(&b, 1).inner.lock().unwrap().weight = 0;
        assert!(b.pick().is_none());
    }

    #[test]
    fn two_consecutive_dial_fails_skip_the_link_at_once() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "up", true), link_info(2, "down", false)]);
        let down = link(&b, 2);
        down.inner.lock().unwrap().weight = 10;
        b.note_fail(&down);
        // One failure is not enough; the link stays pickable.
        assert!(b.pick().is_some());
        // ... includes the failed link still.
        assert!(b.links().iter().any(|l| l.info.ip == addr(2)));
        b.note_fail(&down);
        for _ in 0..3 {
            assert_eq!(
                b.pick().unwrap().info.ip,
                addr(1),
                "after two fails the pick never lands on the dead link"
            );
        }
        assert_eq!(down.status().weight, 0, "the dead link is weight-0");
        // A success brings it back at once.
        b.note_ok(&down);
        assert_eq!(down.status().fails, 0);
        assert!(down.status().weight >= 1, "note_ok un-skips the link");
    }

    #[test]
    fn default_route_link_never_fails_out() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "default", true)]);
        let l = link(&b, 1);
        b.note_fail(&l);
        b.note_fail(&l);
        b.note_fail(&l);
        assert_eq!(l.status().fails, 3, "fails are counted");
        assert_eq!(l.status().weight, 1, "the OS route can never be skipped");
        assert!(b.pick().is_some());
    }

    #[test]
    fn sampling_weights_follow_measured_rates() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "fast", true), link_info(2, "slow", false)]);
        let fast = link(&b, 1);
        let slow = link(&b, 2);
        fast.inner.lock().unwrap().weight = 1;
        slow.inner.lock().unwrap().weight = 1;

        // 300 MB/s vs 100 MB/s over a 5 s window.
        b.record(&fast, 300 * 1024 * 1024 * 5);
        b.record(&slow, 100 * 1024 * 1024 * 5);
        let statuses = b.sample(5.0);
        let by_ip: HashMap<IpAddr, LinkStatus> = statuses.into_iter().map(|s| (s.ip, s)).collect();
        let f = &by_ip[&addr(1)];
        let s = &by_ip[&addr(2)];
        assert!(
            f.weight == 10,
            "fast link saturates the scale, got {}",
            f.weight
        );
        assert!(
            s.weight == 3,
            "100/300 of the best scales to 3, got {}",
            s.weight
        );

        // A tiny-but-working link keeps its floor: the EMA smooths the
        // small sample against the learned rate (60 MB/s here), so the
        // exact value varies — but it is never weight-0. Slowness shrinks
        // chunk sizes in the caller; it is never a ban (the flexible
        // maximizer is stealing + sized claims, not filters).
        b.record(&fast, 300 * 1024 * 1024 * 5);
        b.record(&slow, (0.01 * 1024.0 * 1024.0 * 5.0) as u64);
        let statuses = b.sample(5.0);
        let by_ip: HashMap<IpAddr, LinkStatus> = statuses.into_iter().map(|s| (s.ip, s)).collect();
        assert!(
            (1..=3).contains(&by_ip[&addr(2)].weight),
            "EMA smooths the tiny sample back toward the learned rate, got {}",
            by_ip[&addr(2)].weight
        );
    }

    #[test]
    fn chunk_size_grows_with_the_links_own_rate() {
        // Measured rate -> claim size. No cross-link ratio is computed: the
        // link sizes its own bite, so any mix of speeds splits by whatever
        // each link actually delivers.
        let fast = chunks_for(37.5 * 1024.0 * 1024.0, u64::MAX);
        let slow = chunks_for(12.5 * 1024.0 * 1024.0, u64::MAX);
        assert_eq!(fast, MAX_CHUNK, "300 Mbps clamps at the 64 MiB cap");
        assert_eq!(
            slow, 26_214_400,
            "100 Mbps claims 2 s worth (12.5 MiB/s x 2)"
        );
        assert!(
            fast > slow * 2,
            "the fast link visibly out-bites the slow one"
        );
        // Unmeasured rate: a probe-sized claim that still meters rates.
        assert_eq!(chunks_for(0.0, u64::MAX), MIN_CHUNK);
        // A claim never exceeds what is left of the file/cell.
        assert_eq!(chunks_for(37.5 * 1024.0 * 1024.0, 1_000), 1_000);
    }

    #[test]
    fn idle_link_loses_no_weight() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "a", true), link_info(2, "b", false)]);
        link(&b, 2).inner.lock().unwrap().weight = 5;
        // Link 2 gets no bytes at all this window.
        b.record(&link(&b, 1), 100 * 1024 * 1024);
        let statuses = b.sample(5.0);
        let by_ip: HashMap<IpAddr, LinkStatus> = statuses.into_iter().map(|s| (s.ip, s)).collect();
        assert_eq!(by_ip[&addr(2)].weight, 5, "no traffic is not a slump");
    }

    #[test]
    fn explore_window_resets_weights() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "a", true), link_info(2, "b", false)]);
        link(&b, 2).inner.lock().unwrap().weight = 0; // quarantined
                                                      // Force the explore window.
        *b.last_explore.lock().unwrap() = Some(Instant::now() - EXPLORE_EVERY);
        b.record(&link(&b, 1), 1024 * 1024);
        let statuses = b.sample(5.0);
        let by_ip: HashMap<IpAddr, LinkStatus> = statuses.into_iter().map(|s| (s.ip, s)).collect();
        assert_eq!(by_ip[&addr(2)].weight, 1, "explore unbans everyone");
    }

    #[test]
    fn link_history_follows_address_through_renames() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "Wi-Fi", false)]);
        {
            let l = link(&b, 1);
            l.inner.lock().unwrap().rate = 7.0 * 1024.0 * 1024.0;
        }
        // Same IP renamed (interface re-registered).
        b.set_links(&[link_info(1, "Wi-Fi 2", false)]);
        let s = b.status();
        assert_eq!(s[0].name, "Wi-Fi 2");
        assert_eq!(
            s[0].rate_bps,
            (7.0 * 1024.0 * 1024.0) as u64,
            "history kept"
        );
        // A different address starts clean at weight 1.
        b.set_links(&[link_info(1, "Wi-Fi", false), link_info(2, "new", false)]);
        assert_eq!(
            b.status().iter().find(|s| s.ip == addr(2)).unwrap().weight,
            1
        );
    }

    #[test]
    fn credit_map_drops_gone_links() {
        let b = Bond::new();
        b.set_links(&[link_info(1, "a", true)]);
        assert!(b.pick().is_some());
        b.set_links(&[link_info(2, "b", true)]);
        assert!(b.pick().is_some(), "pick survives a full link-set swap");
    }

    #[test]
    fn v6_links_are_first_class() {
        let b = Bond::new();
        let info = LinkInfo {
            ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
            name: "v6".into(),
            default_route: false,
        };
        b.set_links(&[info]);
        assert!(b.pick().is_some());
    }
}
