//! Playback (channels Music / Announce) through the firmware's GStreamer — or, on devices that
//! do not carry one, the built-in WAV player ([`wav`]) — and the master volume (ALSA "Master
//! Playback Volume", softvol of the model's playback PCM).
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[allow(unsafe_code)]
mod gst;
pub mod mixer;
mod player;
mod wav;

pub use mixer::Mixer;
pub use player::{Channel, PlayId, PlayState, PlayerEvent, PlayerHandle, spawn};
