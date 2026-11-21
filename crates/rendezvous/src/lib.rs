//! The secret of a room typed by hand (manual mode, own server), from which
//! come its id on the server and the key that seals voice end to end.
//! NightCoop sessions don't need it: there the host draws a random key.

pub mod secret;
