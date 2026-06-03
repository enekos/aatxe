use crate::cache::TenantCache;
use crate::types::{Request, Response};

/// Look up the per-route limit override for every tenant in `req`.
///
/// ## Hot path
///
/// This function is called **once per request, in the request thread,
/// on every public API call**. The inner loop runs once per tenant
/// in the request — typically 1–4 — but for the admin-aggregator job
/// it runs up to 30,000 times in a tight loop. Allocation in the
/// inner loop will torch p99 (already measured at 4× regression
/// when a previous PR did `tenant_id.to_string()` per iteration).
///
/// Convention: take `&str` references into the cache, do not allocate
/// scratch strings. The `TenantCache` API has both `get(&str)` and
/// `get_owned(&String)` for exactly this reason.
pub fn route_limits(cache: &TenantCache, req: &Request) -> Vec<Response> {
    let mut out = Vec::with_capacity(req.tenants.len());
    for tenant in &req.tenants {
        let key = format!("limit:{}:{}", tenant.id, req.route);
        let limit = cache.get_owned(&key).unwrap_or(0);
        out.push(Response {
            tenant_id: tenant.id.clone(),
            limit,
        });
    }
    out
}
