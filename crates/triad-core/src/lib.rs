//! triad core library: wire formats, identity and local state. No terminal
//! I/O and no networking; the binary builds on p2panda directly.

pub mod envelope;
pub mod identity;
pub mod net;
pub mod state;
