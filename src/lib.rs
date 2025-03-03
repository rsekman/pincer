/// Interact with the Wayland compositor
pub mod clipboard;
/// Manage IPC
pub mod daemon;
/// Error handling
pub mod error;
/// A [`Pincer`](pincer::Pincer) manages several registers.
pub mod pincer;
/// A [`Register`](register::Register) is a map from MIME types to data buffers
pub mod register;
/// Manage data related to a Wayland seat
pub mod seat;
