//! The real backend: evdev discovery, `EVIOCGRAB`, and uinput presenters.
//!
//! Linux-only, and the one module in `input/` that cannot be exercised in CI —
//! it needs `/dev/input`, `/dev/uinput` and a seat. Everything it decides has
//! been lifted out into the pure modules beside it; what is left here is the
//! syscall glue.
//!
//! # How a grab is released
//!
//! `EVIOCGRAB` is a property of the **open file description**, not of the
//! process's intent. It is released when that descriptor is closed, and the
//! kernel closes every descriptor a process holds when the process dies — by
//! `exit`, by `SIGTERM`, by `SIGKILL`, or by a panic. So:
//!
//! * A clean stop calls [`InputBackend::release`], which ungrabs and drops.
//! * An unclean stop — `SIGKILL`, an OOM kill, a segfault — releases every grab
//!   anyway, because there is no way for a dead process to keep a descriptor
//!   open. The same is true of the uinput presenters: their devices disappear
//!   when their descriptors close, so a killed core leaves no orphan device and
//!   no held controller.
//!
//! There is deliberately **no cleanup-on-exit handler**, because there is
//! nothing for one to do. That is the property that makes the disable path
//! trivially reachable: `enabled = false` plus a restart is sufficient, and so
//! is `systemctl kill`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, Device, EventStream, InputEvent, InputId,
    KeyCode, UinputAbsSetup,
};
use futures::stream::{FuturesUnordered, StreamExt};

use super::backend::{InputBackend, InputError};
use super::discovery::Candidate;
use super::presenter::{ev, AbsRange, Forward, PadProfile, SYN_REPORT};

/// evdev + uinput.
pub struct EvdevBackend {
    /// One per player slot, indexed by slot. Created once at session start and
    /// dropped only when this struct is (i.e. when the process ends).
    presenters: Vec<VirtualDevice>,
    /// Claimed pads, by devnode. Holding the stream here is holding the grab.
    pads: HashMap<PathBuf, EventStream>,
}

impl EvdevBackend {
    pub fn new() -> EvdevBackend {
        EvdevBackend {
            presenters: Vec::new(),
            pads: HashMap::new(),
        }
    }

    /// The next event from any claimed pad, tagged with its devnode.
    ///
    /// Pends forever when no pad is claimed, so a `select!` arm on it simply
    /// never fires rather than spinning.
    pub async fn next_event(&mut self) -> (PathBuf, std::io::Result<InputEvent>) {
        if self.pads.is_empty() {
            return std::future::pending().await;
        }
        let mut futs = FuturesUnordered::new();
        for (path, stream) in self.pads.iter_mut() {
            futs.push(async move { (path.clone(), stream.next_event().await) });
        }
        match futs.next().await {
            Some(next) => next,
            // Unreachable: the map is non-empty, so the set has at least one
            // future. Pend rather than panic if that ever stops being true.
            None => std::future::pending().await,
        }
    }
}

impl Default for EvdevBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn abs_range(info: &AbsInfo) -> AbsRange {
    AbsRange::new(info.minimum(), info.maximum(), info.fuzz(), info.flat())
}

/// Describe one enumerated device for the pure discovery gate.
fn describe(path: PathBuf, dev: &Device) -> Candidate {
    let id = dev.input_id();
    Candidate {
        path,
        name: dev.name().unwrap_or("unknown").to_string(),
        vendor: id.vendor(),
        product: id.product(),
        version: id.version(),
        bus: id.bus_type().0,
        uniq: dev.unique_name().map(str::to_string),
        phys: dev.physical_path().map(str::to_string),
        has_btn_south: dev
            .supported_keys()
            .is_some_and(|keys| keys.contains(KeyCode::BTN_SOUTH)),
    }
}

impl InputBackend for EvdevBackend {
    fn enumerate(&mut self) -> Result<Vec<Candidate>, InputError> {
        let mut devices: Vec<(PathBuf, Device)> = evdev::enumerate().collect();
        // Ascending devnode order, so the fleet's slot assignment is
        // deterministic across boots for a fixed set of pads.
        devices.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(devices
            .into_iter()
            .map(|(path, dev)| describe(path, &dev))
            .collect())
    }

    fn create_presenter(
        &mut self,
        slot: u8,
        profile: &PadProfile,
    ) -> Result<Vec<PathBuf>, InputError> {
        let fail = |detail: String| InputError::Presenter { slot, detail };

        let keys: AttributeSet<KeyCode> = profile.keys().iter().map(|&k| KeyCode::new(k)).collect();
        let name = PadProfile::device_name(slot);
        let mut builder = VirtualDevice::builder()
            .map_err(|e| fail(e.to_string()))?
            .name(&name)
            .input_id(InputId::new(
                BusType(profile.bus),
                profile.vendor,
                profile.product,
                profile.version,
            ))
            .with_keys(&keys)
            .map_err(|e| fail(e.to_string()))?;

        for &(code, range) in profile.axes() {
            // `value` starts at the axis's resting position, so a presenter
            // created before any pad connects reads as a controller at rest
            // rather than one with its triggers held or its sticks pinned.
            let info = AbsInfo::new(
                range.neutral(),
                range.min,
                range.max,
                range.fuzz,
                range.flat,
                0,
            );
            builder = builder
                .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode(code), info))
                .map_err(|e| fail(e.to_string()))?;
        }

        let mut device = builder.build().map_err(|e| fail(e.to_string()))?;

        // The kernel may not have created /dev/input/eventN by the instant
        // build() returns. Without the node we cannot claim ownership of it, and
        // the very next discovery poll would see a database-known pad it does
        // not recognise as ours and grab it — feeding our own output back in.
        // So retry briefly, and treat a persistent absence as a hard failure
        // rather than starting a session that will eat itself.
        let mut nodes = Vec::new();
        for attempt in 0..20 {
            match device.enumerate_dev_nodes_blocking() {
                Ok(found) => {
                    nodes = found.flatten().collect();
                    if !nodes.is_empty() {
                        break;
                    }
                }
                Err(e) => return Err(fail(format!("enumerating its devnodes: {e}"))),
            }
            if attempt < 19 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if nodes.is_empty() {
            return Err(fail(
                "its devnode never appeared, so discovery could not be taught to skip it".into(),
            ));
        }

        // The index IS the slot: `emit` indexes `presenters` by `slot as usize`,
        // so a presenter pushed at the wrong index silently routes one player's
        // input to another player's device. This was a `debug_assert_eq!`, which
        // compiles to nothing in exactly the release build the couch runs — the
        // check would have been absent precisely where the corruption matters.
        // A violated invariant here is not recoverable, so it fails the start.
        if self.presenters.len() != slot as usize {
            return Err(fail(format!(
                "presenters must be created in slot order: asked for slot {slot} with {} \
                 already created. Creating them out of order would index one player's \
                 input at another player's device",
                self.presenters.len()
            )));
        }
        self.presenters.push(device);
        Ok(nodes)
    }

    fn claim(&mut self, path: &Path) -> Result<BTreeMap<u16, AbsRange>, InputError> {
        let fail = |detail: String| InputError::Claim {
            path: path.to_path_buf(),
            detail,
        };

        let device = Device::open(path).map_err(|e| fail(e.to_string()))?;
        let source_axes: BTreeMap<u16, AbsRange> = device
            .get_absinfo()
            .map_err(|e| fail(format!("reading absinfo: {e}")))?
            .map(|(code, info)| (code.0, abs_range(&info)))
            .collect();

        let mut stream = device
            .into_event_stream()
            .map_err(|e| fail(format!("opening its event stream: {e}")))?;
        // The grab is the point. If it fails the pad is still readable by
        // everything else, so we must NOT keep it: a half-claimed pad would be
        // double-read — once by us onto the presenter and once by the game
        // directly — and every input would fire twice.
        stream
            .device_mut()
            .grab()
            .map_err(|e| fail(format!("EVIOCGRAB: {e}")))?;

        self.pads.insert(path.to_path_buf(), stream);
        Ok(source_axes)
    }

    fn release(&mut self, path: &Path) {
        let Some(mut stream) = self.pads.remove(path) else {
            return;
        };
        // Dropping the stream closes the descriptor, which releases the grab on
        // its own. The explicit ungrab is for immediacy and for the log: it
        // makes the release a thing that happened at a point in time rather
        // than a consequence of a drop somewhere.
        if let Err(e) = stream.device_mut().ungrab() {
            tracing::warn!("ungrabbing {}: {e}", path.display());
        }
        drop(stream);
    }

    fn emit(&mut self, slot: u8, forward: Forward) -> Result<(), InputError> {
        let device = self
            .presenters
            .get_mut(slot as usize)
            .ok_or_else(|| InputError::Emit {
                slot,
                detail: "no presenter exists for this slot".into(),
            })?;

        let event = match forward {
            Forward::Key { code, value } => InputEvent::new(ev::KEY, code, value),
            Forward::Abs { code, value } => InputEvent::new(ev::ABS, code, value),
            Forward::Sync => InputEvent::new(ev::SYN, SYN_REPORT, 0),
            // A drop never reaches here — the session counts it and returns —
            // but emitting nothing is the only correct reading of one anyway.
            Forward::Drop(_) => return Ok(()),
        };

        device.emit(&[event]).map_err(|e| InputError::Emit {
            slot,
            detail: e.to_string(),
        })
    }
}
