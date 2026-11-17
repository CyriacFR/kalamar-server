//! Reference implementation of the Kalamar community service (protocol 1.0, `docs/protocol/`).
//!
//! The offering (areas, queues, pools, rooms, ratings, result policies, penalties) comes from the TOML
//! configuration ([`config`]); the HTTP API is in [`api`], the event stream in [`events`], the match engine in
//! [`matches`], matchmaking in [`matchmaking`], the reference rating system in [`rating`].

pub mod activities;
pub mod admin;
pub mod api;
pub mod app;
pub mod auth;
pub mod badges;
pub mod clock;
pub mod config;
pub mod db;
pub mod draft;
pub mod error;
pub mod events;
pub mod host;
pub mod matches;
pub mod matchmaking;
pub mod netutil;
pub mod operator;
pub mod presets;
pub mod ratelimit;
pub mod rating;
pub mod relays;
pub mod reload;
pub mod series;
pub mod spectate;
pub mod state;
pub mod util;
pub mod webhooks;
