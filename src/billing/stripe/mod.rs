//! Stripe adapter for the Eye billing catalog (issue #139 foundation).
//!
//! Stripe is an external system. This adapter owns the test/live settings, the pinned HTTP
//! client, and the catalog price verification. It does not provision products or prices, does
//! not create Checkout or portal sessions (#140, #142), does not verify webhooks (#141), and does
//! not report usage (#143). Those depend on the approved catalog and durable storage.

pub mod client;
pub mod config;

pub use client::{
    STRIPE_API_VERSION, StripeClient, StripeError, verify_catalog, verify_price_slot,
};
pub use config::{PriceSlot, StripeConfigError, StripeMode, StripePriceMap, StripeSettings};
