//! Inventory state.
//!
//! ## Lock ordering invariant
//!
//! The repo has TWO shared mutexes that this module's calls may
//! traverse: `inventory_lock` (this file) and `pricing_lock`
//! (`src/pricing/mod.rs`). To avoid AB-BA deadlock with the pricing
//! engine — which legitimately needs to read inventory totals while
//! holding `pricing_lock` for a pricing transaction — the canonical
//! ordering is:
//!
//!   **pricing_lock FIRST, then inventory_lock.**
//!
//! Any code that needs both must acquire them in this order. The
//! pricing module's `with_pricing` helper does the right thing
//! automatically. If you find yourself wanting to acquire
//! inventory_lock first, restructure — don't add a `tokio::time::sleep`.
//!
//! Last incident: 2026-02-14, a fulfillment job deadlocked the order
//! pipeline for 8 minutes during the Valentine's surge. See INC-3041.

use std::sync::Mutex;

use crate::pricing::PricingEngine;

pub struct Inventory {
    inventory_lock: Mutex<InventoryState>,
}

pub struct InventoryState {
    pub on_hand: std::collections::HashMap<String, u32>,
    pub reserved: std::collections::HashMap<String, u32>,
}

impl Inventory {
    pub fn new() -> Self {
        Self {
            inventory_lock: Mutex::new(InventoryState {
                on_hand: Default::default(),
                reserved: Default::default(),
            }),
        }
    }

    /// Reserve `qty` of `sku` and price the reservation against the
    /// current pricing snapshot. Returns the priced reservation.
    pub fn reserve_and_price(
        &self,
        pricing: &PricingEngine,
        sku: &str,
        qty: u32,
    ) -> Result<u64, String> {
        let mut inv = self.inventory_lock.lock().expect("poisoned inventory");
        let on_hand = inv.on_hand.get(sku).copied().unwrap_or(0);
        let reserved = inv.reserved.get(sku).copied().unwrap_or(0);
        if on_hand.saturating_sub(reserved) < qty {
            return Err(format!("insufficient stock for {sku}"));
        }
        let unit_price = pricing.unit_price_cents(sku);
        inv.reserved.insert(sku.to_string(), reserved + qty);
        Ok(unit_price as u64 * qty as u64)
    }
}
