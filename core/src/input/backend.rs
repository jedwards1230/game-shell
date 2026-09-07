//! The hardware seam.
//!
//! Everything that touches `/dev/input`, `/dev/uinput` or an `ioctl` is behind
//! this trait, so [`super::session::InputSession`] — which holds every rule
//! about which pad is claimed, which slot it takes and what crosses onto a
//! presenter — is exercised in CI on a host with no seat, no `/dev/uinput` and
//! no pad.
//!
//! The backend owns the open devices, including their event streams. A claimed
//! pad's stream is never handed out, because the file descriptor **is** the
//! grab: `EVIOCGRAB` lives on the open file description, so whoever holds the fd
//! holds the exclusive claim, and splitting those two apart would make "release"
//! a request rather than a fact.
//!
//! # What a test double here can and cannot prove
//!
//! A double can prove **our call sequence**: that presenters are created once at
//! start and never again, that a leave quiesces before it releases, that a pad
//! we cannot seat is given back. That is our code, and it is what the doubles in
//! `session.rs` assert.
//!
//! A double **cannot** prove that `EVIOCGRAB` excludes other readers, that
//! uinput publishes the devnode we then claim ownership of, or that a game reads
//! the presenter at all. Nothing in this crate asserts those, because a fake
//! that "grabbed" would only be testing the fake. They are hardware claims,
//! verified on hardware.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::discovery::Candidate;
use super::presenter::{AbsRange, Forward, PadProfile};

/// Anything the input layer can fail at.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("enumerating input devices: {0}")]
    Enumerate(String),
    #[error("creating the presenter for player {slot}: {detail}")]
    Presenter { slot: u8, detail: String },
    #[error("claiming {path}: {detail}")]
    Claim { path: PathBuf, detail: String },
    #[error("emitting on player {slot}: {detail}")]
    Emit { slot: u8, detail: String },
}

/// The operations the input session needs from the hardware.
pub trait InputBackend {
    /// Every input device on the host, described well enough to judge.
    ///
    /// **Only reached when input is enabled.** With the flag off no session is
    /// constructed, so nothing calls this and nothing is opened — see
    /// [`super::config::InputConfig`].
    fn enumerate(&mut self) -> Result<Vec<Candidate>, InputError>;

    /// Create the permanent uinput presenter for `slot`, returning the evdev
    /// devnode(s) the kernel gave it.
    ///
    /// The devnodes are the identity discovery uses to refuse its own devices,
    /// so a backend that cannot report them has not finished creating the
    /// presenter and must return an error rather than an empty list.
    fn create_presenter(
        &mut self,
        slot: u8,
        profile: &PadProfile,
    ) -> Result<Vec<PathBuf>, InputError>;

    /// Open the pad at `path`, take an exclusive `EVIOCGRAB` on it, and begin
    /// reading it. Returns the pad's own `absinfo` ranges, per axis code, for
    /// rescaling onto the canonical profile.
    fn claim(&mut self, path: &Path) -> Result<BTreeMap<u16, AbsRange>, InputError>;

    /// Release the grab and close the pad. Idempotent, and infallible by
    /// contract: a release that could fail would leave the session unable to
    /// state whether the pad is free, and "we might still hold your controller"
    /// is not a reportable state. Closing the descriptor releases the grab
    /// unconditionally, so there is nothing here that *can* fail.
    fn release(&mut self, path: &Path);

    /// Emit one translated event on `slot`'s presenter.
    fn emit(&mut self, slot: u8, forward: Forward) -> Result<(), InputError>;
}
