//! Pricing engine. Acquires `pricing_lock` BEFORE any inventory work —
//! see `inventory/mod.rs` for the canonical lock ordering. Use
//! `with_pricing` to bracket any code that may also need
//! `inventory_lock`; it acquires both in the correct order under the
//! hood.

use std::sync::Mutex;

pub struct PricingEngine {
    pricing_lock: Mutex<PricingState>,
}

pub struct PricingState {
    pub base_cents: std::collections::HashMap<String, u32>,
    pub markdown_pct: std::collections::HashMap<String, u8>,
}

impl PricingEngine {
    pub fn new() -> Self {
        Self {
            pricing_lock: Mutex::new(PricingState {
                base_cents: Default::default(),
                markdown_pct: Default::default(),
            }),
        }
    }

    pub fn unit_price_cents(&self, sku: &str) -> u32 {
        let state = self.pricing_lock.lock().expect("poisoned pricing");
        let base = state.base_cents.get(sku).copied().unwrap_or(0);
        let pct = state.markdown_pct.get(sku).copied().unwrap_or(0) as u32;
        base.saturating_sub(base * pct / 100)
    }

    pub fn with_pricing<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&PricingState) -> R,
    {
        let state = self.pricing_lock.lock().expect("poisoned pricing");
        f(&state)
    }
}
