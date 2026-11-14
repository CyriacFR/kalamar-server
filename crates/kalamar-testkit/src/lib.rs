//! Headless launchers for the Kalamar community protocol: an API and event client ([`client`]), a simulated game
//! that writes the status and result files the supported game build writes ([`sim`]), and the conformance suite
//! ([`conformance`]) that runs the documented scenarios against any server.

pub mod client;
pub mod conformance;
pub mod sim;
