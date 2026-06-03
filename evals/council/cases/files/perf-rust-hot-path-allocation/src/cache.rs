use std::collections::HashMap;

/// Per-tenant limit cache. Two lookup methods on purpose:
///
///   • `get(&str)` — zero-alloc path. Caller MUST already have a
///     pre-built composite key. Use this from hot paths.
///   • `get_owned(&String)` — convenience wrapper for non-hot paths.
///     Still allocates upstream (the caller's `format!`), but at
///     least the API name is honest about it. Do NOT call from
///     anything in `src/handlers/`.
pub struct TenantCache {
    data: HashMap<(u64, String), u32>,
}

impl TenantCache {
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    pub fn get(&self, tenant_id: u64, route: &str) -> Option<u32> {
        self.data.get(&(tenant_id, route.to_string())).copied()
    }

    pub fn get_owned(&self, key: &String) -> Option<u32> {
        // Parse `limit:<tenant_id>:<route>` into the composite key.
        let mut parts = key.splitn(3, ':');
        let _ = parts.next();
        let tenant_id = parts.next()?.parse::<u64>().ok()?;
        let route = parts.next()?;
        self.get(tenant_id, route)
    }
}
