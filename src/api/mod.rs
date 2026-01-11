//! Admin API and Dashboard module
//!
//! Provides REST API endpoints and an Htmx-powered admin dashboard
//! for managing GSD-COMMS settings.

pub mod routes;
pub mod server;

pub use server::ApiServer;
